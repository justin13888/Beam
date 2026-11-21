use super::*;

// ─── backfilling titles from before identity keys ────────────────────────────

#[tokio::test]
async fn a_legacy_title_is_keyed_from_its_files_not_its_enriched_title() {
    let h = Harness::keeping_missing_files().await;
    let movie = h
        .legacy_movie(
            "Amélie: Provider Title",
            Some(2001),
            &["Amelie.2001.1080p.mkv", "Amelie (2001)/Amelie (2001).mkv"],
        )
        .await;
    let show = h
        .legacy_show(
            "Severance: Provider Title",
            &["Severance/Severance.S01E01.mkv"],
        )
        .await;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.keyed, 2);
    let movie = h.movie_repo.find_by_id(movie).await.unwrap().unwrap();
    assert_eq!(
        movie.identity_key,
        Some(title_identity_key("Amelie", Some(2001))),
        "the files' parse, not the stored display title"
    );
    let show = h.show_repo.find_by_id(show).await.unwrap().unwrap();
    assert_eq!(
        show.identity_key,
        Some(title_identity_key("Severance", None))
    );
    assert_eq!(h.backfill_warning().await, None, "nothing to warn about");
}

#[tokio::test]
async fn a_legacy_title_with_no_file_is_keyed_from_its_stored_title() {
    let h = Harness::keeping_missing_files().await;
    let movie = h.legacy_movie("Ghost", Some(1990), &[]).await;
    let show = h.legacy_show("Severance", &[]).await;

    h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(
        h.movie_repo
            .find_by_id(movie)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Ghost", Some(1990)))
    );
    assert_eq!(
        h.show_repo
            .find_by_id(show)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Severance", None)),
        "no file row names it a husk"
    );
}

#[tokio::test]
async fn a_legacy_title_whose_files_are_all_missing_is_keyed_from_those_files() {
    let h = Harness::keeping_missing_files().await;
    let movie = h
        .legacy_movie(
            "Amélie: Provider Title",
            Some(2001),
            &["Amelie.2001.1080p.mkv"],
        )
        .await;
    let show = h
        .legacy_show(
            "Severance: Provider Title",
            &["Severance/Severance.S01E01.mkv"],
        )
        .await;
    // Upgraded while the volume was away: every file is only soft-deleted.
    h.mark_every_file_missing().await;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.keyed, 2);
    assert_eq!(
        h.movie_repo
            .find_by_id(movie)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Amelie", Some(2001))),
        "a missing file still names its title; the enriched title does not"
    );
    assert_eq!(
        h.show_repo
            .find_by_id(show)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Severance", None))
    );
}

#[tokio::test]
async fn of_legacy_duplicates_the_oldest_takes_the_key() {
    let h = Harness::keeping_missing_files().await;
    let base = chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap();
    let qualities = ["480p", "576p", "720p", "1080p", "1440p", "2160p"];
    // Duplicates the old title lookup created, one per file, inserted newest
    // first so neither insertion order nor chance hands the original the key.
    let mut created = Vec::new();
    for (age, quality) in qualities.iter().enumerate().rev() {
        let id = h
            .legacy_movie_created_at(
                "Amelie",
                Some(2001),
                &[&format!("Amelie.2001.{quality}.mkv")],
                base + chrono::Duration::days(age as i64),
            )
            .await;
        created.push((age, id));
    }
    created.sort();
    let original = created[0].1;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.keyed, 1);
    assert_eq!(
        h.movie_repo
            .find_by_id(original)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        Some(title_identity_key("Amelie", Some(2001))),
        "the original, which enrichment and progress most likely hang off"
    );
    let later: Vec<Uuid> = created[1..].iter().map(|(_, id)| *id).collect();
    let warning = h.backfill_warning().await.expect("the admin is told");
    let mut reported: Vec<Uuid> =
        serde_json::from_value(warning["duplicate_movies"].clone()).unwrap();
    reported.sort();
    let mut expected = later;
    expected.sort();
    assert_eq!(reported, expected, "every later duplicate is named");
}

#[tokio::test]
async fn a_failed_backfill_does_not_hold_up_the_scan_and_is_retried() {
    use beam_domain::repositories::movie::MockMovieRepository;

    let dir = TempDir::new().unwrap();
    let library_repo = Arc::new(InMemoryLibraryRepository::default());
    let library = library_repo
        .create(CreateLibrary {
            name: "Backfill".to_string(),
            root_path: dir.path().to_path_buf(),
            description: None,
        })
        .await
        .unwrap();
    // The database refuses the backfill's first read, then recovers. The
    // strict counts are the point: the second scan retries, the third does
    // not backfill again.
    let mut movie_repo = MockMovieRepository::new();
    let mut attempts = mockall::Sequence::new();
    movie_repo
        .expect_find_unkeyed()
        .times(1)
        .in_sequence(&mut attempts)
        .returning(|| Err(DbErr::Custom("connection reset".to_string())));
    movie_repo
        .expect_find_unkeyed()
        .times(1)
        .in_sequence(&mut attempts)
        .returning(|| Ok(Vec::new()));
    movie_repo
        .expect_find_keyed_before_version()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    movie_repo.expect_delete_orphaned().returning(|_| Ok(0));
    let service = LocalIndexService::new(
        library_repo.clone(),
        Arc::new(InMemoryFileRepository::default()),
        Arc::new(movie_repo),
        Arc::new(InMemoryShowRepository::default()),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(InMemoryAdminLogRepository::default()) as Arc<dyn AdminLogRepository>,
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    );

    service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert!(
        library_repo
            .find_by_id(library.id)
            .await
            .unwrap()
            .unwrap()
            .last_scan_finished_at
            .is_some(),
        "the scan ran despite the failed backfill"
    );
    assert!(
        !service.identity_passes_succeeded.load(Ordering::SeqCst),
        "a failed backfill is not recorded as done"
    );

    service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert!(
        service.identity_passes_succeeded.load(Ordering::SeqCst),
        "retried"
    );

    service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
}

#[tokio::test]
async fn legacy_titles_that_cannot_be_keyed_are_left_keyless_and_reported() {
    let h = Harness::keeping_missing_files().await;
    // Two films the old title lookup merged under one row.
    let merged = h
        .legacy_movie("Dune", None, &["Dune.1984.mkv", "Dune.2021.mkv"])
        .await;
    // The duplicate the old lookup created beside a title that is keyed now.
    let keyed = h
        .movie_repo
        .find_or_create_by_identity(CreateMovie::new("Amelie", Some(2001), None))
        .await
        .unwrap();
    let duplicate = h
        .legacy_movie("Amelie", Some(2001), &["Amelie.2001.720p.mkv"])
        .await;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.keyed, 0);
    for id in [merged, duplicate] {
        assert_eq!(
            h.movie_repo
                .find_by_id(id)
                .await
                .unwrap()
                .unwrap()
                .identity_key,
            None
        );
    }
    assert_eq!(
        h.movie_repo
            .find_by_id(keyed.id)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        keyed.identity_key,
        "the keyed title keeps its key"
    );
    let warning = h.backfill_warning().await.expect("the admin is told");
    assert_eq!(warning["ambiguous_movies"], serde_json::json!([merged]));
    assert_eq!(warning["duplicate_movies"], serde_json::json!([duplicate]));
}

#[tokio::test]
async fn scanning_every_library_backfills_first_so_a_legacy_title_takes_its_next_file() {
    let h = Harness::keeping_missing_files().await;
    let legacy = h
        .legacy_movie(
            "The Matrix: Provider Cut",
            Some(1999),
            &["The.Matrix.1999.1080p.mkv"],
        )
        .await;
    h.write("The.Matrix.1999.2160p.mkv");

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    assert_eq!(
        h.only_movie().id,
        legacy,
        "no second movie for the new file"
    );
    assert_eq!(
        h.movie_repo
            .find_entries_by_movie_id(legacy)
            .await
            .unwrap()
            .len(),
        1,
        "the new copy joins the legacy entry"
    );
    // The library list the scan walked is the harness's one library.
    assert_eq!(h.library_repo.find_all().await.unwrap().len(), 1);
}

/// Upgrading straight from a build before issue #183: a show named after a
/// season folder has no key yet. The backfill must leave it keyless -- its
/// files derive the real series' key, and a husk holding that key would
/// capture every file of the series when the scan reclassifies them -- so
/// the series gets its own show and the husk is retired. A husk a provider
/// has matched since -- its display title now the provider's -- is still one.
#[tokio::test]
async fn an_unkeyed_season_folder_husk_is_retired_not_given_its_series_key() {
    for enriched in [false, true] {
        an_unkeyed_husk_is_retired(enriched).await;
    }
}

async fn an_unkeyed_husk_is_retired(enriched: bool) {
    let h = Harness::purging_at_once().await;
    let husk = h
        .legacy_show(
            "Season 05",
            &[
                "Show X/Season 05/Show.X.S05E01.mkv",
                "Show X/Season 05/Show.X.S05E02.mkv",
            ],
        )
        .await;
    if enriched {
        h.enrich_show(husk, 999).await;
    }
    h.write("Show X/Season 01/Show.X.S01E01.mkv");

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let show = h.only_show();
    assert_ne!(show.id, husk, "the husk is retired, not adopted");
    assert_eq!(show.title, "Show X");
    assert_eq!(show.identity_key.as_deref(), Some("show x|"));
    assert_eq!(show.tmdb_id, None, "the husk's match does not carry over");
    let files = h.file_repo.files.lock().unwrap().clone();
    assert_eq!(files.len(), 3);
    for file in files.values() {
        let Some(MediaFileContent::Episode { episode_id, .. }) = file.content else {
            panic!("{} is not an episode file", file.path.display());
        };
        let episode = h.show_repo.episodes.lock().unwrap()[&episode_id].clone();
        let season = h.show_repo.seasons.lock().unwrap()[&episode.season_id].clone();
        assert_eq!(season.show_id, show.id, "{}", file.path.display());
    }
    assert!(
        h.backfill_warning().await.is_none(),
        "a husk left keyless is not a title the administrator must sort out"
    );
}

/// A keyless show whose files a legacy title lookup gathered from a season
/// folder and from a series folder is not a husk: releasing it would drop a
/// title real files were attached to by name. Its files name two shows, so
/// it is left keyless and reported like any title whose files disagree.
#[tokio::test]
async fn a_show_only_some_of_whose_files_sit_in_a_season_folder_is_not_a_husk() {
    let h = Harness::keeping_missing_files().await;
    let show = h
        .legacy_show(
            "The Office",
            &[
                "Show X/Season 01/Show.X.S01E01.mkv",
                "The Office/The.Office.S01E01.mkv",
            ],
        )
        .await;
    h.enrich_show(show, 2316).await;

    let report = h.service.backfill_identity_keys().await.unwrap();

    assert_eq!(report.ambiguous_shows, vec![show], "not released as a husk");
    let warning = h.backfill_warning().await.expect("the admin is told");
    assert_eq!(warning["ambiguous_shows"], serde_json::json!([show]));
    assert_eq!(
        h.show_repo
            .find_by_id(show)
            .await
            .unwrap()
            .unwrap()
            .identity_key,
        None
    );
}
