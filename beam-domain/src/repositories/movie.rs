use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::movie::{CreateMovie, CreateMovieEntry, Movie, MovieEntry};
use crate::providers::enrichment::MovieEnrichment;

/// Persistence for movies and their entries.
///
/// A movie has two names (issue #183). Its **identity key** is what the
/// indexer matches a file to it by: derived from the filename parse, and
/// rewritten only when the rules deriving it change ([`Self::rekey`]). Its
/// **display title** is what a user sees, and enrichment
/// replaces it with the provider's spelling. Looking a movie up by its display
/// title is exactly how a renamed movie used to be missed and duplicated, so
/// the trait offers no such lookup.
///
/// A movie is **live** while at least one of its files is present (not
/// soft-deleted, issue #179). Browse and search -- the
/// [`crate::repositories::CatalogRepository`] -- list only live movies; a read
/// by id still resolves a movie that is not live, so a bookmark or a
/// continue-watching entry does not dangle while its file is away.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait MovieRepository: Send + Sync + std::fmt::Debug {
    /// Live or not.
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>, DbErr>;
    /// Every movie, live or not.
    async fn find_all(&self) -> Result<Vec<Movie>, DbErr>;
    /// The movies among `ids`, live or not, in no particular order; an id
    /// naming no movie is skipped. One statement however many ids, and none
    /// for an empty slice -- this is how a catalogue page reads its movies.
    async fn find_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Movie>, DbErr>;
    /// The movie keyed `create.identity_key`, inserting it only if no movie
    /// carries that key yet. An existing movie is returned **unchanged**:
    /// `create.title`, `year` and `runtime` only populate a new row, so a
    /// later file's parse never overwrites what the first file or enrichment
    /// established. A movie with no key is never matched.
    ///
    /// Atomic: concurrent calls for one key all return the same row.
    async fn find_or_create_by_identity(&self, create: CreateMovie) -> Result<Movie, DbErr>;
    /// Every movie with no identity key -- rows that predate the key, for the
    /// indexer's backfill -- oldest first (`created_at`, then `id`), so of two
    /// legacy duplicates the backfill keys the original.
    async fn find_unkeyed(&self) -> Result<Vec<Movie>, DbErr>;
    /// Give the keyless movie `movie_id` the key `identity_key`, derived by
    /// version `version` of the rules. Returns `false`, changing nothing, when
    /// the movie does not exist, already has a key, or another movie already
    /// holds `identity_key`.
    async fn assign_identity_key(
        &self,
        movie_id: Uuid,
        identity_key: &str,
        version: u16,
    ) -> Result<bool, DbErr>;
    /// The movie keyed `identity_key`, if any.
    async fn find_by_identity_key(&self, identity_key: &str) -> Result<Option<Movie>, DbErr>;
    /// Every keyed movie whose key an older version of the rules than
    /// `version` derived, oldest first (`created_at`, then `id`). A change to
    /// the rules or the title fold can change the key a movie's files derive;
    /// these are the keys the indexer re-derives. A keyless movie is not
    /// listed: [`Self::find_unkeyed`] lists those.
    async fn find_keyed_before_version(&self, version: u16) -> Result<Vec<Movie>, DbErr>;
    /// Replace the key of `movie_id` with `identity_key` -- `None` leaves it
    /// keyless, never matched -- derived by version `version` of the rules.
    /// The movie keeps its id and everything hanging off it. Returns `false`,
    /// changing nothing, when the movie does not exist or another movie holds
    /// `identity_key`.
    async fn rekey(
        &self,
        movie_id: Uuid,
        identity_key: Option<String>,
        version: u16,
    ) -> Result<bool, DbErr>;
    /// Delete every movie entry created before `created_before` that no file
    /// row references, then every movie created before `created_before` left
    /// with no entry, returning how many movies went. A file row that is only
    /// soft-deleted still counts: a title outlives its files' grace period,
    /// never the other way round. Deleting a movie takes its library
    /// associations, enrichment state and genre links with it.
    ///
    /// `created_before` protects a movie or entry the indexer created while
    /// the caller was running, whose file row may not be written yet.
    async fn delete_orphaned(&self, created_before: DateTime<Utc>) -> Result<u64, DbErr>;
    /// The entry for `(library_id, movie_id, edition)`, created if there is
    /// none. Every copy of one edition of a film in a library is a file of one
    /// entry; a second copy never creates a second entry. On a conflict the
    /// stored entry is returned unchanged (`is_primary` included).
    async fn find_or_create_entry(&self, create: CreateMovieEntry) -> Result<MovieEntry, DbErr>;
    async fn find_entries_by_movie_id(&self, movie_id: Uuid) -> Result<Vec<MovieEntry>, DbErr>;
    /// Reverse lookup from a `MediaFileContent::Movie { movie_entry_id }` back
    /// to the entry (and, via `MovieEntry::movie_id`, the movie) -- used to
    /// resolve a file id to its movie for continue-watching.
    async fn find_entry_by_id(&self, entry_id: Uuid) -> Result<Option<MovieEntry>, DbErr>;
    async fn ensure_library_association(
        &self,
        library_id: Uuid,
        movie_id: Uuid,
    ) -> Result<(), DbErr>;
    /// Apply enrichment-provider data to an existing movie (display title,
    /// year, description, external IDs, artwork, rating). Overwrites the
    /// current values -- enrichment is treated as the more authoritative
    /// source once a match is accepted. Never touches the identity key, so the
    /// next file of this movie still finds it however the provider spells the
    /// title.
    async fn apply_enrichment(
        &self,
        movie_id: Uuid,
        enrichment: &MovieEnrichment,
    ) -> Result<(), DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::file::MediaFileContent;
    use crate::repositories::file::in_memory::InMemoryFileRepository;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    /// The in-memory double.
    ///
    /// Whether a movie is live depends on its files, which live in another
    /// repository. [`InMemoryMovieRepository::with_files`] links the double to
    /// the file double the test uses, and then it answers liveness (read by
    /// the catalogue double) and `delete_orphaned` from those files exactly as
    /// the SQL joins do. The unlinked `Default` knows of no files and so treats
    /// every movie as live: the catalogue hides nothing and `delete_orphaned`
    /// removes nothing.
    #[derive(Debug, Default)]
    pub struct InMemoryMovieRepository {
        pub movies: Mutex<HashMap<Uuid, Movie>>,
        pub entries: Mutex<HashMap<Uuid, MovieEntry>>,
        /// The rules version behind each movie's key. A movie absent here --
        /// one a test inserted into `movies` directly -- is version `0`, as a
        /// row keyed before versions existed is.
        pub key_versions: Mutex<HashMap<Uuid, u16>>,
        files: Option<Arc<InMemoryFileRepository>>,
    }

    impl InMemoryMovieRepository {
        /// A double whose liveness and orphan checks read `files`.
        pub fn with_files(files: Arc<InMemoryFileRepository>) -> Self {
            Self {
                files: Some(files),
                ..Self::default()
            }
        }

        /// Entry ids some file row references -- only present files when
        /// `present_only`. `None` when unlinked.
        fn referenced_entries(&self, present_only: bool) -> Option<HashSet<Uuid>> {
            let files = self.files.as_ref()?;
            Some(
                files
                    .files
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|f| !present_only || f.missing_since.is_none())
                    .filter_map(|f| match &f.content {
                        Some(MediaFileContent::Movie { movie_entry_id }) => Some(*movie_entry_id),
                        _ => None,
                    })
                    .collect(),
            )
        }

        /// Movie ids with a present file, or `None` when unlinked.
        pub fn live_movie_ids(&self) -> Option<HashSet<Uuid>> {
            let live_entries = self.referenced_entries(true)?;
            let entries = self.entries.lock().unwrap();
            Some(
                live_entries
                    .iter()
                    .filter_map(|id| entries.get(id).map(|e| e.movie_id))
                    .collect(),
            )
        }
    }

    #[async_trait]
    impl MovieRepository for InMemoryMovieRepository {
        async fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>, DbErr> {
            Ok(self.movies.lock().unwrap().get(&id).cloned())
        }

        async fn find_all(&self) -> Result<Vec<Movie>, DbErr> {
            Ok(self.movies.lock().unwrap().values().cloned().collect())
        }

        async fn find_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Movie>, DbErr> {
            let movies = self.movies.lock().unwrap();
            Ok(ids
                .iter()
                .filter_map(|id| movies.get(id).cloned())
                .collect())
        }

        async fn find_or_create_by_identity(&self, create: CreateMovie) -> Result<Movie, DbErr> {
            let CreateMovie {
                identity_key,
                identity_key_version,
                title,
                year,
                runtime,
            } = create;
            // Lookup and insert under one lock, so the double is as atomic as
            // the `ON CONFLICT` statement it stands in for.
            let mut movies = self.movies.lock().unwrap();
            if let Some(existing) = movies
                .values()
                .find(|m| m.identity_key.as_deref() == Some(identity_key.as_str()))
            {
                return Ok(existing.clone());
            }
            let movie = Movie {
                id: Uuid::new_v4(),
                title,
                identity_key: Some(identity_key),
                title_localized: None,
                description: None,
                year,
                release_date: None,
                runtime,
                poster_url: None,
                backdrop_url: None,
                tmdb_id: None,
                imdb_id: None,
                tvdb_id: None,
                anilist_id: None,
                rating_tmdb: None,
                rating_imdb: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            movies.insert(movie.id, movie.clone());
            self.key_versions
                .lock()
                .unwrap()
                .insert(movie.id, identity_key_version);
            Ok(movie)
        }

        async fn find_unkeyed(&self) -> Result<Vec<Movie>, DbErr> {
            let mut unkeyed: Vec<_> = self
                .movies
                .lock()
                .unwrap()
                .values()
                .filter(|m| m.identity_key.is_none())
                .cloned()
                .collect();
            unkeyed.sort_by_key(|m| (m.created_at, m.id));
            Ok(unkeyed)
        }

        async fn assign_identity_key(
            &self,
            movie_id: Uuid,
            identity_key: &str,
            version: u16,
        ) -> Result<bool, DbErr> {
            let mut movies = self.movies.lock().unwrap();
            if movies
                .values()
                .any(|m| m.identity_key.as_deref() == Some(identity_key))
            {
                return Ok(false);
            }
            match movies.get_mut(&movie_id) {
                Some(movie) if movie.identity_key.is_none() => {
                    movie.identity_key = Some(identity_key.to_string());
                    self.key_versions.lock().unwrap().insert(movie_id, version);
                    Ok(true)
                }
                _ => Ok(false),
            }
        }

        async fn find_by_identity_key(&self, identity_key: &str) -> Result<Option<Movie>, DbErr> {
            Ok(self
                .movies
                .lock()
                .unwrap()
                .values()
                .find(|m| m.identity_key.as_deref() == Some(identity_key))
                .cloned())
        }

        async fn find_keyed_before_version(&self, version: u16) -> Result<Vec<Movie>, DbErr> {
            // `movies` before `key_versions`, the order every method takes
            // them in, so no two calls can deadlock.
            let movies = self.movies.lock().unwrap();
            let versions = self.key_versions.lock().unwrap();
            let mut stale: Vec<_> = movies
                .values()
                .filter(|m| m.identity_key.is_some())
                .filter(|m| versions.get(&m.id).copied().unwrap_or(0) < version)
                .cloned()
                .collect();
            stale.sort_by_key(|m| (m.created_at, m.id));
            Ok(stale)
        }

        async fn rekey(
            &self,
            movie_id: Uuid,
            identity_key: Option<String>,
            version: u16,
        ) -> Result<bool, DbErr> {
            let mut movies = self.movies.lock().unwrap();
            if let Some(key) = identity_key.as_deref()
                && movies
                    .values()
                    .any(|m| m.id != movie_id && m.identity_key.as_deref() == Some(key))
            {
                return Ok(false);
            }
            let Some(movie) = movies.get_mut(&movie_id) else {
                return Ok(false);
            };
            movie.identity_key = identity_key;
            self.key_versions.lock().unwrap().insert(movie_id, version);
            Ok(true)
        }

        async fn delete_orphaned(&self, created_before: DateTime<Utc>) -> Result<u64, DbErr> {
            let Some(referenced) = self.referenced_entries(false) else {
                return Ok(0);
            };
            let mut entries = self.entries.lock().unwrap();
            entries.retain(|id, e| e.created_at >= created_before || referenced.contains(id));
            let mut movies = self.movies.lock().unwrap();
            let before = movies.len();
            movies.retain(|id, m| {
                m.created_at >= created_before || entries.values().any(|e| e.movie_id == *id)
            });
            Ok((before - movies.len()) as u64)
        }

        async fn find_or_create_entry(
            &self,
            create: CreateMovieEntry,
        ) -> Result<MovieEntry, DbErr> {
            let CreateMovieEntry {
                library_id,
                movie_id,
                edition,
                is_primary,
            } = create;
            // Lookup and insert under one lock, as atomic as `ON CONFLICT`.
            let mut entries = self.entries.lock().unwrap();
            if let Some(existing) = entries.values().find(|e| {
                e.library_id == library_id && e.movie_id == movie_id && e.edition == edition
            }) {
                return Ok(existing.clone());
            }
            let entry = MovieEntry {
                id: Uuid::new_v4(),
                library_id,
                movie_id,
                edition,
                is_primary,
                created_at: chrono::Utc::now(),
            };
            entries.insert(entry.id, entry.clone());
            Ok(entry)
        }

        async fn find_entries_by_movie_id(&self, movie_id: Uuid) -> Result<Vec<MovieEntry>, DbErr> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .values()
                .filter(|e| e.movie_id == movie_id)
                .cloned()
                .collect())
        }

        async fn find_entry_by_id(&self, entry_id: Uuid) -> Result<Option<MovieEntry>, DbErr> {
            Ok(self.entries.lock().unwrap().get(&entry_id).cloned())
        }

        async fn ensure_library_association(
            &self,
            _library_id: Uuid,
            _movie_id: Uuid,
        ) -> Result<(), DbErr> {
            Ok(())
        }

        async fn apply_enrichment(
            &self,
            movie_id: Uuid,
            enrichment: &MovieEnrichment,
        ) -> Result<(), DbErr> {
            let mut movies = self.movies.lock().unwrap();
            if let Some(movie) = movies.get_mut(&movie_id) {
                movie.title = enrichment.title.clone();
                movie.title_localized = enrichment.original_title.clone();
                movie.description = enrichment.description.clone();
                movie.year = enrichment.year;
                movie.release_date = enrichment.release_date;
                movie.poster_url = enrichment.poster_url.clone();
                movie.backdrop_url = enrichment.backdrop_url.clone();
                movie.tmdb_id = enrichment.tmdb_id;
                movie.imdb_id = enrichment.imdb_id.clone();
                movie.anilist_id = enrichment.anilist_id;
                movie.runtime = enrichment
                    .runtime_mins
                    .map(|mins| std::time::Duration::from_secs(u64::from(mins) * 60));
                movie.rating_tmdb = enrichment.rating;
                movie.updated_at = chrono::Utc::now();
            }
            Ok(())
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::MovieRepository;
    use super::in_memory::InMemoryMovieRepository;
    use crate::models::movie::Movie;
    use crate::repositories::FileRepository;
    use crate::repositories::contract::fixture::MovieRepositoryFixture;
    use crate::repositories::file::in_memory::InMemoryFileRepository;

    /// The hermetic instantiation of the shared contract: a movie double
    /// linked to the file double the contract writes files through.
    #[derive(Debug)]
    pub struct InMemoryFixture {
        repo: InMemoryMovieRepository,
        files: Arc<InMemoryFileRepository>,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            let files = Arc::new(InMemoryFileRepository::default());
            Self {
                repo: InMemoryMovieRepository::with_files(files.clone()),
                files,
            }
        }
    }

    #[async_trait::async_trait]
    impl MovieRepositoryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn MovieRepository {
            &self.repo
        }

        fn files(&self) -> &dyn FileRepository {
            self.files.as_ref()
        }

        async fn new_library(&self) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_unkeyed_movie(
            &self,
            title: &str,
            created_at: chrono::DateTime<chrono::Utc>,
        ) -> Uuid {
            let now = created_at;
            let movie = Movie {
                id: Uuid::new_v4(),
                title: title.to_string(),
                identity_key: None,
                title_localized: None,
                description: None,
                year: None,
                release_date: None,
                runtime: None,
                poster_url: None,
                backdrop_url: None,
                tmdb_id: None,
                imdb_id: None,
                tvdb_id: None,
                anilist_id: None,
                rating_tmdb: None,
                rating_imdb: None,
                created_at: now,
                updated_at: now,
            };
            let id = movie.id;
            self.repo.movies.lock().unwrap().insert(id, movie);
            id
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::movie_repository_contract!(setup);
}
