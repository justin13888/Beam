//! NFR-205: what a sweep does when a store or a provider fails under it.
//!
//! Each failure is produced through the injected trait: a store here wraps
//! the in-memory one, answers every call through it, and fails the one call
//! the test is about. The test then reads the state back through the inner
//! store, so it asserts what the failure left behind, not what the wrapper
//! was told to say.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use uuid::Uuid;

use beam_domain::models::catalog::ShowChildCounts;
use beam_domain::models::enrichment::{
    EnrichmentListFilter, EnrichmentListQuery, EnrichmentState, EnrichmentStatus,
    EnrichmentStatusCounts, EnrichmentTargetId, FieldLocks,
};
use beam_domain::models::{
    CreateEpisode, CreateMovie, CreateMovieEntry, CreateShow, Episode, Movie, MovieEntry,
    PinSource, ProviderPin, Season, Show,
};
use beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider;
use beam_domain::providers::enrichment::{
    EnrichmentError, EnrichmentProvider, ExternalMediaRef, MediaQuery, MovieEnrichment,
    MovieSearchHit, SeasonEnrichment, ShowEnrichment, ShowSearchHit,
};
use beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository;
use beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;
use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::person::in_memory::InMemoryPersonRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::{EnrichmentStateRepository, MovieRepository, ShowRepository};
use beam_domain::services::TestClock;
use beam_domain::utils::next_up::OutlineEpisode;

use super::{MetadataEnrichmentService, SweepReport};
use crate::services::admin_log::NoOpAdminLogService;
use crate::services::notification::InMemoryNotificationService;

fn database_down() -> DbErr {
    DbErr::Custom("the database is down".to_string())
}

/// `#[async_trait] impl $trait for $wrapper`, answering each call listed
/// under `delegate` through the wrapped store (`self.0`), and every other
/// through the items that follow it.
macro_rules! wrap_store {
    (
        impl $trait:ident for $wrapper:ty {
            delegate {
                $( fn $name:ident(&self $(, $arg:ident : $arg_ty:ty)* ) -> $ret:ty; )*
            }
            $($own:tt)*
        }
    ) => {
        #[async_trait::async_trait]
        impl $trait for $wrapper {
            $(
                async fn $name(&self $(, $arg: $arg_ty)*) -> $ret {
                    self.0.$name($($arg),*).await
                }
            )*
            $($own)*
        }
    };
}

/// A queue whose due rows cannot be read.
#[derive(Debug)]
struct DueRowsUnreadable(Arc<InMemoryEnrichmentStateRepository>);

wrap_store! {
    impl EnrichmentStateRepository for DueRowsUnreadable {
        delegate {
            fn ensure_pending(&self, target: EnrichmentTargetId) -> Result<(), DbErr>;
            fn find_by_target(&self, target: EnrichmentTargetId) -> Result<Option<EnrichmentState>, DbErr>;
            fn mark_enriched(&self, id: Uuid, matched_ref: &str, confidence: f32, now: DateTime<Utc>) -> Result<(), DbErr>;
            fn mark_unmatched(&self, id: Uuid, reason: &str, now: DateTime<Utc>) -> Result<(), DbErr>;
            fn mark_retrying(&self, id: Uuid, error: &str, attempts: u32, next_attempt_at: DateTime<Utc>, now: DateTime<Utc>) -> Result<(), DbErr>;
            fn mark_failed(&self, id: Uuid, error: &str, now: DateTime<Utc>) -> Result<(), DbErr>;
            fn request_refresh(&self, target: EnrichmentTargetId, rematch: bool) -> Result<bool, DbErr>;
            fn request_refresh_library(&self, library_id: Uuid, rematch: bool) -> Result<u64, DbErr>;
            fn request_refresh_all(&self, rematch: bool) -> Result<u64, DbErr>;
            fn set_locked_fields(&self, target: EnrichmentTargetId, locks: &FieldLocks) -> Result<EnrichmentState, DbErr>;
            fn list(&self, query: &EnrichmentListQuery) -> Result<Vec<EnrichmentState>, DbErr>;
            fn count(&self, filter: &EnrichmentListFilter) -> Result<u64, DbErr>;
            fn count_by_status(&self) -> Result<EnrichmentStatusCounts, DbErr>;
        }

        async fn fetch_due(
            &self,
            _now: DateTime<Utc>,
            _limit: u32,
        ) -> Result<Vec<EnrichmentState>, DbErr> {
            Err(database_down())
        }
    }
}

/// A movie store that cannot take an enrichment.
#[derive(Debug)]
struct EnrichmentUnwritable(Arc<InMemoryMovieRepository>);

wrap_store! {
    impl MovieRepository for EnrichmentUnwritable {
        delegate {
            fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>, DbErr>;
            fn find_all(&self) -> Result<Vec<Movie>, DbErr>;
            fn find_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Movie>, DbErr>;
            fn find_or_create_by_identity(&self, create: CreateMovie) -> Result<Movie, DbErr>;
            fn find_unkeyed(&self) -> Result<Vec<Movie>, DbErr>;
            fn assign_identity_key(&self, movie_id: Uuid, identity_key: &str, version: u16) -> Result<bool, DbErr>;
            fn find_by_identity_key(&self, identity_key: &str) -> Result<Option<Movie>, DbErr>;
            fn find_keyed_before_version(&self, version: u16) -> Result<Vec<Movie>, DbErr>;
            fn rekey(&self, movie_id: Uuid, identity_key: Option<String>, version: u16) -> Result<bool, DbErr>;
            fn retitle_from(&self, movie_id: Uuid, expected: &str, title: &str) -> Result<bool, DbErr>;
            fn find_by_pin(&self, pin: &ProviderPin) -> Result<Option<Movie>, DbErr>;
            fn set_pinned_ref(&self, movie_id: Uuid, pin: &ProviderPin, source: PinSource) -> Result<bool, DbErr>;
            fn clear_admin_pin(&self, movie_id: Uuid) -> Result<bool, DbErr>;
            fn delete_orphaned(&self, created_before: DateTime<Utc>) -> Result<u64, DbErr>;
            fn find_or_create_entry(&self, create: CreateMovieEntry) -> Result<MovieEntry, DbErr>;
            fn find_entries_by_movie_id(&self, movie_id: Uuid) -> Result<Vec<MovieEntry>, DbErr>;
            fn find_entry_by_id(&self, entry_id: Uuid) -> Result<Option<MovieEntry>, DbErr>;
            fn ensure_library_association(&self, library_id: Uuid, movie_id: Uuid) -> Result<(), DbErr>;
            fn library_ids(&self, movie_ids: &[Uuid]) -> Result<HashMap<Uuid, BTreeSet<Uuid>>, DbErr>;
        }

        async fn apply_enrichment(
            &self,
            _movie_id: Uuid,
            _enrichment: &MovieEnrichment,
            _locks: &FieldLocks,
        ) -> Result<(), DbErr> {
            Err(database_down())
        }
    }
}

/// Which of a show's reads fail.
#[derive(Debug, Clone, Copy)]
enum UnreadableShowPart {
    Seasons,
    Episodes,
}

/// A show store whose seasons, or whose seasons' episodes, cannot be read.
#[derive(Debug)]
struct ShowPartUnreadable(Arc<InMemoryShowRepository>, UnreadableShowPart);

wrap_store! {
    impl ShowRepository for ShowPartUnreadable {
        delegate {
            fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr>;
            fn find_all(&self) -> Result<Vec<Show>, DbErr>;
            fn find_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Show>, DbErr>;
            fn child_counts(&self, show_ids: &[Uuid]) -> Result<HashMap<Uuid, ShowChildCounts>, DbErr>;
            fn find_or_create_by_identity(&self, create: CreateShow) -> Result<Show, DbErr>;
            fn find_unkeyed(&self) -> Result<Vec<Show>, DbErr>;
            fn assign_identity_key(&self, show_id: Uuid, identity_key: &str, version: u16) -> Result<bool, DbErr>;
            fn find_by_identity_key(&self, identity_key: &str) -> Result<Option<Show>, DbErr>;
            fn find_keyed_before_version(&self, version: u16) -> Result<Vec<Show>, DbErr>;
            fn rekey(&self, show_id: Uuid, identity_key: Option<String>, version: u16) -> Result<bool, DbErr>;
            fn find_by_pin(&self, pin: &ProviderPin) -> Result<Option<Show>, DbErr>;
            fn set_pinned_ref(&self, show_id: Uuid, pin: &ProviderPin, source: PinSource) -> Result<bool, DbErr>;
            fn clear_admin_pin(&self, show_id: Uuid) -> Result<bool, DbErr>;
            fn delete_orphaned(&self, created_before: DateTime<Utc>) -> Result<u64, DbErr>;
            fn ensure_library_association(&self, library_id: Uuid, show_id: Uuid) -> Result<(), DbErr>;
            fn library_ids(&self, show_ids: &[Uuid]) -> Result<HashMap<Uuid, BTreeSet<Uuid>>, DbErr>;
            fn find_or_create_season(&self, show_id: Uuid, season_number: u32) -> Result<Season, DbErr>;
            fn find_or_create_episode(&self, create: CreateEpisode) -> Result<Episode, DbErr>;
            fn find_episode_by_id(&self, episode_id: Uuid) -> Result<Option<Episode>, DbErr>;
            fn find_season_by_id(&self, season_id: Uuid) -> Result<Option<Season>, DbErr>;
            fn episode_outline(&self, show_id: Uuid) -> Result<Vec<OutlineEpisode>, DbErr>;
            fn episode_outlines(&self, show_ids: &[Uuid]) -> Result<HashMap<Uuid, Vec<OutlineEpisode>>, DbErr>;
            fn apply_enrichment(&self, show_id: Uuid, enrichment: &ShowEnrichment, locks: &FieldLocks) -> Result<(), DbErr>;
            fn apply_season_enrichment(&self, show_id: Uuid, enrichment: &SeasonEnrichment) -> Result<u32, DbErr>;
        }

        async fn find_seasons_by_show_id(&self, show_id: Uuid) -> Result<Vec<Season>, DbErr> {
            match self.1 {
                UnreadableShowPart::Seasons => Err(database_down()),
                UnreadableShowPart::Episodes => self.0.find_seasons_by_show_id(show_id).await,
            }
        }

        async fn find_episodes_by_season_id(&self, season_id: Uuid) -> Result<Vec<Episode>, DbErr> {
            match self.1 {
                UnreadableShowPart::Episodes => Err(database_down()),
                UnreadableShowPart::Seasons => self.0.find_episodes_by_season_id(season_id).await,
            }
        }
    }
}

/// A provider configured by name whose upstream then says it is not
/// configured -- a key revoked, or a client that never got one.
#[derive(Debug)]
struct UnconfiguredUpstream;

#[async_trait::async_trait]
impl EnrichmentProvider for UnconfiguredUpstream {
    fn available_providers(&self) -> Vec<String> {
        vec!["tmdb".to_string()]
    }

    async fn search_movies(
        &self,
        _query: &MediaQuery,
    ) -> Result<Vec<MovieSearchHit>, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn search_shows(
        &self,
        _query: &MediaQuery,
    ) -> Result<Vec<ShowSearchHit>, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn movie_enrichment(
        &self,
        _id: &ExternalMediaRef,
    ) -> Result<MovieEnrichment, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn show_enrichment(
        &self,
        _id: &ExternalMediaRef,
    ) -> Result<ShowEnrichment, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn season_enrichment(
        &self,
        _show: &ExternalMediaRef,
        _season_number: u32,
    ) -> Result<SeasonEnrichment, EnrichmentError> {
        Err(EnrichmentError::NotConfigured)
    }

    async fn invalidate(&self, _id: &ExternalMediaRef) {}
}

/// The stores a sweep reads and writes, in memory, for a test to wrap one of
/// and read back through.
#[derive(Debug, Default)]
struct Stores {
    states: Arc<InMemoryEnrichmentStateRepository>,
    movies: Arc<InMemoryMovieRepository>,
    shows: Arc<InMemoryShowRepository>,
    genres: Arc<InMemoryGenreRepository>,
}

impl Stores {
    fn service(
        &self,
        states: Arc<dyn EnrichmentStateRepository>,
        movies: Arc<dyn MovieRepository>,
        shows: Arc<dyn ShowRepository>,
        provider: Arc<dyn EnrichmentProvider>,
    ) -> MetadataEnrichmentService {
        MetadataEnrichmentService::new(
            states,
            movies,
            shows,
            self.genres.clone(),
            Arc::new(InMemoryPersonRepository::default()),
            Arc::new(InMemoryCollectionRepository::default()),
            provider,
            Arc::new(NoOpAdminLogService),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(TestClock::new()),
        )
    }

    async fn queued_movie(&self, title: &str, year: u32) -> Uuid {
        let movie = self
            .movies
            .find_or_create_by_identity(CreateMovie::new(title.to_string(), Some(year), None))
            .await
            .unwrap();
        self.states
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();
        movie.id
    }

    async fn row(&self, target: EnrichmentTargetId) -> EnrichmentState {
        self.states
            .find_by_target(target)
            .await
            .unwrap()
            .expect("the title keeps its row")
    }
}

fn matrix_hit() -> MovieSearchHit {
    MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "603"),
        title: "The Matrix".to_string(),
        original_title: None,
        year: Some(1999),
        popularity: Some(80.0),
        vote_average: Some(8.7),
    }
}

fn matrix_provider() -> InMemoryEnrichmentProvider {
    InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("The Matrix", vec![matrix_hit()])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            genres: vec!["Action".to_string()],
            ..Default::default()
        })
}

#[tokio::test]
async fn a_db_failure_fetching_due_rows_sweeps_nothing() {
    let stores = Stores::default();
    let movie = stores.queued_movie("The Matrix", 1999).await;
    let service = stores.service(
        Arc::new(DueRowsUnreadable(stores.states.clone())),
        stores.movies.clone(),
        stores.shows.clone(),
        Arc::new(matrix_provider()),
    );

    let report = service.sweep_once().await;

    assert_eq!(report, SweepReport::default(), "nothing was processed");
    let row = stores.row(EnrichmentTargetId::Movie(movie)).await;
    assert_eq!(row.status, EnrichmentStatus::Pending);
    assert_eq!(row.attempts, 0, "a sweep that read nothing spends nothing");
    assert_eq!(
        stores
            .movies
            .find_by_id(movie)
            .await
            .unwrap()
            .unwrap()
            .tmdb_id,
        None,
        "nothing was matched"
    );
}

#[tokio::test]
async fn a_failed_enrichment_write_leaves_the_title_unmatched() {
    let stores = Stores::default();
    let movie = stores.queued_movie("The Matrix", 1999).await;
    let service = stores.service(
        stores.states.clone(),
        Arc::new(EnrichmentUnwritable(stores.movies.clone())),
        stores.shows.clone(),
        Arc::new(matrix_provider()),
    );

    let report = service.sweep_once().await;

    assert_eq!((report.processed, report.unmatched), (1, 1));
    assert_eq!(report.enriched, 0);
    let row = stores.row(EnrichmentTargetId::Movie(movie)).await;
    assert_eq!(row.status, EnrichmentStatus::Unmatched);
    assert_eq!(
        row.last_error.as_deref(),
        Some("failed to persist enrichment")
    );
    assert_eq!(row.matched_ref, None, "no match is recorded for it");
    assert!(
        stores.genres.genres_for_movie(movie).is_empty(),
        "no genre is written for a title whose enrichment was not"
    );
}

/// A show with season 1 queued for enrichment, and a provider that matches
/// it and has a poster for that season.
async fn queued_show_with_a_season(stores: &Stores) -> (Uuid, InMemoryEnrichmentProvider) {
    let show_ref = ExternalMediaRef::new("tmdb", "1399");
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_show_search(
            "Game of Thrones",
            vec![ShowSearchHit {
                external_ref: show_ref.clone(),
                title: "Game of Thrones".to_string(),
                original_title: None,
                year: Some(2011),
                popularity: None,
                vote_average: None,
            }],
        )
        .with_show_enrichment(
            show_ref.clone(),
            ShowEnrichment {
                tmdb_id: Some(1399),
                title: "Game of Thrones".to_string(),
                year: Some(2011),
                ..Default::default()
            },
        )
        .with_season_enrichment(
            show_ref,
            SeasonEnrichment {
                season_number: 1,
                poster_url: Some("https://example.com/s1.jpg".to_string()),
                ..Default::default()
            },
        );
    let show = stores
        .shows
        .find_or_create_by_identity(CreateShow::new("Game of Thrones".to_string(), Some(2011)))
        .await
        .unwrap();
    stores
        .shows
        .find_or_create_season(show.id, 1)
        .await
        .unwrap();
    stores
        .states
        .ensure_pending(EnrichmentTargetId::Show(show.id))
        .await
        .unwrap();
    (show.id, provider)
}

#[tokio::test]
async fn a_failing_season_read_still_enriches_the_show() {
    let stores = Stores::default();
    let (show, provider) = queued_show_with_a_season(&stores).await;
    let service = stores.service(
        stores.states.clone(),
        stores.movies.clone(),
        Arc::new(ShowPartUnreadable(
            stores.shows.clone(),
            UnreadableShowPart::Seasons,
        )),
        Arc::new(provider),
    );

    let report = service.sweep_once().await;

    assert_eq!(report.enriched, 1);
    let row = stores.row(EnrichmentTargetId::Show(show)).await;
    assert_eq!(row.status, EnrichmentStatus::Enriched);
    assert_eq!(row.matched_ref.as_deref(), Some("tmdb:1399"));
    assert_eq!(
        stores
            .shows
            .find_by_id(show)
            .await
            .unwrap()
            .unwrap()
            .tmdb_id,
        Some(1399)
    );
    let seasons = stores.shows.find_seasons_by_show_id(show).await.unwrap();
    assert_eq!(
        seasons[0].poster_url, None,
        "a season that could not be read is left as it was"
    );
}

#[tokio::test]
async fn a_failing_episode_read_still_enriches_the_season() {
    let stores = Stores::default();
    let (show, provider) = queued_show_with_a_season(&stores).await;
    let service = stores.service(
        stores.states.clone(),
        stores.movies.clone(),
        Arc::new(ShowPartUnreadable(
            stores.shows.clone(),
            UnreadableShowPart::Episodes,
        )),
        Arc::new(provider),
    );

    let report = service.sweep_once().await;

    assert_eq!(report.enriched, 1);
    let seasons = stores.shows.find_seasons_by_show_id(show).await.unwrap();
    assert_eq!(
        seasons[0].poster_url.as_deref(),
        Some("https://example.com/s1.jpg"),
        "the season takes its own metadata without its episodes"
    );
}

#[tokio::test]
async fn an_unconfigured_provider_fails_the_row() {
    let stores = Stores::default();
    let movie = stores.queued_movie("The Matrix", 1999).await;
    let service = stores.service(
        stores.states.clone(),
        stores.movies.clone(),
        stores.shows.clone(),
        Arc::new(UnconfiguredUpstream),
    );

    let report = service.sweep_once().await;

    assert_eq!((report.processed, report.failed), (1, 1));
    assert_eq!(report.retrying, 0, "not configured is not worth a retry");
    let row = stores.row(EnrichmentTargetId::Movie(movie)).await;
    assert_eq!(row.status, EnrichmentStatus::Failed);
    assert_eq!(
        row.last_error.as_deref(),
        Some("no enrichment providers configured")
    );
    assert!(
        stores
            .states
            .fetch_due(chrono::DateTime::<Utc>::MAX_UTC, 25)
            .await
            .unwrap()
            .is_empty(),
        "a failed row is never due again"
    );
}
