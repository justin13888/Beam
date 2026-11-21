use super::*;

/// Two movies providers matched to different entries are two films, however
/// the current rules read their names (C3 of the #233 review): the stale one
/// is not merged into the one holding the key it now derives. Each keeps its
/// key, its match and its file -- which a scan leaves where it is rather than
/// moving to the holder -- and the administrator is told.
#[tokio::test]
async fn a_rekey_never_merges_movies_matched_to_different_entries() {
    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let whole = "Movie (2019)/Movie (2019).mkv";
    let part = "Movie (2019)/Movie (2019) - CD2.avi";
    let holder = h
        .keyed_movie("Movie", Some(2019), "movie|2019", &[whole], base)
        .await;
    assert!(
        h.movie_repo
            .rekey(holder, Some("movie|2019".to_string()), CLASSIFIER_VERSION)
            .await
            .unwrap()
    );
    h.enrich_movie(holder, 2019, 1).await;
    let stale = h
        .keyed_movie(
            "Movie - CD2",
            Some(2019),
            "movie cd2|2019",
            &[part],
            base + chrono::Duration::days(1),
        )
        .await;
    h.enrich_movie(stale, 2019, 2).await;

    // Twice: the second process start -- which runs the identity passes
    // again -- finds the pair as the first left it, its files already held.
    for start in 0..2 {
        if start > 0 {
            h.service
                .identity_passes_succeeded
                .store(false, Ordering::SeqCst);
        }
        h.service
            .scan_all_libraries(ScanTrigger::Periodic)
            .await
            .unwrap();

        let movie = |id| h.movie_repo.movies.lock().unwrap()[&id].clone();
        assert_eq!(
            (movie(holder).identity_key, movie(holder).tmdb_id),
            (Some("movie|2019".to_string()), Some(1))
        );
        assert_eq!(
            (movie(stale).identity_key, movie(stale).tmdb_id),
            (Some("movie cd2|2019".to_string()), Some(2)),
            "the stale movie keeps its key and its match"
        );
        for (id, rel) in [(holder, whole), (stale, part)] {
            let entries = h.movie_repo.find_entries_by_movie_id(id).await.unwrap();
            assert_eq!(entries.len(), 1, "{rel}");
            let files: Vec<(std::path::PathBuf, Option<u32>)> = h
                .file_repo
                .find_by_movie_entry_id(entries[0].id)
                .await
                .unwrap()
                .into_iter()
                .map(|f| match f.content {
                    Some(MediaFileContent::Movie { part_number, .. }) => (f.path, part_number),
                    other => panic!("not a movie file: {other:?}"),
                })
                .collect();
            assert_eq!(files, vec![(h.root.join(rel), None)], "{rel} stays put");
        }
        assert_eq!(h.file_version(part), CLASSIFIER_VERSION);
    }

    let warnings: Vec<serde_json::Value> = h
        .admin_log_repo
        .list(100, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|l| l.message.contains("different provider entries"))
        .filter_map(|l| l.details)
        .map(|details| details["conflicting_movies"].clone())
        .collect();
    assert_eq!(
        warnings,
        vec![serde_json::json!([{ "stale": stale, "holder": holder }]); 2],
        "each process start re-examines the pair and tells the administrator again"
    );
    assert!(
        h.admin_log_details("identity keys of").await.is_none(),
        "nothing was rekeyed or merged"
    );
}

/// What an administrator set on a title a merge retires survives it (issue
/// #185): its pin moves to the survivor, which is queued to be fetched by
/// it, and its field locks join the survivor's. Of two titles an
/// administrator pinned to different ids, the survivor keeps its own pin and
/// the administrator is told which went.
#[tokio::test]
async fn a_merge_keeps_what_an_administrator_set_on_the_retired_title() {
    use beam_domain::models::enrichment::{EnrichmentStatus, MetadataField};
    use beam_domain::repositories::EnrichmentStateRepository;

    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    // Both matched to one entry, so they merge and the older survives --
    // and the newer carries the pin. (Matched to different entries, they
    // would be kept apart: see
    // `a_rekey_never_merges_movies_matched_to_different_entries`.)
    let older = h
        .keyed_movie(
            "Ocean's Eleven",
            Some(2001),
            "ocean s eleven|2001",
            &["Ocean's Eleven (2001)/Ocean's Eleven (2001).mkv"],
            base,
        )
        .await;
    let newer = h
        .keyed_movie(
            "Oceans Eleven",
            Some(2001),
            "oceans eleven|2001",
            &["Oceans.Eleven.2001.1080p.mkv"],
            base + chrono::Duration::days(1),
        )
        .await;
    h.enrich_movie(older, 2001, 1).await;
    h.enrich_movie(newer, 2001, 1).await;
    assert!(
        h.movie_repo
            .set_pinned_ref(newer, &ProviderPin::Tmdb(161), PinSource::Admin)
            .await
            .unwrap()
    );
    let locks = |fields: &[MetadataField]| fields.iter().copied().collect::<FieldLocks>();
    h.enrichment_repo
        .set_locked_fields(
            EnrichmentTargetId::Movie(newer),
            &locks(&[MetadataField::Title, MetadataField::Poster]),
        )
        .await
        .unwrap();
    h.enrichment_repo
        .set_locked_fields(
            EnrichmentTargetId::Movie(older),
            &locks(&[MetadataField::Genres]),
        )
        .await
        .unwrap();

    let scene = h
        .keyed_show(
            "Greys Anatomy",
            "greys anatomy|",
            &["Greys.Anatomy.S01E01.720p.mkv"],
            base,
        )
        .await;
    let folder = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &["Grey's Anatomy/Season 1/Greys.Anatomy.S01E02.mkv"],
            base + chrono::Duration::days(1),
        )
        .await;
    h.enrich_show(scene, 1416).await;
    h.enrich_show(folder, 1416).await;
    for (show, pin) in [(scene, 1416), (folder, 1417)] {
        assert!(
            h.show_repo
                .set_pinned_ref(show, &ProviderPin::Tmdb(pin), PinSource::Admin)
                .await
                .unwrap()
        );
    }

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let movie = h.only_movie();
    assert_eq!(movie.id, older, "the survivor is chosen as before");
    assert_eq!(
        (movie.pinned_ref.as_deref(), movie.pin_source),
        (Some("tmdb:161"), Some(PinSource::Admin)),
        "the retired title's administrator's pin moved to it"
    );
    let row = h
        .enrichment_repo
        .find_by_target(EnrichmentTargetId::Movie(older))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.locked_fields,
        locks(&[
            MetadataField::Title,
            MetadataField::Poster,
            MetadataField::Genres
        ]),
        "both titles' locks"
    );
    assert_eq!(row.status, EnrichmentStatus::Pending);
    assert_eq!(row.matched_ref, None, "fetched by the carried pin");

    let show = h.only_show();
    assert_eq!(show.id, scene);
    assert_eq!(
        (show.pinned_ref.as_deref(), show.pin_source),
        (Some("tmdb:1416"), Some(PinSource::Admin)),
        "the survivor keeps its own administrator's pin"
    );
    let conflict = h
        .admin_log_details("pinned to different ids")
        .await
        .expect("the administrator is told");
    assert_eq!(conflict["kept"], serde_json::json!(scene));
    assert_eq!(conflict["retired"], serde_json::json!(folder));
    assert_eq!(conflict["dropped_pin"], "tmdb:1417");
}

/// If another writer takes the key between the merge releasing the loser and
/// keying the survivor, the merge stops there: no file is moved onto a
/// survivor left on its stale key.
#[tokio::test]
async fn a_merge_whose_survivor_loses_the_key_moves_no_file() {
    use beam_domain::repositories::movie::MockMovieRepository;

    let h = Harness::keeping_missing_files().await;
    let stale = h
        .keyed_movie(
            "Ocean's Eleven",
            Some(2001),
            "ocean s eleven|2001",
            &["Ocean's Eleven (2001)/Ocean's Eleven (2001).mkv"],
            chrono::Utc::now() - chrono::Duration::days(2),
        )
        .await;
    let holder = h
        .keyed_movie(
            "Oceans Eleven",
            Some(2001),
            "oceans eleven|2001",
            &["Oceans.Eleven.2001.1080p.mkv"],
            chrono::Utc::now() - chrono::Duration::days(1),
        )
        .await;
    let stale_row = h.movie_repo.find_by_id(stale).await.unwrap().unwrap();
    let holder_row = h.movie_repo.find_by_id(holder).await.unwrap().unwrap();
    let stale_entries = h.movie_repo.find_entries_by_movie_id(stale).await.unwrap();
    let files_before = h.file_repo.files.lock().unwrap().clone();

    // The movie repository as the pass sees it when a concurrent writer
    // takes `oceans eleven|2001` after the loser is released.
    let mut movies = MockMovieRepository::new();
    movies.expect_find_unkeyed().returning(|| Ok(Vec::new()));
    let listed = stale_row.clone();
    movies
        .expect_find_keyed_before_version()
        .returning(move |_| Ok(vec![listed.clone()]));
    movies
        .expect_find_by_id()
        .withf(move |id| *id == stale)
        .returning(move |_| Ok(Some(stale_row.clone())));
    movies
        .expect_find_entries_by_movie_id()
        .withf(move |id| *id == stale)
        .times(1)
        .returning(move |_| Ok(stale_entries.clone()));
    movies
        .expect_find_entries_by_movie_id()
        .withf(move |id| *id != stale)
        .never();
    movies
        .expect_find_by_identity_key()
        .withf(|key| key == "oceans eleven|2001")
        .returning(move |_| Ok(Some(holder_row.clone())));
    movies
        .expect_rekey()
        .withf(|_, key, _| key.is_none())
        .times(1)
        .returning(|_, _, _| Ok(true));
    movies
        .expect_rekey()
        .withf(|_, key, _| key.is_some())
        .times(1)
        .returning(|_, _, _| Ok(false));
    movies.expect_find_or_create_entry().never();
    movies.expect_ensure_library_association().never();
    let service = LocalIndexService::new(
        h.library_repo.clone(),
        h.file_repo.clone(),
        Arc::new(movies),
        h.show_repo.clone(),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        h.admin_log_repo.clone() as Arc<dyn AdminLogRepository>,
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    );

    let report = service.rekey_stale_titles().await.unwrap();

    assert!(report.merged_movies.is_empty(), "{report:?}");
    let files_after = h.file_repo.files.lock().unwrap().clone();
    for (id, before) in &files_before {
        assert_eq!(
            format!("{:?}", files_after[id].content),
            format!("{:?}", before.content),
            "{}",
            before.path.display()
        );
    }
}

/// A title whose every file is away at upgrade is rekeyed from those
/// files, so it is still found when they come back.
#[tokio::test]
async fn a_title_whose_files_are_all_missing_is_rederived_from_them() {
    let h = Harness::keeping_missing_files().await;
    let rel = "Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv";
    let show = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &[rel],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;
    h.mark_every_file_missing().await;

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(
        h.show_repo
            .find_by_id(show)
            .await
            .unwrap()
            .unwrap()
            .identity_key
            .as_deref(),
        Some("greys anatomy|")
    );

    // Back again: it is still the same show.
    h.write(rel);
    h.scan().await;
    assert_eq!(h.only_show().id, show);
}

/// A husk a build between #183 and #182 keyed (`season 05|`) is released,
/// not handed its series' key: the series gets its own show and the husk is
/// retired, as for a keyless husk. It is known by its key, so a husk a
/// provider has matched since -- its display title now the provider's -- is
/// still released, and its match does not pass to the series.
#[tokio::test]
async fn a_keyed_season_folder_husk_is_released_not_rekeyed() {
    for enriched in [false, true] {
        a_keyed_husk_is_released(enriched).await;
    }
}

async fn a_keyed_husk_is_released(enriched: bool) {
    let h = Harness::keeping_missing_files().await;
    let husk = h
        .keyed_show(
            "Season 05",
            "season 05|",
            &["Show X/Season 05/Show.X.S05E01.mkv"],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;
    if enriched {
        h.enrich_show(husk, 999).await;
    }

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let show = h.only_show();
    assert_ne!(show.id, husk, "enriched: {enriched}");
    assert_eq!(show.title, "Show X");
    assert_eq!(show.identity_key.as_deref(), Some("show x|"));
    assert_eq!(show.tmdb_id, None, "the husk's match does not carry over");
}

/// A title whose files now name two titles keeps its old key -- there is no
/// one key to give it -- and the administrator is told.
#[tokio::test]
async fn a_title_whose_files_name_two_titles_keeps_its_key_and_is_reported() {
    let h = Harness::keeping_missing_files().await;
    let merged = h
        .keyed_movie(
            "Dune",
            None,
            "dune|",
            &["Dune.1984.mkv", "Dune.2021.mkv"],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;

    h.service.rekey_stale_titles().await.unwrap();

    assert_eq!(
        h.movie_repo
            .find_by_id(merged)
            .await
            .unwrap()
            .unwrap()
            .identity_key
            .as_deref(),
        Some("dune|")
    );
    let warning = h
        .admin_log_details("older naming rules")
        .await
        .expect("the administrator is told");
    assert_eq!(warning["ambiguous_movies"], serde_json::json!([merged]));
}

// ─── reclassification waits for the identity passes (issue #182) ────────────

const GREYS: &str = "Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv";

/// A show a build between #183 and #182 keyed `grey s anatomy|`, which a
/// provider has matched since. Its file derives `greys anatomy|` now.
async fn stale_enriched_greys(h: &Harness) -> Uuid {
    let show = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &[GREYS],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;
    h.enrich_show(show, 1416).await;
    show
}

/// The stale show is untouched: reclassification was held.
fn assert_held(h: &Harness, show: Uuid) {
    let after = h.only_show();
    assert_eq!(after.id, show, "the stale show is not retired");
    assert_eq!(after.tmdb_id, Some(1416), "it keeps its enrichment");
    assert_eq!(after.identity_key.as_deref(), Some("grey s anatomy|"));
    assert_eq!(
        h.season_one(show),
        vec![(1, vec![GREYS.to_string()])],
        "it keeps its file"
    );
    assert_eq!(h.file_version(GREYS), 0, "left for a later scan");
}

/// While the identity passes fail, no entry point -- the scan of every
/// library, the administrator's scan of one, a watcher event --
/// reclassifies a file older rules classified: moving it would find no title
/// by its new key, create one, and retire the stale title with its
/// enrichment. The first pass to succeed rekeys the title in place, and the
/// scan then reclassifies the file onto it.
#[tokio::test]
async fn a_failed_rekey_pass_holds_reclassification_until_a_pass_succeeds() {
    use beam_domain::repositories::movie::MockMovieRepository;

    let mut movies = MockMovieRepository::new();
    let mut attempts = mockall::Sequence::new();
    movies.expect_find_unkeyed().returning(|| Ok(Vec::new()));
    movies
        .expect_find_keyed_before_version()
        .times(3)
        .in_sequence(&mut attempts)
        .returning(|_| Err(DbErr::Custom("connection reset".to_string())));
    movies
        .expect_find_keyed_before_version()
        .times(1)
        .in_sequence(&mut attempts)
        .returning(|_| Ok(Vec::new()));
    movies.expect_delete_orphaned().returning(|_| Ok(0));
    let h = Harness::purging_at_once_with_movies(Arc::new(movies)).await;
    let show = stale_enriched_greys(&h).await;

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_held(&h, show);
    h.scan().await;
    assert_held(&h, show);
    h.service
        .reconcile_path(h.library.id, h.root.join(GREYS), FsEventKind::Modified)
        .await
        .unwrap();
    assert_held(&h, show);
    let warning = h
        .admin_log_details("current naming rules")
        .await
        .expect("the administrator is told");
    assert_eq!(warning["pass"], "re-derivation");

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let after = h.only_show();
    assert_eq!(after.id, show, "the same show");
    assert_eq!(after.tmdb_id, Some(1416), "with its enrichment");
    assert_eq!(after.identity_key.as_deref(), Some("greys anatomy|"));
    assert_eq!(h.season_one(show), vec![(1, vec![GREYS.to_string()])]);
    assert_eq!(
        h.file_version(GREYS),
        CLASSIFIER_VERSION,
        "now reclassified"
    );
}

/// A watcher event for a file with nothing to reclassify -- classified by
/// the current rules, or never probed -- does not ask for the identity
/// passes, so a failing pass is not retried, and reported again, on every
/// event. The strict count is the point: the scan's attempt is the only one.
#[tokio::test]
async fn a_watcher_event_with_nothing_to_reclassify_does_not_retry_the_passes() {
    use beam_domain::repositories::movie::MockMovieRepository;

    let mut movies = MockMovieRepository::new();
    movies.expect_find_unkeyed().returning(|| Ok(Vec::new()));
    movies
        .expect_find_keyed_before_version()
        .times(1)
        .returning(|_| Err(DbErr::Custom("connection reset".to_string())));
    movies.expect_delete_orphaned().returning(|_| Ok(0));
    let h = Harness::purging_at_once_with_movies(Arc::new(movies)).await;
    const CURRENT: &str = "Severance/Season 1/Severance.S01E01.mkv";
    h.write(CURRENT);
    const UNPROBED: &str = "Severance/Season 1/Severance.S01E02.mkv";
    let unprobed = h.write(UNPROBED);
    let (size_bytes, mtime) = read_fs_meta(unprobed.parent().unwrap(), &unprobed).unwrap();
    h.file_repo
        .create(CreateMediaFile {
            library_id: h.library.id,
            path: unprobed,
            hash: u64::MAX,
            size_bytes,
            mtime,
            identity: None,
            mime_type: Some("video/x-matroska".to_string()),
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Unknown,
            classifier_version: 0,
            container_tags: None,
        })
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(h.file_version(CURRENT), CLASSIFIER_VERSION);
    for rel in [CURRENT, UNPROBED, CURRENT] {
        h.service
            .reconcile_path(h.library.id, h.root.join(rel), FsEventKind::Modified)
            .await
            .unwrap();
    }

    let warnings = h
        .admin_log_repo
        .list(100, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|l| l.message.contains("current naming rules"))
        .count();
    assert_eq!(warnings, 1, "only the scan's failed pass is reported");
}

/// The administrator's scan of one library, arriving before any scan of
/// every library, runs the identity passes itself before it reclassifies.
#[tokio::test]
async fn a_scan_of_one_library_rekeys_before_it_reclassifies() {
    let h = Harness::purging_at_once().await;
    let show = stale_enriched_greys(&h).await;

    h.scan().await;

    let after = h.only_show();
    assert_eq!(after.id, show, "the same show");
    assert_eq!(after.tmdb_id, Some(1416), "with its enrichment");
    assert_eq!(after.identity_key.as_deref(), Some("greys anatomy|"));
    assert_eq!(h.season_one(show), vec![(1, vec![GREYS.to_string()])]);
    assert_eq!(h.file_version(GREYS), CLASSIFIER_VERSION);

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(h.only_show().id, show);
}
