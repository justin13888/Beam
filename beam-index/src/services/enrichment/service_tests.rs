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

/// [`harness`], and the store credits are written to.
fn harness_with_people(
    provider: InMemoryEnrichmentProvider,
) -> (Harness, Arc<InMemoryPersonRepository>) {
    let (harness, _notifications, people) = full_harness(provider);
    (harness, people)
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

/// [`harness`] over a provider the test keeps a handle on, to read back
/// what the sweep asked of it.
fn harness_sharing(provider: Arc<InMemoryEnrichmentProvider>) -> Harness {
    harness_storing(
        provider,
        |people| people,
        Arc::new(InMemoryCollectionRepository::default()),
    )
    .0
}

/// [`harness`], and the store collections are written to.
fn harness_with_collections(
    provider: InMemoryEnrichmentProvider,
    collections: Arc<dyn CollectionRepository>,
) -> Harness {
    harness_storing(Arc::new(provider), |people| people, collections).0
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

fn credit(native: &str, name: &str, role: CreditRole) -> CreditEnrichment {
    CreditEnrichment {
        person_ref: ExternalMediaRef::new("tmdb", native),
        name: name.to_string(),
        profile_url: None,
        role,
    }
}

fn lead(character: &str) -> CreditRole {
    CreditRole::Cast {
        character: Some(character.to_string()),
        billing_order: 0,
    }
}

/// The names credited on a movie that held `stored` credits once a match
/// reporting `reported` is applied to it.
async fn movie_credits_after_match(
    stored: &[CreditEnrichment],
    reported: Option<Vec<CreditEnrichment>>,
) -> Vec<String> {
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "603"),
        title: "The Matrix".to_string(),
        original_title: None,
        year: Some(1999),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("The Matrix", vec![hit])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            credits: reported,
            ..Default::default()
        });
    let ((service, movie_repo, _shows, state_repo, _genres, _clock), people) =
        harness_with_people(provider);
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("The Matrix".to_string(), Some(1999), None))
        .await
        .unwrap();
    people.set_movie_credits(movie.id, stored).await.unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    let mut names: Vec<String> = people
        .movie_credits(movie.id)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.person.name)
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_match_replaces_the_cast_and_crew_whole() {
    let names = movie_credits_after_match(
        &[credit("1", "Stale Credit", lead("Old"))],
        Some(vec![
            credit("6384", "Keanu Reeves", lead("Neo")),
            credit(
                "9339",
                "Lana Wachowski",
                CreditRole::Crew {
                    job: "Director".to_string(),
                    department: Some("Directing".to_string()),
                },
            ),
        ]),
    )
    .await;
    assert_eq!(names, ["Keanu Reeves", "Lana Wachowski"]);
}

#[tokio::test]
async fn a_match_reporting_no_credits_clears_them() {
    let names =
        movie_credits_after_match(&[credit("1", "Stale Credit", lead("Old"))], Some(vec![])).await;
    assert!(names.is_empty(), "{names:?}");
}

#[tokio::test]
async fn a_match_without_credits_keeps_the_ones_stored() {
    let names =
        movie_credits_after_match(&[credit("6384", "Keanu Reeves", lead("Neo"))], None).await;
    assert_eq!(
        names,
        ["Keanu Reeves"],
        "credits a provider did not report are not credits it removed"
    );
}

#[tokio::test]
async fn a_show_match_stores_its_credits() {
    let show_ref = ExternalMediaRef::new("tmdb", "1396");
    let hit = ShowSearchHit {
        external_ref: show_ref.clone(),
        title: "Breaking Bad".to_string(),
        original_title: None,
        year: Some(2008),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_show_search("Breaking Bad", vec![hit])
        .with_show_enrichment(
            show_ref,
            ShowEnrichment {
                tmdb_id: Some(1396),
                title: "Breaking Bad".to_string(),
                year: Some(2008),
                credits: Some(vec![credit(
                    "17419",
                    "Bryan Cranston",
                    lead("Walter White"),
                )]),
                ..Default::default()
            },
        );
    let ((service, _movies, show_repo, state_repo, _genres, _clock), people) =
        harness_with_people(provider);
    let show = show_repo
        .find_or_create_by_identity(CreateShow::new("Breaking Bad".to_string(), Some(2008)))
        .await
        .unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Show(show.id))
        .await
        .unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    let credits = people.show_credits(show.id).await.unwrap();
    assert_eq!(credits.len(), 1);
    assert_eq!(credits[0].person.name, "Bryan Cranston");
    assert_eq!(credits[0].role, lead("Walter White"));
}

fn matrix_collection() -> beam_domain::providers::enrichment::CollectionEnrichment {
    beam_domain::providers::enrichment::CollectionEnrichment {
        collection_ref: ExternalMediaRef::new("tmdb", "2344"),
        name: "The Matrix Collection".to_string(),
        poster_url: Some("https://image.tmdb.org/collection.jpg".to_string()),
        backdrop_url: None,
    }
}

/// The collection of a movie that was in `stored` once a match reporting
/// `reported` is applied to it, through a service writing to
/// `collections`.
async fn movie_collection_after_match(
    collections: Arc<dyn CollectionRepository>,
    stored: Option<beam_domain::providers::enrichment::CollectionEnrichment>,
    reported: Option<beam_domain::providers::enrichment::CollectionEnrichment>,
) -> (
    Option<beam_domain::models::collection::Collection>,
    Arc<InMemoryEnrichmentStateRepository>,
    uuid::Uuid,
) {
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "603"),
        title: "The Matrix".to_string(),
        original_title: None,
        year: Some(1999),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("The Matrix", vec![hit])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            collection: reported,
            ..Default::default()
        });
    let (service, movie_repo, _shows, state_repo, _genres, _clock) =
        harness_with_collections(provider, collections.clone());
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("The Matrix".to_string(), Some(1999), None))
        .await
        .unwrap();
    if let Some(stored) = &stored {
        collections
            .set_movie_collection(movie.id, Some(stored))
            .await
            .unwrap();
    }
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    (
        collections.movie_collection(movie.id).await.unwrap(),
        state_repo,
        movie.id,
    )
}

#[tokio::test]
async fn a_movie_match_stores_its_collection() {
    let (collection, _, _) = movie_collection_after_match(
        Arc::new(InMemoryCollectionRepository::default()),
        None,
        Some(matrix_collection()),
    )
    .await;
    let collection = collection.expect("the movie's collection");
    assert_eq!(collection.external_ref, "tmdb:2344");
    assert_eq!(collection.name, "The Matrix Collection");
}

#[tokio::test]
async fn a_match_reporting_no_collection_takes_the_movie_out_of_its_old_one() {
    let (collection, _, _) = movie_collection_after_match(
        Arc::new(InMemoryCollectionRepository::default()),
        Some(matrix_collection()),
        None,
    )
    .await;
    assert_eq!(collection, None);
}

/// A collection store whose writes fail, its reads answered by the
/// in-memory store it wraps.
#[derive(Debug)]
struct FailingCollectionWrites(InMemoryCollectionRepository);

#[async_trait::async_trait]
impl CollectionRepository for FailingCollectionWrites {
    async fn set_movie_collection(
        &self,
        _: uuid::Uuid,
        _: Option<&beam_domain::providers::enrichment::CollectionEnrichment>,
    ) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(
            "collection store unavailable".to_string(),
        ))
    }
    async fn movie_collection(
        &self,
        movie_id: uuid::Uuid,
    ) -> Result<Option<beam_domain::models::collection::Collection>, sea_orm::DbErr> {
        self.0.movie_collection(movie_id).await
    }
    async fn find_by_id(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<beam_domain::models::collection::Collection>, sea_orm::DbErr> {
        self.0.find_by_id(id).await
    }
    async fn movie_ids(
        &self,
        id: uuid::Uuid,
        access: &beam_domain::models::access::AccessPolicy,
    ) -> Result<Vec<uuid::Uuid>, sea_orm::DbErr> {
        self.0.movie_ids(id, access).await
    }
    async fn list(
        &self,
        access: &beam_domain::models::access::AccessPolicy,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<beam_domain::models::collection::CollectionSummary>, sea_orm::DbErr> {
        self.0.list(access, limit, offset).await
    }
    async fn count(
        &self,
        access: &beam_domain::models::access::AccessPolicy,
    ) -> Result<u64, sea_orm::DbErr> {
        self.0.count(access).await
    }
}

#[tokio::test]
async fn a_movie_whose_collection_fails_to_store_is_enriched_all_the_same() {
    let (collection, state_repo, movie_id) = movie_collection_after_match(
        Arc::new(FailingCollectionWrites(
            InMemoryCollectionRepository::default(),
        )),
        None,
        Some(matrix_collection()),
    )
    .await;
    assert_eq!(collection, None, "the write failed, so nothing is stored");
    assert_enriched_as(&state_repo, EnrichmentTargetId::Movie(movie_id), "tmdb:603").await;
}

/// A person store whose credit writes fail, its reads answered by the
/// in-memory store it wraps.
#[derive(Debug)]
struct FailingCreditWrites(Arc<InMemoryPersonRepository>);

#[async_trait::async_trait]
impl PersonRepository for FailingCreditWrites {
    async fn set_movie_credits(
        &self,
        _: uuid::Uuid,
        _: &[CreditEnrichment],
    ) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(
            "credits store unavailable".to_string(),
        ))
    }
    async fn set_show_credits(
        &self,
        _: uuid::Uuid,
        _: &[CreditEnrichment],
    ) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(
            "credits store unavailable".to_string(),
        ))
    }
    async fn movie_credits(
        &self,
        movie_id: uuid::Uuid,
    ) -> Result<Vec<beam_domain::models::person::TitleCredit>, sea_orm::DbErr> {
        self.0.movie_credits(movie_id).await
    }
    async fn show_credits(
        &self,
        show_id: uuid::Uuid,
    ) -> Result<Vec<beam_domain::models::person::TitleCredit>, sea_orm::DbErr> {
        self.0.show_credits(show_id).await
    }
    async fn find_by_id(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<beam_domain::models::person::Person>, sea_orm::DbErr> {
        self.0.find_by_id(id).await
    }
    async fn filmography(
        &self,
        person_id: uuid::Uuid,
        access: &beam_domain::models::access::AccessPolicy,
    ) -> Result<Vec<beam_domain::models::person::PersonCredit>, sea_orm::DbErr> {
        self.0.filmography(person_id, access).await
    }
    async fn search(
        &self,
        query: Option<&str>,
        access: &beam_domain::models::access::AccessPolicy,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<beam_domain::models::person::Person>, sea_orm::DbErr> {
        self.0.search(query, access, limit, offset).await
    }
    async fn count(
        &self,
        query: Option<&str>,
        access: &beam_domain::models::access::AccessPolicy,
    ) -> Result<u64, sea_orm::DbErr> {
        self.0.count(query, access).await
    }
}

fn failing_credit_writes(people: Arc<InMemoryPersonRepository>) -> Arc<dyn PersonRepository> {
    Arc::new(FailingCreditWrites(people))
}

/// The row enrichment leaves for `target`: enriched, and matched to
/// `matched`.
async fn assert_enriched_as(
    state_repo: &InMemoryEnrichmentStateRepository,
    target: EnrichmentTargetId,
    matched: &str,
) {
    let row = state_repo.find_by_target(target).await.unwrap().unwrap();
    assert_eq!(row.status, EnrichmentStatus::Enriched, "{row:?}");
    assert_eq!(row.matched_ref.as_deref(), Some(matched));
}

#[tokio::test]
async fn a_movie_whose_credits_fail_to_store_is_enriched_and_keeps_its_old_ones() {
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "603"),
        title: "The Matrix".to_string(),
        original_title: None,
        year: Some(1999),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("The Matrix", vec![hit])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            credits: Some(vec![credit("6384", "Keanu Reeves", lead("Neo"))]),
            ..Default::default()
        });
    let ((service, movie_repo, _shows, state_repo, _genres, _clock), _events, people) =
        full_harness_storing_credits(provider, failing_credit_writes);
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("The Matrix".to_string(), Some(1999), None))
        .await
        .unwrap();
    people
        .set_movie_credits(movie.id, &[credit("1", "Stale Credit", lead("Old"))])
        .await
        .unwrap();
    let target = EnrichmentTargetId::Movie(movie.id);
    state_repo.ensure_pending(target).await.unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    assert_enriched_as(&state_repo, target, "tmdb:603").await;
    let names: Vec<String> = people
        .movie_credits(movie.id)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.person.name)
        .collect();
    assert_eq!(names, ["Stale Credit"]);
}

#[tokio::test]
async fn a_show_whose_credits_fail_to_store_is_enriched_and_keeps_its_old_ones() {
    let show_ref = ExternalMediaRef::new("tmdb", "1396");
    let hit = ShowSearchHit {
        external_ref: show_ref.clone(),
        title: "Breaking Bad".to_string(),
        original_title: None,
        year: Some(2008),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_show_search("Breaking Bad", vec![hit])
        .with_show_enrichment(
            show_ref,
            ShowEnrichment {
                tmdb_id: Some(1396),
                title: "Breaking Bad".to_string(),
                year: Some(2008),
                credits: Some(vec![credit(
                    "17419",
                    "Bryan Cranston",
                    lead("Walter White"),
                )]),
                ..Default::default()
            },
        );
    let ((service, _movies, show_repo, state_repo, _genres, _clock), _events, people) =
        full_harness_storing_credits(provider, failing_credit_writes);
    let show = show_repo
        .find_or_create_by_identity(CreateShow::new("Breaking Bad".to_string(), Some(2008)))
        .await
        .unwrap();
    people
        .set_show_credits(show.id, &[credit("1", "Stale Credit", lead("Old"))])
        .await
        .unwrap();
    let target = EnrichmentTargetId::Show(show.id);
    state_repo.ensure_pending(target).await.unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    assert_enriched_as(&state_repo, target, "tmdb:1396").await;
    let names: Vec<String> = people
        .show_credits(show.id)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.person.name)
        .collect();
    assert_eq!(names, ["Stale Credit"]);
}

#[tokio::test]
async fn no_providers_configured_skips_sweep_entirely() {
    let (service, ..) = harness(InMemoryEnrichmentProvider::new(&[]));
    let report = service.sweep_once().await;
    assert!(report.skipped_no_providers);
    assert_eq!(report.processed, 0);
}

/// The worker sweeps at once, then waits: a title queued meanwhile waits
/// for the interval, unless the worker is poked, when it is swept at once.
/// The injected clock never moves here, so only the poke can wake it.
#[tokio::test]
async fn a_poke_starts_the_next_sweep_without_waiting_for_the_interval() {
    // No candidate for anything: a swept title ends unmatched.
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, clock) =
        harness(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let queue = |title: &'static str| {
        let movie_repo = movie_repo.clone();
        let state_repo = state_repo.clone();
        async move {
            let movie = movie_repo
                .find_or_create_by_identity(CreateMovie::new(title.to_string(), None, None))
                .await
                .unwrap();
            let target = EnrichmentTargetId::Movie(movie.id);
            state_repo.ensure_pending(target).await.unwrap();
            target
        }
    };
    let status = |target: EnrichmentTargetId| {
        let state_repo = state_repo.clone();
        async move {
            state_repo
                .find_by_target(target)
                .await
                .unwrap()
                .unwrap()
                .status
        }
    };
    let first = queue("Heat").await;
    let poke = service.notify_handle();
    let service = Arc::new(service);
    let worker = tokio::spawn({
        let service = service.clone();
        async move { service.run(Duration::from_secs(3600)).await }
    });

    // Bounded only against a hang; nothing here is ordered by time.
    let swept = |target| async move {
        tokio::time::timeout(Duration::from_secs(10), async {
            while status(target).await != EnrichmentStatus::Unmatched {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok()
    };
    assert!(swept(first).await, "the first sweep runs at once");
    // The worker is now waiting on the interval or a poke.
    tokio::time::timeout(Duration::from_secs(10), async {
        while clock.waiter_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the worker waits on the interval");

    let second = queue("Alien").await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        status(second).await,
        EnrichmentStatus::Pending,
        "unpoked, the worker waits out the interval"
    );

    poke.notify_one();
    assert!(swept(second).await, "a poke sweeps at once");
    worker.abort();
}

#[tokio::test]
async fn successful_movie_match_applies_enrichment_and_genres() {
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "603"),
        title: "The Matrix".to_string(),
        original_title: None,
        year: Some(1999),
        popularity: Some(80.0),
        vote_average: Some(8.7),
    };
    let enrichment = MovieEnrichment {
        tmdb_id: Some(603),
        title: "The Matrix".to_string(),
        year: Some(1999),
        genres: vec!["Science Fiction".to_string(), "Action".to_string()],
        ..Default::default()
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("The Matrix", vec![hit])
        .with_movie_enrichment(enrichment);

    let (service, movie_repo, _show_repo, state_repo, genre_repo, _clock) = harness(provider);

    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("The Matrix".to_string(), Some(1999), None))
        .await
        .unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();

    let report = service.sweep_once().await;
    assert_eq!(report.enriched, 1);

    let updated = movie_repo.find_by_id(movie.id).await.unwrap().unwrap();
    assert_eq!(updated.tmdb_id, Some(603));
    assert_eq!(genre_repo.genres_for_movie(movie.id).len(), 2);
}

#[tokio::test]
async fn strict_min_confidence_policy_rejects_borderline_match() {
    // Exact title, but the local movie carries no year, so the candidate
    // scores exactly the title weight (0.70): accepted under the default
    // policy, rejected once the operator raises BEAM_ENRICH_MIN_CONFIDENCE
    // above it. Same fixture, opposite outcome purely on the knob.
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "603"),
        title: "The Matrix".to_string(),
        original_title: None,
        year: Some(1999),
        popularity: None,
        vote_average: None,
    };
    let provider =
        InMemoryEnrichmentProvider::new(&["tmdb"]).with_movie_search("The Matrix", vec![hit]);
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) = harness(provider);
    let service = service.with_policy(EnrichmentPolicy {
        min_confidence: 0.9,
        ..EnrichmentPolicy::default()
    });

    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("The Matrix".to_string(), None, None))
        .await
        .unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();

    let report = service.sweep_once().await;
    assert_eq!(report.unmatched, 1);
    assert_eq!(report.enriched, 0);
}

#[tokio::test]
async fn no_match_marks_unmatched_and_logs_candidates() {
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "1"),
        title: "Totally Unrelated".to_string(),
        original_title: None,
        year: Some(1950),
        popularity: None,
        vote_average: None,
    };
    let provider =
        InMemoryEnrichmentProvider::new(&["tmdb"]).with_movie_search("My Movie", vec![hit]);
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) = harness(provider);

    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("My Movie".to_string(), Some(2020), None))
        .await
        .unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();

    let report = service.sweep_once().await;
    assert_eq!(report.unmatched, 1);
}

/// FR-309: every title a sweep attempts is announced once, with where it
/// now stands; a title the sweep never reached says nothing.
#[tokio::test]
async fn each_attempted_title_is_announced_once_with_its_outcome() {
    use crate::services::notification::EnrichmentEvent;
    use beam_domain::models::enrichment::EnrichmentStatus;

    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search(
            "The Matrix",
            vec![MovieSearchHit {
                external_ref: ExternalMediaRef::new("tmdb", "603"),
                title: "The Matrix".to_string(),
                original_title: None,
                year: Some(1999),
                popularity: None,
                vote_average: None,
            }],
        )
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            ..Default::default()
        });
    let ((service, movie_repo, show_repo, state_repo, _genres, _clock), events) =
        harness_with_events(provider);
    let matrix = movie_repo
        .find_or_create_by_identity(CreateMovie::new("The Matrix".to_string(), Some(1999), None))
        .await
        .unwrap();
    let unknown = show_repo
        .find_or_create_by_identity(CreateShow::new("Nowhere Show".to_string(), None))
        .await
        .unwrap();
    for target in [
        EnrichmentTargetId::Movie(matrix.id),
        EnrichmentTargetId::Show(unknown.id),
    ] {
        state_repo.ensure_pending(target).await.unwrap();
    }

    let report = service.sweep_once().await;
    assert_eq!((report.enriched, report.unmatched), (1, 1));

    let mut announced: Vec<(EventLevel, EnrichmentEvent)> = events
        .published_events()
        .into_iter()
        .map(|event| {
            assert_eq!(event.category, EventCategory::Enrichment);
            (event.level, event.enrichment.expect("it names its title"))
        })
        .collect();
    announced.sort_by_key(|(_, e)| e.title.clone());
    assert_eq!(
        announced,
        vec![
            (
                EventLevel::Warning,
                EnrichmentEvent {
                    target: EnrichmentTargetId::Show(unknown.id),
                    title: Some("Nowhere Show".to_string()),
                    status: EnrichmentStatus::Unmatched,
                    matched_ref: None,
                    error: Some("no candidate cleared the match threshold".to_string()),
                }
            ),
            (
                EventLevel::Info,
                EnrichmentEvent {
                    target: EnrichmentTargetId::Movie(matrix.id),
                    title: Some("The Matrix".to_string()),
                    status: EnrichmentStatus::Enriched,
                    matched_ref: Some("tmdb:603".to_string()),
                    error: None,
                }
            ),
        ]
    );
}

#[tokio::test]
async fn a_rate_limited_sweep_announces_nothing_for_the_title_it_did_not_attempt() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_search_error(|| EnrichmentError::RateLimited { retry_after: None });
    let ((service, movie_repo, _shows, state_repo, _genres, _clock), events) =
        harness_with_events(provider);
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("Heat".to_string(), None, None))
        .await
        .unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();

    assert!(service.sweep_once().await.rate_limited);
    assert!(events.published_events().is_empty());
}

/// Sweep `target` until its row leaves `Pending`, advancing the clock
/// past each backoff, and return how many sweeps attempted it.
async fn sweep_until_settled(
    service: &MetadataEnrichmentService,
    state_repo: &InMemoryEnrichmentStateRepository,
    clock: &TestClock,
    target: EnrichmentTargetId,
) -> u32 {
    let mut sweeps = 0;
    loop {
        sweeps += service.sweep_once().await.processed;
        let row = state_repo.find_by_target(target).await.unwrap().unwrap();
        if row.status != EnrichmentStatus::Pending {
            return sweeps;
        }
        assert!(sweeps < 50, "never settled: {row:?}");
        clock.advance(Duration::from_secs(90_000));
    }
}

/// A pinned title the provider answers "not found" for is retried with
/// backoff (FR-304) -- the answer may be a passing fault -- naming the id
/// meanwhile, and left unmatched, never failed, once the attempts are
/// spent (FR-313): for a movie and for a show alike.
#[tokio::test]
async fn a_pin_the_provider_has_no_title_for_is_retried_then_left_unmatched() {
    use beam_domain::models::PinSource;

    for kind in [TitleKind::Movie, TitleKind::Show] {
        let provider = InMemoryEnrichmentProvider::new(&["tmdb"]);
        let (service, movie_repo, show_repo, state_repo, _genres, clock) = harness(provider);
        let pin = ProviderPin::Tmdb(424_242);
        let target = match kind {
            TitleKind::Movie => {
                let movie = movie_repo
                    .find_or_create_by_identity(CreateMovie::new("Heat".to_string(), None, None))
                    .await
                    .unwrap();
                assert!(
                    movie_repo
                        .set_pinned_ref(movie.id, &pin, PinSource::Nfo)
                        .await
                        .unwrap()
                );
                EnrichmentTargetId::Movie(movie.id)
            }
            TitleKind::Show => {
                let show = show_repo
                    .find_or_create_by_identity(CreateShow::new("Shogun".to_string(), None))
                    .await
                    .unwrap();
                assert!(
                    show_repo
                        .set_pinned_ref(show.id, &pin, PinSource::Admin)
                        .await
                        .unwrap()
                );
                EnrichmentTargetId::Show(show.id)
            }
        };
        state_repo.ensure_pending(target).await.unwrap();

        let report = service.sweep_once().await;
        assert_eq!((report.retrying, report.unmatched), (1, 0), "{kind:?}");
        let row = state_repo.find_by_target(target).await.unwrap().unwrap();
        assert_eq!(row.status, EnrichmentStatus::Pending, "{kind:?}");
        assert_eq!(row.attempts, 1, "{kind:?}");
        assert!(row.next_attempt_at.is_some(), "{kind:?}: backed off");
        assert_eq!(
            row.last_error.as_deref(),
            Some("the provider has no title tmdb:424242"),
            "{kind:?}"
        );

        let sweeps = sweep_until_settled(&service, &state_repo, &clock, target).await;
        let row = state_repo.find_by_target(target).await.unwrap().unwrap();
        assert_eq!(
            1 + sweeps,
            EnrichmentPolicy::default().max_attempts,
            "{kind:?}"
        );
        assert_eq!(row.status, EnrichmentStatus::Unmatched, "{kind:?}");
        assert_eq!(
            row.last_error.as_deref(),
            Some("the provider has no title tmdb:424242"),
            "{kind:?}"
        );
    }
}

/// A refresh of a pinned title's match fetches by that match; a match
/// the provider no longer has falls back to the pin (issue #246) -- one
/// a provider configured since resolves -- and the dead match is
/// forgotten.
#[tokio::test]
async fn a_pinned_titles_lost_match_falls_back_to_the_pin() {
    let provider =
        InMemoryEnrichmentProvider::new(&["tmdb"]).with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            ..Default::default()
        });
    let (service, movie_repo, _shows, state_repo, _genres, clock) = harness(provider);
    let id = pinned_movie(&movie_repo, &state_repo, "matrix", ProviderPin::Tmdb(603)).await;
    let target = EnrichmentTargetId::Movie(id);
    let row = state_repo.find_by_target(target).await.unwrap().unwrap().id;
    state_repo
        .mark_enriched(row, "tmdb:999", 1.0, clock.now())
        .await
        .unwrap();
    state_repo.request_refresh(target, false).await.unwrap();

    let report = service.sweep_once().await;

    assert_eq!(report.enriched, 1, "{report:?}");
    let row = state_repo.find_by_target(target).await.unwrap().unwrap();
    assert_eq!(row.matched_ref.as_deref(), Some("tmdb:603"));
    assert_eq!(
        movie_repo.find_by_id(id).await.unwrap().unwrap().title,
        "The Matrix"
    );
}

/// As for a movie, a pinned show's lost match falls back to its pin.
#[tokio::test]
async fn a_pinned_shows_lost_match_falls_back_to_the_pin() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"]).with_show_enrichment(
        ExternalMediaRef::new("tmdb", "1399"),
        ShowEnrichment {
            tmdb_id: Some(1399),
            title: "Game of Thrones".to_string(),
            ..Default::default()
        },
    );
    let (service, _movies, show_repo, state_repo, _genres, clock) = harness(provider);
    let show = show_repo
        .find_or_create_by_identity(CreateShow::new("got".to_string(), None))
        .await
        .unwrap();
    assert!(
        show_repo
            .set_pinned_ref(
                show.id,
                &ProviderPin::Tmdb(1399),
                beam_domain::models::PinSource::Nfo
            )
            .await
            .unwrap()
    );
    let target = EnrichmentTargetId::Show(show.id);
    state_repo.ensure_pending(target).await.unwrap();
    let row = state_repo.find_by_target(target).await.unwrap().unwrap().id;
    state_repo
        .mark_enriched(row, "tmdb:999", 1.0, clock.now())
        .await
        .unwrap();
    state_repo.request_refresh(target, false).await.unwrap();

    let report = service.sweep_once().await;

    assert_eq!(report.enriched, 1, "{report:?}");
    let row = state_repo.find_by_target(target).await.unwrap().unwrap();
    assert_eq!(row.matched_ref.as_deref(), Some("tmdb:1399"));
    assert_eq!(
        show_repo.find_by_id(show.id).await.unwrap().unwrap().title,
        "Game of Thrones"
    );
}

/// A pinned title whose match and pin the provider both no longer has is
/// retried by the pin, which the retry names.
#[tokio::test]
async fn a_pinned_titles_lost_match_and_pin_are_retried_by_the_pin() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"]);
    let (service, movie_repo, _shows, state_repo, _genres, clock) = harness(provider);
    let id = pinned_movie(&movie_repo, &state_repo, "matrix", ProviderPin::Tmdb(603)).await;
    let target = EnrichmentTargetId::Movie(id);
    let row = state_repo.find_by_target(target).await.unwrap().unwrap().id;
    state_repo
        .mark_enriched(row, "tmdb:999", 1.0, clock.now())
        .await
        .unwrap();
    state_repo.request_refresh(target, false).await.unwrap();

    let report = service.sweep_once().await;

    assert_eq!(report.retrying, 1, "{report:?}");
    let row = state_repo.find_by_target(target).await.unwrap().unwrap();
    assert_eq!(
        row.last_error.as_deref(),
        Some("the provider has no title tmdb:603")
    );
    assert_eq!(
        movie_repo.find_by_id(id).await.unwrap().unwrap().title,
        "matrix",
        "nothing was applied"
    );
}

/// A pinned title whose match is its pin, and which the provider no
/// longer has, is retried by that id and keeps the match: the dead match
/// is the pin, so it is not forgotten (issue #259) -- for a movie and for
/// a show alike.
#[tokio::test]
async fn a_pinned_titles_lost_match_equal_to_its_pin_is_retried_and_kept() {
    use beam_domain::models::PinSource;

    for kind in [TitleKind::Movie, TitleKind::Show] {
        let provider = InMemoryEnrichmentProvider::new(&["tmdb"]);
        let (service, movie_repo, show_repo, state_repo, _genres, clock) = harness(provider);
        let pin = ProviderPin::Tmdb(603);
        let target = match kind {
            TitleKind::Movie => {
                let id = pinned_movie(&movie_repo, &state_repo, "matrix", pin).await;
                EnrichmentTargetId::Movie(id)
            }
            TitleKind::Show => {
                let show = show_repo
                    .find_or_create_by_identity(CreateShow::new("got".to_string(), None))
                    .await
                    .unwrap();
                assert!(
                    show_repo
                        .set_pinned_ref(show.id, &pin, PinSource::Nfo)
                        .await
                        .unwrap()
                );
                let target = EnrichmentTargetId::Show(show.id);
                state_repo.ensure_pending(target).await.unwrap();
                target
            }
        };
        let row = state_repo.find_by_target(target).await.unwrap().unwrap().id;
        state_repo
            .mark_enriched(row, "tmdb:603", 1.0, clock.now())
            .await
            .unwrap();
        state_repo.request_refresh(target, false).await.unwrap();

        let report = service.sweep_once().await;

        assert_eq!(
            (report.retrying, report.unmatched),
            (1, 0),
            "{kind:?}: {report:?}"
        );
        let row = state_repo.find_by_target(target).await.unwrap().unwrap();
        assert_eq!(row.status, EnrichmentStatus::Pending, "{kind:?}");
        assert_eq!(
            row.last_error.as_deref(),
            Some("the provider has no title tmdb:603"),
            "{kind:?}"
        );
        assert_eq!(
            row.matched_ref.as_deref(),
            Some("tmdb:603"),
            "{kind:?}: the match equal to the pin is kept"
        );
    }
}

/// An unpinned title whose match the provider no longer has is searched
/// for by its name in the same pass, as a title never matched is, rather
/// than retried by the dead id -- and the dead id is gone from its row
/// whatever the search finds.
#[tokio::test]
async fn an_unpinned_titles_lost_match_is_searched_for_again() {
    let movie_hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "949"),
        title: "Heat".to_string(),
        original_title: None,
        year: Some(1995),
        popularity: None,
        vote_average: None,
    };
    let show_hit = ShowSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "1399"),
        title: "Shogun".to_string(),
        original_title: None,
        year: Some(2024),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("Heat", vec![movie_hit])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(949),
            title: "Heat".to_string(),
            year: Some(1995),
            ..Default::default()
        })
        .with_show_search("Shogun", vec![show_hit])
        .with_show_enrichment(
            ExternalMediaRef::new("tmdb", "1399"),
            ShowEnrichment {
                tmdb_id: Some(1399),
                title: "Shogun".to_string(),
                year: Some(2024),
                ..Default::default()
            },
        );
    let (service, movie_repo, show_repo, state_repo, _genres, clock) = harness(provider);
    let found = movie_repo
        .find_or_create_by_identity(CreateMovie::new("Heat".to_string(), Some(1995), None))
        .await
        .unwrap();
    let show = show_repo
        .find_or_create_by_identity(CreateShow::new("Shogun".to_string(), Some(2024)))
        .await
        .unwrap();
    let lost = movie_repo
        .find_or_create_by_identity(CreateMovie::new("Nowhere".to_string(), None, None))
        .await
        .unwrap();
    let targets = [
        (EnrichmentTargetId::Movie(found.id), Some("tmdb:949")),
        (EnrichmentTargetId::Show(show.id), Some("tmdb:1399")),
        (EnrichmentTargetId::Movie(lost.id), None),
    ];
    for (target, _) in targets {
        state_repo.ensure_pending(target).await.unwrap();
        let row = state_repo.find_by_target(target).await.unwrap().unwrap().id;
        state_repo
            .mark_enriched(row, "tmdb:1", 0.8, clock.now())
            .await
            .unwrap();
        state_repo.request_refresh(target, false).await.unwrap();
    }

    let report = service.sweep_once().await;

    assert_eq!((report.enriched, report.unmatched), (2, 1), "{report:?}");
    assert_eq!(report.retrying, 0, "the dead id is never retried");
    for (target, matched) in targets {
        let row = state_repo.find_by_target(target).await.unwrap().unwrap();
        assert_eq!(row.matched_ref.as_deref(), matched, "{target:?}");
    }
    assert_eq!(
        movie_repo
            .find_by_id(found.id)
            .await
            .unwrap()
            .unwrap()
            .tmdb_id,
        Some(949)
    );
}

#[tokio::test]
async fn rate_limited_search_aborts_sweep_without_consuming_attempts() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"]).with_search_error(|| {
        EnrichmentError::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        }
    });
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, clock) = harness(provider);

    let mut targets = Vec::new();
    for title in ["My Movie", "My Other Movie"] {
        let movie = movie_repo
            .find_or_create_by_identity(CreateMovie::new(title.to_string(), Some(2020), None))
            .await
            .unwrap();
        let target = EnrichmentTargetId::Movie(movie.id);
        state_repo.ensure_pending(target).await.unwrap();
        targets.push(target);
    }

    let report = service.sweep_once().await;
    assert!(report.rate_limited);
    assert_eq!(report.processed, 1, "the sweep stops at the first refusal");

    let mut rows = Vec::new();
    for target in targets {
        rows.push(state_repo.find_by_target(target).await.unwrap().unwrap());
    }
    let (attempted, untouched): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .partition(|row| row.next_attempt_at.is_some());
    let [attempted] = attempted.as_slice() else {
        panic!("exactly one title was attempted: {attempted:?}");
    };
    assert_eq!(attempted.status, EnrichmentStatus::Pending);
    assert_eq!(attempted.attempts, 0, "a refusal spends no attempt");
    assert_eq!(
        attempted.next_attempt_at,
        Some(clock.now() + chrono::Duration::seconds(30)),
        "retried when the provider's Retry-After says"
    );
    assert_eq!(attempted.updated_at, clock.now());
    assert_eq!(
        attempted.last_error.as_deref(),
        Some("rate limited by provider")
    );
    let [untouched] = untouched.as_slice() else {
        panic!("exactly one title was left alone: {untouched:?}");
    };
    assert_eq!(
        (untouched.attempts, untouched.last_error.as_deref()),
        (0, None)
    );
}

/// A refusal that names no delay waits a minute.
#[tokio::test]
async fn a_rate_limit_without_retry_after_retries_in_a_minute() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_search_error(|| EnrichmentError::RateLimited { retry_after: None });
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, clock) = harness(provider);
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("My Movie".to_string(), Some(2020), None))
        .await
        .unwrap();
    let target = EnrichmentTargetId::Movie(movie.id);
    state_repo.ensure_pending(target).await.unwrap();

    assert!(service.sweep_once().await.rate_limited);

    let row = state_repo.find_by_target(target).await.unwrap().unwrap();
    assert_eq!(
        row.next_attempt_at,
        Some(clock.now() + chrono::Duration::seconds(60))
    );
}

#[tokio::test]
async fn transient_error_retries_then_fails_after_max_attempts() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_search_error(|| EnrichmentError::Transport("boom".to_string()));
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, clock) = harness(provider);

    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("My Movie".to_string(), Some(2020), None))
        .await
        .unwrap();
    let target = EnrichmentTargetId::Movie(movie.id);
    state_repo.ensure_pending(target).await.unwrap();

    // max_attempts defaults to 5: four failures are retried, each after
    // the backoff its count has earned, stamped at the clock's now, and
    // the fifth gives up.
    let policy = EnrichmentPolicy::default();
    for attempts in 1..policy.max_attempts {
        let report = service.sweep_once().await;
        assert_eq!(report.retrying, 1, "attempt {attempts} is retried");
        let row = state_repo.find_by_target(target).await.unwrap().unwrap();
        let backoff = policy.backoff_schedule[attempts as usize - 1];
        assert_eq!(row.status, EnrichmentStatus::Pending);
        assert_eq!(row.attempts, attempts);
        assert_eq!(
            row.next_attempt_at,
            Some(clock.now() + chrono::Duration::from_std(backoff).unwrap()),
            "attempt {attempts} waits {backoff:?}"
        );
        assert_eq!(row.updated_at, clock.now());
        assert_eq!(row.last_error.as_deref(), Some("transport error: boom"));

        // Not due a moment before its backoff ends, and due at its end.
        clock.advance(backoff - Duration::from_secs(1));
        assert_eq!(service.sweep_once().await.processed, 0);
        clock.advance(Duration::from_secs(1));
    }
    let last_report = service.sweep_once().await;
    assert_eq!(last_report.failed, 1);
    let row = state_repo.find_by_target(target).await.unwrap().unwrap();
    assert_eq!(row.status, EnrichmentStatus::Failed);
    assert_eq!(row.updated_at, clock.now());

    clock.advance(Duration::from_secs(10 * 86_400));
    let due = state_repo.fetch_due(clock.now(), 25).await.unwrap();
    assert!(due.is_empty(), "a Failed row must never be picked up again");
}

#[tokio::test]
async fn refresh_reuses_matched_ref_and_invalidates_cache() {
    let ref_id = ExternalMediaRef::new("tmdb", "603");
    let enrichment = MovieEnrichment {
        tmdb_id: Some(603),
        title: "The Matrix Reloaded".to_string(),
        year: Some(2003),
        ..Default::default()
    };
    // No search result is configured: a refresh that searched instead of
    // reusing its match would find nothing and leave the title unmatched.
    let provider = Arc::new(
        InMemoryEnrichmentProvider::new(&["tmdb"])
            .with_movie_enrichment_at(ref_id.clone(), enrichment),
    );
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, clock) =
        harness_sharing(provider.clone());

    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new(
            "The Matrix Reloaded".to_string(),
            Some(2003),
            None,
        ))
        .await
        .unwrap();
    let target = EnrichmentTargetId::Movie(movie.id);
    state_repo.ensure_pending(target).await.unwrap();
    state_repo
        .mark_enriched(
            state_repo.fetch_due(clock.now(), 25).await.unwrap()[0].id,
            ref_id.as_str(),
            0.9,
            clock.now(),
        )
        .await
        .unwrap();
    state_repo.request_refresh(target, false).await.unwrap();
    assert!(provider.invalidated_refs().is_empty());

    clock.advance(Duration::from_secs(5));
    let report = service.sweep_once().await;
    assert_eq!(report.enriched, 1);
    assert_eq!(
        provider.invalidated_refs(),
        vec![ref_id.as_str().to_string()],
        "the match's cached response is dropped before it is re-fetched"
    );
    let updated = movie_repo.find_by_id(movie.id).await.unwrap().unwrap();
    assert_eq!(updated.tmdb_id, Some(603));
    let row = state_repo.find_by_target(target).await.unwrap().unwrap();
    assert_eq!(row.status, EnrichmentStatus::Enriched);
    assert_eq!(row.matched_ref.as_deref(), Some(ref_id.as_str()));
    assert_eq!(
        row.match_confidence,
        Some(0.9),
        "a refreshed match keeps the confidence it was made with"
    );
    assert!(!row.force_refresh, "the refresh is spent");
    assert_eq!(row.enriched_at, Some(clock.now()));
}

#[tokio::test]
async fn successful_show_match_applies_show_and_season_enrichment() {
    let show_ref = ExternalMediaRef::new("tmdb", "1399");
    let hit = ShowSearchHit {
        external_ref: show_ref.clone(),
        title: "Game of Thrones".to_string(),
        original_title: None,
        year: Some(2011),
        popularity: None,
        vote_average: None,
    };
    let show_enrichment = ShowEnrichment {
        tmdb_id: Some(1399),
        title: "Game of Thrones".to_string(),
        year: Some(2011),
        genres: vec!["Drama".to_string()],
        ..Default::default()
    };
    let season_enrichment = beam_domain::providers::enrichment::SeasonEnrichment {
        season_number: 1,
        poster_url: Some("https://example.com/poster.jpg".to_string()),
        ..Default::default()
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_show_search("Game of Thrones", vec![hit])
        .with_show_enrichment(show_ref.clone(), show_enrichment)
        .with_season_enrichment(show_ref, season_enrichment);

    let (service, _movie_repo, show_repo, state_repo, genre_repo, _clock) = harness(provider);

    let show = show_repo
        .find_or_create_by_identity(CreateShow::new("Game of Thrones".to_string(), Some(2011)))
        .await
        .unwrap();
    show_repo.find_or_create_season(show.id, 1).await.unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Show(show.id))
        .await
        .unwrap();

    let report = service.sweep_once().await;
    assert_eq!(report.enriched, 1);

    let updated = show_repo.find_by_id(show.id).await.unwrap().unwrap();
    assert_eq!(updated.tmdb_id, Some(1399));
    assert_eq!(genre_repo.genres_for_show(show.id).len(), 1);

    let seasons = show_repo.find_seasons_by_show_id(show.id).await.unwrap();
    assert_eq!(
        seasons[0].poster_url.as_deref(),
        Some("https://example.com/poster.jpg")
    );
}

/// Issue #267: a file numbered absolutely (`Show - 013`) is indexed as
/// that episode of season 1, or of its season folder's season, and takes
/// the metadata of the episode the provider's absolute order puts there.
#[tokio::test]
async fn an_absolutely_numbered_episode_takes_the_metadata_its_place_in_the_order_names() {
    use beam_domain::models::CreateEpisode;
    use beam_domain::providers::enrichment::{EpisodeEnrichment, SeasonEnrichment};

    let show_ref = ExternalMediaRef::new("tvdb", "81797");
    let provider_season = |season_number: u32, count: u32| SeasonEnrichment {
        season_number,
        poster_url: Some(format!("https://example.com/s{season_number}.jpg")),
        episodes: (1..=count)
            .map(|n| EpisodeEnrichment {
                episode_number: n,
                title: Some(format!("S{season_number:02}E{n:02}")),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let mut provider = InMemoryEnrichmentProvider::new(&["tvdb"])
        .with_show_search(
            "One Piece",
            vec![ShowSearchHit {
                external_ref: show_ref.clone(),
                title: "One Piece".to_string(),
                original_title: None,
                year: Some(1999),
                popularity: None,
                vote_average: None,
            }],
        )
        .with_show_enrichment(
            show_ref.clone(),
            ShowEnrichment {
                tvdb_id: Some(81797),
                title: "One Piece".to_string(),
                year: Some(1999),
                ..Default::default()
            },
        );
    for (season_number, count) in [(0, 1), (1, 12), (2, 3)] {
        provider = provider
            .with_season_enrichment(show_ref.clone(), provider_season(season_number, count));
    }
    let (service, _movies, show_repo, state_repo, _genres, _clock) = harness(provider);

    let show = show_repo
        .find_or_create_by_identity(CreateShow::new("One Piece".to_string(), Some(1999)))
        .await
        .unwrap();
    let mut local = Vec::new();
    for (season_number, episode_number) in
        [(0, 1), (1, 1), (1, 13), (1, 15), (1, 16), (2, 14), (3, 2)]
    {
        let season = show_repo
            .find_or_create_season(show.id, season_number)
            .await
            .unwrap();
        let episode = show_repo
            .find_or_create_episode(CreateEpisode {
                season_id: season.id,
                episode_number,
                title: format!("local {season_number}x{episode_number}"),
                runtime: None,
                air_date: None,
            })
            .await
            .unwrap();
        local.push(((season_number, episode_number), episode.id));
    }
    state_repo
        .ensure_pending(EnrichmentTargetId::Show(show.id))
        .await
        .unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);

    let mut titles = Vec::new();
    for (position, episode_id) in local {
        let episode = show_repo
            .find_episode_by_id(episode_id)
            .await
            .unwrap()
            .unwrap();
        titles.push((position, episode.title));
    }
    let expected = [
        // A special keeps its own number: the order counts none.
        ((0, 1), "S00E01"),
        ((1, 1), "S01E01"),
        // Past season 1's twelve: the order runs on into season 2.
        ((1, 13), "S02E01"),
        ((1, 15), "S02E03"),
        // Past the order's end: left as indexed.
        ((1, 16), "local 1x16"),
        // In a season folder, and the order agrees it is that season's.
        ((2, 14), "S02E02"),
        // The order puts 2 in season 1, not the folder's season 3.
        ((3, 2), "local 3x2"),
    ];
    let expected: Vec<((u32, u32), String)> = expected
        .into_iter()
        .map(|(position, title)| (position, title.to_string()))
        .collect();
    assert_eq!(titles, expected);

    // Season 3 has nothing from the provider, so its row is left alone.
    let seasons = show_repo.find_seasons_by_show_id(show.id).await.unwrap();
    let poster = |number: u32| {
        seasons
            .iter()
            .find(|season| season.season_number == number)
            .and_then(|season| season.poster_url.clone())
    };
    assert_eq!(poster(1).as_deref(), Some("https://example.com/s1.jpg"));
    assert_eq!(poster(3), None);
}

#[tokio::test]
async fn missing_target_row_marks_failed() {
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) =
        harness(InMemoryEnrichmentProvider::new(&["tmdb"]));

    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("Ghost".to_string(), None, None))
        .await
        .unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();
    // Simulate the movie being deleted out from under the queue row.
    movie_repo.movies.lock().unwrap().remove(&movie.id);

    let report = service.sweep_once().await;
    assert_eq!(report.failed, 1);
}

#[test]
fn backoff_schedule_uses_last_entry_beyond_its_length() {
    let policy = EnrichmentPolicy::default();
    assert_eq!(policy.backoff_for(1), Duration::from_secs(60));
    assert_eq!(policy.backoff_for(5), Duration::from_secs(86400));
    assert_eq!(policy.backoff_for(99), Duration::from_secs(86400));
}

/// The sweep's report tallies flow into
/// `beam_enrichment_outcomes_total{outcome}` counters. Asserted through a
/// thread-local recorder (never a global install, which would collide
/// across parallel tests): the async sweep runs on a current-thread
/// runtime *inside* the local-recorder scope so every sample is captured.
#[test]
fn sweep_outcomes_are_counted_as_metrics() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    metrics::with_local_recorder(&recorder, || {
        rt.block_on(async {
            // One row that enriches (exact title+year match) and one that
            // fails (target movie deleted out from under the queue row).
            let hit = MovieSearchHit {
                external_ref: ExternalMediaRef::new("tmdb", "603"),
                title: "The Matrix".to_string(),
                original_title: None,
                year: Some(1999),
                popularity: Some(80.0),
                vote_average: Some(8.7),
            };
            let enrichment = MovieEnrichment {
                tmdb_id: Some(603),
                title: "The Matrix".to_string(),
                year: Some(1999),
                ..Default::default()
            };
            let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
                .with_movie_search("The Matrix", vec![hit])
                .with_movie_enrichment(enrichment);
            let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) =
                harness(provider);

            let matched = movie_repo
                .find_or_create_by_identity(CreateMovie::new(
                    "The Matrix".to_string(),
                    Some(1999),
                    None,
                ))
                .await
                .unwrap();
            state_repo
                .ensure_pending(EnrichmentTargetId::Movie(matched.id))
                .await
                .unwrap();

            let doomed = movie_repo
                .find_or_create_by_identity(CreateMovie::new("Ghost".to_string(), None, None))
                .await
                .unwrap();
            state_repo
                .ensure_pending(EnrichmentTargetId::Movie(doomed.id))
                .await
                .unwrap();
            movie_repo.movies.lock().unwrap().remove(&doomed.id);

            let report = service.sweep_once().await;
            assert_eq!(report.enriched, 1);
            assert_eq!(report.failed, 1);
        })
    });

    let snapshot = snapshotter.snapshot().into_vec();
    let outcome_count = |outcome: &str| -> Option<u64> {
        snapshot.iter().find_map(|(key, _, _, value)| {
            let key = key.key();
            if key.name() != "beam_enrichment_outcomes_total" {
                return None;
            }
            if !key
                .labels()
                .any(|l| l.key() == "outcome" && l.value() == outcome)
            {
                return None;
            }
            match value {
                DebugValue::Counter(v) => Some(*v),
                _ => None,
            }
        })
    };

    assert_eq!(outcome_count("enriched"), Some(1), "snapshot: {snapshot:?}");
    assert_eq!(outcome_count("failed"), Some(1), "snapshot: {snapshot:?}");
    // Zero tallies must not mint a series at all.
    assert_eq!(outcome_count("unmatched"), None);
    assert_eq!(outcome_count("rate_limited"), None);
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

#[tokio::test]
async fn a_pinned_movie_is_fetched_by_its_pin_and_never_searched_for() {
    // No search result exists for the title at all: only a fetch by the
    // pin can enrich it.
    let provider =
        InMemoryEnrichmentProvider::new(&["tmdb"]).with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            ..Default::default()
        });
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) = harness(provider);
    let id = pinned_movie(&movie_repo, &state_repo, "matrix", ProviderPin::Tmdb(603)).await;

    let report = service.sweep_once().await;

    assert_eq!(report.enriched, 1, "{report:?}");
    let movie = movie_repo.find_by_id(id).await.unwrap().unwrap();
    assert_eq!(movie.tmdb_id, Some(603));
    assert_eq!(movie.title, "The Matrix");
    assert_eq!(movie.pinned_ref.as_deref(), Some("tmdb:603"));
}

#[tokio::test]
async fn a_match_an_administrator_set_wins_over_the_pin() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "Pinned".to_string(),
            ..Default::default()
        })
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(604),
            title: "Chosen".to_string(),
            ..Default::default()
        });
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, clock) = harness(provider);
    let id = pinned_movie(&movie_repo, &state_repo, "matrix", ProviderPin::Tmdb(603)).await;
    let row = state_repo.fetch_due(clock.now(), 25).await.unwrap()[0].id;
    state_repo
        .mark_enriched(row, "tmdb:604", 1.0, clock.now())
        .await
        .unwrap();
    state_repo
        .request_refresh(EnrichmentTargetId::Movie(id), false)
        .await
        .unwrap();

    service.sweep_once().await;

    assert_eq!(
        movie_repo.find_by_id(id).await.unwrap().unwrap().title,
        "Chosen"
    );
}

#[tokio::test]
async fn an_imdb_pin_rejects_a_search_match_with_another_imdb_id() {
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "949"),
        title: "Heat".to_string(),
        original_title: None,
        year: Some(1995),
        popularity: None,
        vote_average: None,
    };
    let enrichment = |imdb: &str| MovieEnrichment {
        tmdb_id: Some(949),
        imdb_id: Some(imdb.to_string()),
        title: "Heat".to_string(),
        year: Some(1995),
        ..Default::default()
    };

    for (pinned, matched, enriched) in [
        ("tt0113277", "tt0113277", true),
        ("tt0000001", "tt0113277", false),
    ] {
        let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
            .with_movie_search("Heat", vec![hit.clone()])
            .with_movie_enrichment(enrichment(matched));
        let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) = harness(provider);
        let id = pinned_movie(
            &movie_repo,
            &state_repo,
            "Heat",
            ProviderPin::Imdb(pinned.to_string()),
        )
        .await;

        let report = service.sweep_once().await;

        let movie = movie_repo.find_by_id(id).await.unwrap().unwrap();
        if enriched {
            assert_eq!(report.enriched, 1, "a match the pin agrees with");
            assert_eq!(movie.tmdb_id, Some(949));
        } else {
            assert_eq!(report.unmatched, 1, "a match the pin contradicts");
            assert_eq!(movie.tmdb_id, None, "nothing of it is applied");
        }
    }
}

#[tokio::test]
async fn a_pin_no_configured_provider_resolves_is_only_checked() {
    // TMDB is pinned but only AniList is configured: the title is
    // searched, and an AniList match carries no TMDB id to contradict.
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("anilist", "5114"),
        title: "Akira".to_string(),
        original_title: None,
        year: Some(1988),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["anilist"])
        .with_movie_search("Akira", vec![hit])
        .with_movie_enrichment(MovieEnrichment {
            anilist_id: Some(5114),
            title: "Akira".to_string(),
            year: Some(1988),
            ..Default::default()
        });
    let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) = harness(provider);
    let id = pinned_movie(&movie_repo, &state_repo, "Akira", ProviderPin::Tmdb(149)).await;

    assert_eq!(service.sweep_once().await.enriched, 1);
    assert_eq!(
        movie_repo.find_by_id(id).await.unwrap().unwrap().anilist_id,
        Some(5114)
    );
}

#[tokio::test]
async fn a_pinned_show_is_fetched_by_its_pin() {
    let show_ref = ExternalMediaRef::new("tmdb", "1399");
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"]).with_show_enrichment(
        show_ref,
        ShowEnrichment {
            tmdb_id: Some(1399),
            title: "Game of Thrones".to_string(),
            ..Default::default()
        },
    );
    let (service, _movie_repo, show_repo, state_repo, _genre_repo, _clock) = harness(provider);
    let show = show_repo
        .find_or_create_by_identity(CreateShow::new("GoT".to_string(), None))
        .await
        .unwrap();
    assert!(
        show_repo
            .set_pinned_ref(
                show.id,
                &ProviderPin::Tmdb(1399),
                beam_domain::models::PinSource::Nfo
            )
            .await
            .unwrap()
    );
    state_repo
        .ensure_pending(EnrichmentTargetId::Show(show.id))
        .await
        .unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    assert_eq!(
        show_repo.find_by_id(show.id).await.unwrap().unwrap().title,
        "Game of Thrones"
    );
}

#[test]
fn a_match_contradicts_a_pin_only_with_another_id_of_the_pins_provider() {
    let ids = |tmdb, imdb, anilist, tvdb| MatchIds {
        tmdb,
        imdb,
        anilist,
        tvdb,
    };
    let tmdb = ProviderPin::Tmdb(603);
    assert_eq!(contradiction(&tmdb, ids(Some(603), None, None, None)), None);
    assert!(contradiction(&tmdb, ids(Some(604), None, None, None)).is_some());
    assert_eq!(
        contradiction(&tmdb, ids(None, Some("tt1"), Some(1), Some(1))),
        None,
        "a match with no TMDB id cannot contradict a TMDB pin"
    );
    let anilist = ProviderPin::Anilist(5);
    assert!(contradiction(&anilist, ids(None, None, Some(6), None)).is_some());
    assert_eq!(
        contradiction(&anilist, ids(Some(1), None, Some(5), None)),
        None
    );
    let imdb = ProviderPin::Imdb("tt0000001".to_string());
    assert!(contradiction(&imdb, ids(None, Some("tt0000002"), None, None)).is_some());
    assert_eq!(
        contradiction(&imdb, ids(Some(1), Some("tt0000001"), None, None)),
        None
    );
    // TheTVDB reports its id now (issue #267), so a TheTVDB pin is
    // contradicted by a match with another one, and only by that.
    let tvdb = ProviderPin::Tvdb(81189);
    assert_eq!(
        contradiction(&tvdb, ids(Some(1), Some("tt0000001"), Some(1), None)),
        None
    );
    assert_eq!(
        contradiction(&tvdb, ids(None, None, None, Some(81189))),
        None
    );
    assert_eq!(
        contradiction(&tvdb, ids(None, None, None, Some(81190))).as_deref(),
        Some("the NFO pins tvdb:81189, but the best match found is tvdb:81190")
    );
}
