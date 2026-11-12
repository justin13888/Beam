use super::*;

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
