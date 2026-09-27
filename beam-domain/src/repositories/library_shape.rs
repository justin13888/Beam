use async_trait::async_trait;
use sea_orm::DbErr;

use crate::models::library_shape::LibraryShape;

/// The one read the anonymous library report makes (issue #93).
///
/// A read-only aggregate across every other repository's rows, kept behind a
/// trait of its own rather than assembled from `find_all` calls: the SQL
/// implementation answers with a handful of `COUNT`/`GROUP BY` statements, and
/// loading every file row into memory to count them is exactly what a server
/// with a large library should never do on a timer.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait LibraryShapeRepository: Send + Sync + std::fmt::Debug {
    /// The shape of every library right now. See [`LibraryShape`] for what
    /// counts.
    async fn shape(&self) -> Result<LibraryShape, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Arc;

    use uuid::Uuid;

    use super::*;
    use crate::models::file::MediaFileContent;
    use crate::models::library_shape::{FilesByContentType, NamedCount};
    use crate::models::stream::StreamType;
    use crate::repositories::file::in_memory::InMemoryFileRepository;
    use crate::repositories::library::in_memory::InMemoryLibraryRepository;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use crate::repositories::stream::in_memory::InMemoryMediaStreamRepository;
    use crate::utils::telemetry::{FileSizeHistogram, UNKNOWN_LABEL};

    /// The in-memory double: a view over linked library, movie, show, file
    /// and stream doubles, answering from their rows exactly as the SQL
    /// aggregates answer from the tables. Seed it through the linked doubles'
    /// own trait methods.
    #[derive(Debug, Clone)]
    pub struct InMemoryLibraryShapeRepository {
        pub libraries: Arc<InMemoryLibraryRepository>,
        pub movies: Arc<InMemoryMovieRepository>,
        pub shows: Arc<InMemoryShowRepository>,
        pub files: Arc<InMemoryFileRepository>,
        pub streams: Arc<InMemoryMediaStreamRepository>,
    }

    impl Default for InMemoryLibraryShapeRepository {
        fn default() -> Self {
            let files = Arc::new(InMemoryFileRepository::default());
            Self {
                libraries: Arc::new(InMemoryLibraryRepository::default()),
                movies: Arc::new(InMemoryMovieRepository::with_files(files.clone())),
                shows: Arc::new(InMemoryShowRepository::with_files(files.clone())),
                files,
                streams: Arc::new(InMemoryMediaStreamRepository::default()),
            }
        }
    }

    fn sorted(counts: BTreeMap<String, u64>) -> Vec<NamedCount> {
        counts
            .into_iter()
            .map(|(name, count)| NamedCount { name, count })
            .collect()
    }

    #[async_trait]
    impl LibraryShapeRepository for InMemoryLibraryShapeRepository {
        async fn shape(&self) -> Result<LibraryShape, DbErr> {
            let present: Vec<_> = self
                .files
                .files
                .lock()
                .unwrap()
                .values()
                .filter(|f| f.missing_since.is_none())
                .cloned()
                .collect();

            let mut files = FilesByContentType::default();
            let mut live_entries = HashSet::new();
            let mut live_episodes = HashSet::new();
            let mut containers = BTreeMap::new();
            let mut file_sizes = FileSizeHistogram::default();
            let mut total_bytes = 0u64;
            for file in &present {
                match &file.content {
                    Some(MediaFileContent::Movie { movie_entry_id }) => {
                        files.movie += 1;
                        live_entries.insert(*movie_entry_id);
                    }
                    Some(MediaFileContent::Episode {
                        episode_id,
                        last_episode_number: _,
                    }) => {
                        files.episode += 1;
                        live_episodes.insert(*episode_id);
                    }
                    None => files.unclassified += 1,
                }
                let container = file
                    .container_format
                    .clone()
                    .unwrap_or_else(|| UNKNOWN_LABEL.to_string());
                *containers.entry(container).or_insert(0) += 1;
                file_sizes.record(file.size_bytes);
                total_bytes += file.size_bytes;
            }

            let movies: HashSet<Uuid> = {
                let entries = self.movies.entries.lock().unwrap();
                live_entries
                    .iter()
                    .filter_map(|id| entries.get(id).map(|e| e.movie_id))
                    .collect()
            };
            let seasons: HashSet<Uuid> = {
                let episodes = self.shows.episodes.lock().unwrap();
                live_episodes
                    .iter()
                    .filter_map(|id| episodes.get(id).map(|e| e.season_id))
                    .collect()
            };
            let shows: HashSet<Uuid> = {
                let all = self.shows.seasons.lock().unwrap();
                seasons
                    .iter()
                    .filter_map(|id| all.get(id).map(|s| s.show_id))
                    .collect()
            };

            let present_ids: HashSet<Uuid> = present.iter().map(|f| f.id).collect();
            let mut video = BTreeMap::new();
            let mut audio = BTreeMap::new();
            let mut subtitle = BTreeMap::new();
            for (file_id, streams) in self.streams.streams.lock().unwrap().iter() {
                if !present_ids.contains(file_id) {
                    continue;
                }
                for stream in streams {
                    let into = match stream.stream_type {
                        StreamType::Video => &mut video,
                        StreamType::Audio => &mut audio,
                        StreamType::Subtitle => &mut subtitle,
                    };
                    *into.entry(stream.codec.clone()).or_insert(0) += 1;
                }
            }

            Ok(LibraryShape {
                libraries: self.libraries.libraries.lock().unwrap().len() as u64,
                movies: movies.len() as u64,
                shows: shows.len() as u64,
                seasons: seasons.len() as u64,
                episodes: live_episodes
                    .iter()
                    .filter(|id| self.shows.episodes.lock().unwrap().contains_key(id))
                    .count() as u64,
                files,
                containers: sorted(containers),
                video_codecs: sorted(video),
                audio_codecs: sorted(audio),
                subtitle_codecs: sorted(subtitle),
                file_sizes,
                total_bytes,
            })
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use super::LibraryShapeRepository;
    use super::in_memory::InMemoryLibraryShapeRepository;
    use crate::repositories::contract::fixture::LibraryShapeFixture;
    use crate::repositories::{
        FileRepository, LibraryRepository, MediaStreamRepository, MovieRepository, ShowRepository,
    };

    /// The hermetic instantiation of the shared contract.
    #[derive(Debug, Default)]
    pub struct InMemoryFixture {
        repo: InMemoryLibraryShapeRepository,
    }

    impl LibraryShapeFixture for InMemoryFixture {
        fn repo(&self) -> &dyn LibraryShapeRepository {
            &self.repo
        }

        fn libraries(&self) -> &dyn LibraryRepository {
            self.repo.libraries.as_ref()
        }

        fn movies(&self) -> &dyn MovieRepository {
            self.repo.movies.as_ref()
        }

        fn shows(&self) -> &dyn ShowRepository {
            self.repo.shows.as_ref()
        }

        fn files(&self) -> &dyn FileRepository {
            self.repo.files.as_ref()
        }

        fn streams(&self) -> &dyn MediaStreamRepository {
            self.repo.streams.as_ref()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::library_shape_repository_contract!(setup);
}
