use super::*;

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
