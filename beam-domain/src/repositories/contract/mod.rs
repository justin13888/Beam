//! Shared behavioural contracts for the repository traits.
//!
//! Each macro here expands to a suite of `#[tokio::test]`s written purely
//! against a trait. The same suite is instantiated over the in-memory double
//! (hermetic, always run) and -- under the opt-in `pg-integration` feature in
//! `beam-index` -- over the SeaORM implementation against a real Postgres.
//!
//! This is what makes the doubles legitimate. A test that drives an
//! `InMemory*` repository and asserts on its own `HashMap` proves nothing about
//! production; the *same* assertions, run over both implementations, constrain
//! both at once and turn any fake/Postgres divergence into a failure rather
//! than silent drift. It is the one exception AGENTS.md grants to "never test
//! the double".
//!
//! Ordering is asserted by advancing an injected
//! [`crate::services::TestClock`], never by sleeping: every implementation
//! under contract takes its `updated_at` from the injected [`crate::services::Clock`].
//!
//! One file per aggregate holds its contract macro. Each is
//! `#[macro_export]`ed, so every one is invoked from the crate root
//! (`$crate::chapter_repository_contract!`) whichever file defines it.

mod access_policy;
mod admin_log;
mod applied_nfo;
mod catalog;
mod chapter;
mod collection;
mod enrichment_state;
mod file;
mod genre;
mod language_preference;
mod library;
mod library_shape;
mod media_stream;
mod movie;
mod person;
mod playback_telemetry;
mod show;
mod sidecar_subtitle;
mod trickplay;
mod user_list;
mod watch_state;

/// Fixtures the shared contracts are written against. Gated behind
/// `test-utils`: only test code builds one.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod fixture {
    use uuid::Uuid;

    use crate::models::watch_state::WatchTarget;
    use crate::repositories::WatchStateRepository;
    use crate::services::TestClock;

    /// Everything the [`crate::watch_state_repository_contract`] suite needs
    /// from a backing store.
    ///
    /// Identifiers are allocated by the fixture rather than invented by the
    /// contract because a real Postgres enforces the user, movie, episode,
    /// show and file foreign keys: the in-memory fixture can hand back a bare
    /// [`Uuid::new_v4`], while the Postgres fixture must insert the referenced
    /// rows first. The contract itself stays identical across both.
    #[async_trait::async_trait]
    pub trait WatchStateFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn WatchStateRepository;

        /// The clock the repository stamps `last_played_at` from.
        fn clock(&self) -> &TestClock;

        /// A user that exists as far as the backing store is concerned.
        async fn new_user(&self) -> Uuid;

        /// A movie, as the target a row of it is keyed by.
        async fn new_movie(&self) -> WatchTarget;

        /// A show with no episodes yet.
        async fn new_show(&self) -> Uuid;

        /// A further episode of `show_id`.
        async fn new_episode(&self, show_id: Uuid) -> WatchTarget;

        /// A media file a report can name.
        async fn new_file(&self) -> Uuid;
    }

    /// Everything the [`crate::file_repository_contract`] suite needs from a
    /// backing store.
    ///
    /// The parents a file row hangs off are allocated by the fixture for the
    /// same reason as in [`WatchStateFixture`]: Postgres enforces the
    /// `library_id`, `movie_entry_id` and `episode_id` foreign keys, the
    /// in-memory store does not.
    #[async_trait::async_trait]
    pub trait FileRepositoryFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::FileRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;

        /// A movie entry inside `library_id`.
        async fn new_movie_entry(&self, library_id: Uuid) -> Uuid;

        /// An episode (with the show and season it needs) for `library_id`.
        async fn new_episode(&self, library_id: Uuid) -> Uuid;
    }

    /// Everything the [`crate::show_repository_contract`] suite needs from a
    /// backing store. Every show, season and episode the contract needs is
    /// created through the trait itself, so a real Postgres sees the same
    /// foreign-key chain the indexer builds.
    ///
    /// Whether a show is live, or orphaned, is a question about its files, so
    /// the fixture also hands out the file repository over the same store --
    /// linked to `repo` so the one sees the other's rows -- and a library for
    /// those files to belong to.
    ///
    /// `delete_orphaned` is global: over a database other tests share it
    /// would delete their fileless shows mid-test. A Postgres fixture must
    /// therefore give each test a store of its own.
    #[async_trait::async_trait]
    pub trait ShowRepositoryFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::ShowRepository;

        /// The file repository over the same store.
        fn files(&self) -> &dyn crate::repositories::FileRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;

        /// A show titled `title`, created at `created_at`, with no identity
        /// key -- a row from before keys existed, which the trait itself can
        /// no longer create.
        async fn new_unkeyed_show(
            &self,
            title: &str,
            created_at: ::chrono::DateTime<::chrono::Utc>,
        ) -> Uuid;
    }

    /// Everything the [`crate::movie_repository_contract`] suite needs from a
    /// backing store; the movie counterpart of [`ShowRepositoryFixture`], with
    /// the same requirement that each Postgres test own its store.
    #[async_trait::async_trait]
    pub trait MovieRepositoryFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::MovieRepository;

        /// The file repository over the same store.
        fn files(&self) -> &dyn crate::repositories::FileRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;

        /// A movie titled `title`, created at `created_at`, with no identity
        /// key.
        async fn new_unkeyed_movie(
            &self,
            title: &str,
            created_at: ::chrono::DateTime<::chrono::Utc>,
        ) -> Uuid;
    }

    /// Everything the [`crate::library_shape_repository_contract`] suite needs
    /// from a backing store: the aggregate under contract, and the repositories
    /// the contract seeds rows through -- all over one store, so what one
    /// writes the aggregate sees.
    ///
    /// The shape is global, so as for shows and movies a Postgres fixture must
    /// give each test a store of its own.
    pub trait LibraryShapeFixture: Send + Sync {
        /// The aggregate under contract.
        fn repo(&self) -> &dyn crate::repositories::LibraryShapeRepository;
        fn libraries(&self) -> &dyn crate::repositories::LibraryRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;
        fn files(&self) -> &dyn crate::repositories::FileRepository;
        fn streams(&self) -> &dyn crate::repositories::MediaStreamRepository;
    }

    /// Everything the [`crate::catalog_repository_contract`] suite needs from a
    /// backing store: the read model under contract, and the repositories the
    /// contract builds its titles through -- all over one store, so what one
    /// writes the catalogue lists.
    ///
    /// The catalogue is global, so as for shows and movies a Postgres fixture
    /// must give each test a store of its own.
    #[async_trait::async_trait]
    pub trait CatalogRepositoryFixture: Send + Sync {
        /// The read model under contract.
        fn repo(&self) -> &dyn crate::repositories::CatalogRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;
        fn genres(&self) -> &dyn crate::repositories::GenreRepository;
        fn files(&self) -> &dyn crate::repositories::FileRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::genre_repository_contract`] suite needs: the
    /// repository under contract and the title repositories over the same
    /// store, since a real Postgres enforces the junction tables' foreign keys.
    #[async_trait::async_trait]
    pub trait GenreRepositoryFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::GenreRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::person_repository_contract`] suite needs: the
    /// repository under contract and the title repositories over the same
    /// store, since a real Postgres holds a credit to its title by a foreign
    /// key. Search and count are global, so a Postgres fixture must give each
    /// test a store of its own.
    #[async_trait::async_trait]
    pub trait PersonRepositoryFixture: Send + Sync {
        /// The repository under contract, empty.
        fn repo(&self) -> &dyn crate::repositories::PersonRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::user_list_repository_contract`] suite needs
    /// from a backing store. Identifiers are allocated by the fixture, as for
    /// [`WatchStateFixture`], because a real Postgres enforces the user,
    /// movie and show foreign keys.
    #[async_trait::async_trait]
    pub trait UserListFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::UserListRepository;

        /// The clock the repository stamps `added_at` from.
        fn clock(&self) -> &TestClock;

        /// The title repositories over the same store, which an access
        /// policy is applied through.
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;

        /// A user that exists as far as the backing store is concerned.
        async fn new_user(&self) -> Uuid;

        /// A movie's id.
        async fn new_movie(&self) -> Uuid;

        /// A show's id.
        async fn new_show(&self) -> Uuid;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::collection_repository_contract`] suite needs:
    /// the repository under contract and the movie repository over the same
    /// store, since a real Postgres holds a membership to its movie by a
    /// foreign key. Listing and counting are global, so a Postgres fixture
    /// must give each test a store of its own.
    #[async_trait::async_trait]
    pub trait CollectionRepositoryFixture: Send + Sync {
        /// The repository under contract, empty.
        fn repo(&self) -> &dyn crate::repositories::CollectionRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::playback_telemetry_repository_contract`] suite
    /// needs from a backing store. The counters reference nothing, so the
    /// repository is all there is -- but `summarize` and `prune_before` are
    /// global, so a Postgres fixture must give each test a store of its own.
    pub trait PlaybackTelemetryFixture: Send + Sync {
        /// The repository under contract, empty.
        fn repo(&self) -> &dyn crate::repositories::PlaybackTelemetryRepository;
    }

    /// Everything the [`crate::applied_nfo_repository_contract`] suite needs
    /// from a backing store: the libraries a record hangs off, which Postgres
    /// holds to a foreign key.
    #[async_trait::async_trait]
    pub trait AppliedNfoFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::AppliedNfoRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::sidecar_subtitle_repository_contract`] suite
    /// needs from a backing store: the libraries and video files a subtitle
    /// row hangs off, which Postgres holds to foreign keys.
    #[async_trait::async_trait]
    pub trait SidecarSubtitleFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::SidecarSubtitleRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;

        /// A video file in `library_id`.
        async fn new_video_file(&self, library_id: Uuid) -> Uuid;
    }

    /// Everything the [`crate::enrichment_state_repository_contract`] suite
    /// needs: the repository under contract and the title repositories over
    /// the same store, since a real Postgres holds a row to its title by a
    /// foreign key. Listing, counting and refreshing everything are global,
    /// so a Postgres fixture must give each test a store of its own.
    #[async_trait::async_trait]
    pub trait EnrichmentStateFixture: Send + Sync {
        /// The repository under contract, empty.
        fn repo(&self) -> &dyn crate::repositories::EnrichmentStateRepository;
        fn movies(&self) -> &dyn crate::repositories::MovieRepository;
        fn shows(&self) -> &dyn crate::repositories::ShowRepository;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::access_policy_repository_contract`] suite
    /// needs: the repository under contract and the users and libraries a
    /// policy names, which a real Postgres holds to foreign keys.
    #[async_trait::async_trait]
    pub trait AccessPolicyFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::AccessPolicyRepository;

        /// A user that exists as far as the backing store is concerned.
        async fn new_user(&self) -> Uuid;

        /// A library that exists as far as the backing store is concerned.
        async fn new_library(&self) -> Uuid;
    }

    /// Everything the [`crate::language_preference_repository_contract`]
    /// suite needs: the repository under contract and the users it stores
    /// preferences for, which a real Postgres holds to a foreign key.
    #[async_trait::async_trait]
    pub trait LanguagePreferenceFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::LanguagePreferenceRepository;

        /// A user that exists as far as the backing store is concerned.
        async fn new_user(&self) -> Uuid;
    }

    /// Everything the [`crate::chapter_repository_contract`] suite needs:
    /// the repository under contract and the files a chapter belongs to,
    /// which a real Postgres holds to a foreign key.
    #[async_trait::async_trait]
    pub trait ChapterFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::ChapterRepository;

        /// A video file that exists as far as the backing store is concerned.
        async fn new_file(&self) -> Uuid;
    }

    /// Everything the [`crate::trickplay_repository_contract`] suite needs:
    /// the repository under contract and the files a record belongs to,
    /// which a real Postgres holds to a foreign key.
    #[async_trait::async_trait]
    pub trait TrickplayFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::TrickplayRepository;

        /// A video file that exists as far as the backing store is concerned.
        async fn new_file(&self) -> Uuid;
    }

    /// Everything the [`crate::admin_log_repository_contract`] suite needs:
    /// a log of its own -- listing and counting are global -- and the clock
    /// it stamps entries from, so entries are ordered without sleeping.
    pub trait AdminLogFixture: Send + Sync {
        /// The repository under contract, empty when the fixture is made.
        fn repo(&self) -> &dyn crate::repositories::AdminLogRepository;

        /// The clock the repository stamps `created_at` from.
        fn clock(&self) -> &TestClock;
    }

    /// Everything the [`crate::library_repository_contract`] suite needs: a
    /// store of its own -- listing and counting are global -- and the file
    /// store whose rows a library's file count is read from.
    pub trait LibraryRepositoryFixture: Send + Sync {
        /// The repository under contract, empty when the fixture is made.
        fn repo(&self) -> &dyn crate::repositories::LibraryRepository;

        /// The file store sharing the repository's backing store.
        fn files(&self) -> &dyn crate::repositories::FileRepository;
    }

    /// Everything the [`crate::media_stream_repository_contract`] suite
    /// needs: the repository under contract and the files a stream belongs
    /// to, which a real Postgres holds to a foreign key.
    #[async_trait::async_trait]
    pub trait MediaStreamFixture: Send + Sync {
        /// The repository under contract.
        fn repo(&self) -> &dyn crate::repositories::MediaStreamRepository;

        /// A media file that exists as far as the backing store is concerned.
        async fn new_file(&self) -> Uuid;
    }

    /// The titles the list, collection, people and genre contracts apply an
    /// access policy to (issue #328): in the library `kids` a `PG` movie
    /// and a `TV-PG` show, in `grown` an `R` movie and a `TV-MA` show.
    ///
    /// Every restricted policy here names its libraries, so what it admits
    /// is the scenario's own titles alone, even over a store other tests
    /// write to at the same time.
    #[derive(Debug, Clone, Copy)]
    pub struct AccessScenario {
        pub kids: Uuid,
        pub grown: Uuid,
        pub kids_movie: Uuid,
        pub kids_show: Uuid,
        pub grown_movie: Uuid,
        pub grown_show: Uuid,
    }

    impl AccessScenario {
        /// The four titles, created through `movies` and `shows` in the
        /// libraries `kids` and `grown`, which must exist.
        pub async fn build(
            movies: &dyn crate::repositories::MovieRepository,
            shows: &dyn crate::repositories::ShowRepository,
            kids: Uuid,
            grown: Uuid,
        ) -> Self {
            Self {
                kids,
                grown,
                kids_movie: certified_movie(movies, kids, "PG").await,
                kids_show: certified_show(shows, kids, "TV-PG").await,
                grown_movie: certified_movie(movies, grown, "R").await,
                grown_show: certified_show(shows, grown, "TV-MA").await,
            }
        }

        /// The `kids` library alone: its titles, whatever their rating.
        pub fn kids_only(&self) -> crate::models::access::AccessPolicy {
            crate::models::access::AccessPolicy {
                libraries: crate::models::access::LibraryAccess::Only([self.kids].into()),
                max_rating: None,
            }
        }

        /// Both libraries under a parental-guidance ceiling: the `kids`
        /// titles again, this time for their rating.
        pub fn both_under_parental_guidance(&self) -> crate::models::access::AccessPolicy {
            crate::models::access::AccessPolicy {
                libraries: crate::models::access::LibraryAccess::Only(
                    [self.kids, self.grown].into(),
                ),
                max_rating: Some(crate::models::access::ContentRating::ParentalGuidance),
            }
        }

        /// The `grown` library under a teen ceiling, which nothing in it is
        /// within: no title at all.
        pub fn grown_under_teen(&self) -> crate::models::access::AccessPolicy {
            crate::models::access::AccessPolicy {
                libraries: crate::models::access::LibraryAccess::Only([self.grown].into()),
                max_rating: Some(crate::models::access::ContentRating::Teen),
            }
        }
    }

    async fn certified_movie(
        movies: &dyn crate::repositories::MovieRepository,
        library_id: Uuid,
        certification: &str,
    ) -> Uuid {
        let title = format!("{certification} movie {}", Uuid::new_v4());
        let movie = movies
            .find_or_create_by_identity(crate::models::movie::CreateMovie::new(
                title.clone(),
                None,
                None,
            ))
            .await
            .expect("create a movie");
        movies
            .apply_enrichment(
                movie.id,
                &crate::providers::enrichment::MovieEnrichment {
                    title,
                    content_rating: Some(certification.to_string()),
                    ..Default::default()
                },
                &crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .expect("certify the movie");
        movies
            .ensure_library_association(library_id, movie.id)
            .await
            .expect("associate the movie");
        movie.id
    }

    async fn certified_show(
        shows: &dyn crate::repositories::ShowRepository,
        library_id: Uuid,
        certification: &str,
    ) -> Uuid {
        let title = format!("{certification} show {}", Uuid::new_v4());
        let show = shows
            .find_or_create_by_identity(crate::models::show::CreateShow::new(title.clone(), None))
            .await
            .expect("create a show");
        shows
            .apply_enrichment(
                show.id,
                &crate::providers::enrichment::ShowEnrichment {
                    title,
                    content_rating: Some(certification.to_string()),
                    ..Default::default()
                },
                &crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .expect("certify the show");
        shows
            .ensure_library_association(library_id, show.id)
            .await
            .expect("associate the show");
        show.id
    }
}
