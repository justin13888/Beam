use super::*;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::catalog::TitleKind;
use beam_domain::models::enrichment::EnrichmentStatus;
use beam_domain::models::{CreateMovie, CreateShow};
use beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider;
use beam_domain::providers::enrichment::{
    CreditEnrichment, CreditRole, MovieEnrichment, MovieSearchHit, ShowEnrichment, ShowSearchHit,
};
use beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository;
use beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;
use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::person::in_memory::InMemoryPersonRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::{MovieRepository, ShowRepository};
use beam_domain::services::TestClock;

type Harness = (
    MetadataEnrichmentService,
    Arc<InMemoryMovieRepository>,
    Arc<InMemoryShowRepository>,
    Arc<InMemoryEnrichmentStateRepository>,
    Arc<InMemoryGenreRepository>,
    Arc<TestClock>,
);

fn harness(provider: InMemoryEnrichmentProvider) -> Harness {
    harness_with_events(provider).0
}

/// [`harness`], and the notification service it announces to.
fn harness_with_events(
    provider: InMemoryEnrichmentProvider,
) -> (Harness, Arc<InMemoryNotificationService>) {
    let (harness, notifications, _people) = full_harness(provider);
    (harness, notifications)
}

fn full_harness(
    provider: InMemoryEnrichmentProvider,
) -> (
    Harness,
    Arc<InMemoryNotificationService>,
    Arc<InMemoryPersonRepository>,
) {
    full_harness_storing_credits(provider, |people| people)
}

/// [`full_harness`], with the service writing credits through the store
/// `credit_store` wraps the in-memory one in.
fn full_harness_storing_credits(
    provider: InMemoryEnrichmentProvider,
    credit_store: impl FnOnce(Arc<InMemoryPersonRepository>) -> Arc<dyn PersonRepository>,
) -> (
    Harness,
    Arc<InMemoryNotificationService>,
    Arc<InMemoryPersonRepository>,
) {
    harness_storing(
        Arc::new(provider),
        credit_store,
        Arc::new(InMemoryCollectionRepository::default()),
    )
}

fn harness_storing(
    provider: Arc<InMemoryEnrichmentProvider>,
    credit_store: impl FnOnce(Arc<InMemoryPersonRepository>) -> Arc<dyn PersonRepository>,
    collections: Arc<dyn CollectionRepository>,
) -> (
    Harness,
    Arc<InMemoryNotificationService>,
    Arc<InMemoryPersonRepository>,
) {
    let people = Arc::new(InMemoryPersonRepository::default());
    let notifications = Arc::new(InMemoryNotificationService::new());
    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let show_repo = Arc::new(InMemoryShowRepository::default());
    let state_repo = Arc::new(InMemoryEnrichmentStateRepository::default());
    let genre_repo = Arc::new(InMemoryGenreRepository::default());
    let admin_log_repo = Arc::new(
        beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository::default(),
    );
    let admin_log = admin_log_repo;
    let clock = Arc::new(TestClock::new());

    let service = MetadataEnrichmentService::new(
        state_repo.clone(),
        movie_repo.clone(),
        show_repo.clone(),
        genre_repo.clone(),
        credit_store(people.clone()),
        collections,
        provider,
        admin_log,
        notifications.clone(),
        clock.clone(),
    );
    (
        (
            service, movie_repo, show_repo, state_repo, genre_repo, clock,
        ),
        notifications,
        people,
    )
}

/// A pinned movie, queued as the indexer queues a newly pinned one.
async fn pinned_movie(
    movie_repo: &InMemoryMovieRepository,
    state_repo: &InMemoryEnrichmentStateRepository,
    title: &str,
    pin: ProviderPin,
) -> uuid::Uuid {
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new(title.to_string(), None, None))
        .await
        .unwrap();
    assert!(
        movie_repo
            .set_pinned_ref(movie.id, &pin, beam_domain::models::PinSource::Nfo)
            .await
            .unwrap()
    );
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();
    state_repo
        .request_refresh(EnrichmentTargetId::Movie(movie.id), true)
        .await
        .unwrap();
    movie.id
}

/// What a match stores beside the title: its cast and crew, and a movie's collection.
#[path = "service_credits_tests.rs"]
mod credits;

/// A sweep: when it runs, what it matches, and what it announces.
#[path = "service_sweep_tests.rs"]
mod sweep;

/// What a sweep does when the provider fails or has lost a match:
/// retries, rate limits, refreshes and falling back to the pin.
#[path = "service_retry_tests.rs"]
mod retries;

/// A show's seasons, a vanished title, the sweep's metrics, and titles
/// an NFO or an administrator pins.
#[path = "service_title_tests.rs"]
mod titles;
