use super::*;

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
