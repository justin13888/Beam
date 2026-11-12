use super::*;

// ─── keys an older fold or inference derived (issue #182) ──────────────────

/// A deployment of #183 keyed `Grey's Anatomy` as `grey s anatomy|`; the
/// current fold keys its files `greys anatomy|`. The first scan re-derives
/// the key in place, so the title keeps its id and enrichment and takes its
/// files, present and missing, instead of a second show being created
/// beside it.
#[tokio::test]
async fn a_key_from_before_the_apostrophe_fold_is_rederived_in_place() {
    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let show = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &[
                "Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv",
                "Grey's Anatomy/Season 1/Greys.Anatomy.S01E02.mkv",
            ],
            base,
        )
        .await;
    h.enrich_show(show, 1416).await;
    let movie = h
        .keyed_movie(
            "Ocean's Eleven",
            Some(2001),
            "ocean s eleven|2001",
            &["Ocean's Eleven (2001)/Ocean's Eleven (2001).mkv"],
            base,
        )
        .await;
    h.enrich_movie(movie, 2001, 161).await;
    // The second episode went missing before the upgrade.
    h.remove("Grey's Anatomy/Season 1/Greys.Anatomy.S01E02.mkv");
    let missing: Vec<Uuid> = h
        .file_repo
        .files
        .lock()
        .unwrap()
        .values()
        .filter(|f| f.path.ends_with("Greys.Anatomy.S01E02.mkv"))
        .map(|f| f.id)
        .collect();
    h.file_repo
        .mark_missing(missing, chrono::Utc::now())
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let after = h.only_show();
    assert_eq!(after.id, show, "the same show, not a new one");
    assert_eq!(after.tmdb_id, Some(1416), "with its enrichment");
    assert_eq!(after.identity_key.as_deref(), Some("greys anatomy|"));
    assert_eq!(
        h.season_one(show),
        vec![
            (
                1,
                vec!["Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv".to_string()]
            ),
            (
                2,
                vec!["Grey's Anatomy/Season 1/Greys.Anatomy.S01E02.mkv".to_string()]
            ),
        ],
        "the present and the missing episode both stay on it"
    );
    let after = h.only_movie();
    assert_eq!(after.id, movie);
    assert_eq!(after.tmdb_id, Some(161));
    assert_eq!(after.identity_key.as_deref(), Some("oceans eleven|2001"));
    assert!(
        h.show_repo
            .find_keyed_before_version(CLASSIFIER_VERSION)
            .await
            .unwrap()
            .is_empty()
            && h.movie_repo
                .find_keyed_before_version(CLASSIFIER_VERSION)
                .await
                .unwrap()
                .is_empty(),
        "every key now carries the current version"
    );
    let details = h
        .admin_log_details("identity keys of")
        .await
        .expect("the administrator is told");
    assert_eq!(details["rekeyed"], 2);

    // The next file of either spelling joins it.
    h.write("Greys.Anatomy.S02E01.mkv");
    h.scan().await;
    assert_eq!(h.only_show().id, show);
}

/// Under #183's fold a folder's `Grey's Anatomy` and a scene release's
/// `Greys.Anatomy` were two shows. Now they are one key, so the two merge:
/// the matched show survives, though it is the newer, and takes the other's
/// episodes -- one episode per number, however many files -- while the
/// other is retired.
#[tokio::test]
async fn titles_the_current_fold_reads_as_one_are_merged_into_the_matched_one() {
    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let scene = h
        .keyed_show(
            "Greys Anatomy",
            "greys anatomy|",
            &["Greys.Anatomy.S01E01.720p.mkv", "Greys.Anatomy.S01E02.mkv"],
            base,
        )
        .await;
    let folder = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &["Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv"],
            base + chrono::Duration::days(1),
        )
        .await;
    h.enrich_show(folder, 1416).await;
    // Two movies neither of which a provider matched: the older survives.
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

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let show = h.only_show();
    assert_eq!(show.id, folder, "the show a provider matched is kept");
    assert_eq!(show.tmdb_id, Some(1416));
    assert_eq!(show.identity_key.as_deref(), Some("greys anatomy|"));
    assert_eq!(
        h.season_one(folder),
        vec![
            (
                1,
                vec![
                    "Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv".to_string(),
                    "Greys.Anatomy.S01E01.720p.mkv".to_string(),
                ]
            ),
            (2, vec!["Greys.Anatomy.S01E02.mkv".to_string()]),
        ],
        "both copies of episode 1 are sources of one episode"
    );
    assert!(h.show_repo.find_by_id(scene).await.unwrap().is_none());

    let movie = h.only_movie();
    assert_eq!(
        movie.id, older,
        "of two unmatched titles, the older is kept"
    );
    assert_eq!(movie.identity_key.as_deref(), Some("oceans eleven|2001"));
    let entries = h.movie_repo.find_entries_by_movie_id(older).await.unwrap();
    assert_eq!(entries.len(), 1, "one default-edition entry");
    let on_entry = h
        .file_repo
        .files
        .lock()
        .unwrap()
        .values()
        .filter(|f| matches!(f.content, Some(MediaFileContent::Movie { movie_entry_id, .. }) if movie_entry_id == entries[0].id))
        .count();
    assert_eq!(on_entry, 2, "with both copies");
    assert!(h.movie_repo.find_by_id(newer).await.unwrap().is_none());

    let details = h
        .admin_log_details("identity keys of")
        .await
        .expect("the administrator is told");
    assert_eq!(
        details["merged_shows"],
        serde_json::json!([{ "kept": folder, "retired": scene }])
    );
    assert_eq!(
        details["merged_movies"],
        serde_json::json!([{ "kept": older, "retired": newer }])
    );

    // Every key is current now, so a second pass finds nothing to do, and a
    // second scan leaves the merged titles as they are.
    let IdentityRekey {
        rekeyed,
        merged_movies,
        merged_shows,
        ambiguous_movies,
        ambiguous_shows,
        conflicting_movies,
        conflicting_shows,
    } = h.service.rekey_stale_titles().await.unwrap();
    assert_eq!(rekeyed, 0);
    assert!(merged_movies.is_empty() && merged_shows.is_empty());
    assert!(ambiguous_movies.is_empty() && ambiguous_shows.is_empty());
    assert!(conflicting_movies.is_empty() && conflicting_shows.is_empty());
    h.scan().await;
    assert_eq!(h.only_show().id, folder);
    assert_eq!(h.only_movie().id, older);
    assert_eq!(h.season_one(folder).len(), 2);
}

/// A library indexed before part tokens were read (issue #233): each part of
/// `Movie (2019) - CD1.avi` and `- CD2.avi` became a film of its own, titled
/// and keyed with its token (`movie cd1|2019`) by version-2 rules. The first
/// scan under the current rules merges them into one movie titled as the
/// files now spell it, records each file's part, and keeps every file's row
/// -- and with it the progress a user had on part 2.
#[tokio::test]
async fn an_upgrade_merges_the_parts_a_pre_stacking_build_indexed_as_films() {
    use beam_domain::models::watch_state::{RecordProgress, TitleRef, WatchTarget};
    use beam_domain::repositories::WatchStateRepository;

    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let cd1 = "Movie (2019)/Movie (2019) - CD1.avi";
    let cd2 = "Movie (2019)/Movie (2019) - CD2.avi";
    let mut films = Vec::new();
    for (n, (rel, title, key)) in [
        (cd1, "Movie - CD1", "movie cd1|2019"),
        (cd2, "Movie - CD2", "movie cd2|2019"),
    ]
    .into_iter()
    .enumerate()
    {
        let created_at = base + chrono::Duration::days(n as i64);
        let id = h
            .legacy_movie_created_at(title, Some(2019), &[rel], created_at)
            .await;
        assert!(
            h.movie_repo
                .rekey(id, Some(key.to_string()), 2)
                .await
                .unwrap()
        );
        films.push(id);
    }
    let file_id = |rel: &str| {
        let path = h.root.join(rel);
        h.file_repo
            .files
            .lock()
            .unwrap()
            .values()
            .find(|f| f.path == path)
            .map(|f| f.id)
            .expect("the file is indexed")
    };
    let (cd1_id, cd2_id) = (file_id(cd1), file_id(cd2));
    let user = Uuid::new_v4();
    // Played as the film part 2 was indexed as.
    h.watch_state
        .record_progress(RecordProgress {
            user_id: user,
            target: WatchTarget::Movie { movie_id: films[1] },
            file_id: cd2_id,
            position_secs: 600.0,
            duration_secs: Some(3000.0),
            // Part 2 of 2 plays the end.
            finishes_title: true,
        })
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let movie = h.only_movie();
    assert_eq!(
        movie.id, films[0],
        "of two unmatched titles, the older is kept"
    );
    assert_eq!(movie.title, "Movie", "titled as the files now spell it");
    assert_eq!(movie.year, Some(2019));
    assert_eq!(movie.identity_key.as_deref(), Some("movie|2019"));
    assert!(h.movie_repo.find_by_id(films[1]).await.unwrap().is_none());

    let entries = h
        .movie_repo
        .find_entries_by_movie_id(movie.id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1, "both parts are the one default edition");
    let mut parts: Vec<(Uuid, Option<u32>, u16)> = h
        .file_repo
        .find_by_movie_entry_id(entries[0].id)
        .await
        .unwrap()
        .into_iter()
        .map(|f| match f.content {
            Some(MediaFileContent::Movie { part_number, .. }) => {
                (f.id, part_number, f.classifier_version)
            }
            other => panic!("not a movie file: {other:?}"),
        })
        .collect();
    parts.sort_by_key(|(_, part, _)| *part);
    assert_eq!(
        parts,
        vec![
            (cd1_id, Some(1), CLASSIFIER_VERSION),
            (cd2_id, Some(2), CLASSIFIER_VERSION)
        ],
        "each file keeps its row and gains its part"
    );

    let progress = h
        .watch_state
        .find(user, WatchTarget::Movie { movie_id: movie.id })
        .await
        .unwrap()
        .expect("the progress on part 2 survives the merge, on the kept movie");
    assert_eq!(progress.position_secs, 600.0);
    assert_eq!(progress.last_file_id, Some(cd2_id), "at part 2");
    assert_eq!(
        h.watch_state
            .find_continue_candidates(user, None, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.title)
            .collect::<Vec<_>>(),
        vec![TitleRef::Movie(movie.id)],
        "and is still listed to continue"
    );

    let details = h
        .admin_log_details("identity keys of")
        .await
        .expect("the administrator is told");
    assert_eq!(
        details["merged_movies"],
        serde_json::json!([{ "kept": films[0], "retired": films[1] }])
    );
}

/// A title an administrator or a provider named keeps its name through a
/// re-derivation: only a title still spelled as older rules spelled it is
/// retitled.
#[tokio::test]
async fn a_rekey_retitles_only_a_title_older_rules_spelled() {
    let h = Harness::keeping_missing_files().await;
    let matched = h
        .legacy_movie("Heat - CD1", Some(1995), &["Heat (1995) - CD1.avi"])
        .await;
    assert!(
        h.movie_repo
            .rekey(matched, Some("heat cd1|1995".to_string()), 2)
            .await
            .unwrap()
    );
    h.enrich_movie(matched, 1995, 949).await;
    let locked = h
        .legacy_movie("Ronin - CD1", Some(1998), &["Ronin (1998) - CD1.avi"])
        .await;
    assert!(
        h.movie_repo
            .rekey(locked, Some("ronin cd1|1998".to_string()), 2)
            .await
            .unwrap()
    );
    h.enrichment_repo
        .ensure_pending(EnrichmentTargetId::Movie(locked))
        .await
        .unwrap();
    h.enrichment_repo
        .set_locked_fields(
            EnrichmentTargetId::Movie(locked),
            &[MetadataField::Title].into_iter().collect(),
        )
        .await
        .unwrap();
    // A key the current rules leave as it was: the title keeps its spelling,
    // however the files spell it.
    let unchanged = h
        .legacy_movie("Amelie", Some(2001), &["amelie.2001.mkv"])
        .await;
    assert!(
        h.movie_repo
            .rekey(unchanged, Some("amelie|2001".to_string()), 2)
            .await
            .unwrap()
    );

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let title = |id| h.movie_repo.movies.lock().unwrap()[&id].clone().title;
    assert_eq!(
        title(matched),
        "Provider Title",
        "a provider's title is kept"
    );
    assert_eq!(title(locked), "Ronin - CD1", "a locked title is kept");
    assert_eq!(
        title(unchanged),
        "Amelie",
        "an unchanged key keeps its title"
    );
    for (id, key) in [(matched, "heat|1995"), (locked, "ronin|1998")] {
        assert_eq!(
            h.movie_repo.movies.lock().unwrap()[&id]
                .identity_key
                .as_deref(),
            Some(key),
            "the key is re-derived all the same"
        );
    }
}

/// The `matched` guard alone: a title a provider matched keeps its name even
/// while that name is still the one older rules spelled.
#[tokio::test]
async fn a_rekey_does_not_retitle_a_matched_title_older_rules_spelled() {
    let h = Harness::keeping_missing_files().await;
    let id = h
        .legacy_movie("Heat - CD1", Some(1995), &["Heat (1995) - CD1.avi"])
        .await;
    assert!(
        h.movie_repo
            .rekey(id, Some("heat cd1|1995".to_string()), 2)
            .await
            .unwrap()
    );
    // Matched without renaming it: only the provider id says so.
    h.movie_repo
        .movies
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .tmdb_id = Some(949);

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let movie = h.only_movie();
    assert_eq!(movie.title, "Heat - CD1", "a matched title is kept");
    assert_eq!(movie.identity_key.as_deref(), Some("heat|1995"));
}

/// The `same_year` guard alone: files that spell another year than the
/// title's leave its name as it is.
#[tokio::test]
async fn a_rekey_does_not_retitle_a_title_whose_files_spell_another_year() {
    let h = Harness::keeping_missing_files().await;
    let id = h
        .legacy_movie("Heat - CD1", Some(1996), &["Heat (1995) - CD1.avi"])
        .await;
    assert!(
        h.movie_repo
            .rekey(id, Some("heat cd1|1996".to_string()), 2)
            .await
            .unwrap()
    );

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let movie = h.only_movie();
    assert_eq!(movie.title, "Heat - CD1", "another year keeps the title");
    assert_eq!(movie.identity_key.as_deref(), Some("heat|1995"));
}

/// Which pairs of provider ids name different entries: both titles are
/// matched, and they disagree on an id both carry or share none.
#[test]
fn provider_ids_conflict_only_when_both_are_matched_to_different_entries() {
    let ids = |tmdb: Option<u32>, imdb: Option<&'static str>| ProviderIds {
        tmdb,
        imdb,
        tvdb: None,
        anilist: None,
    };
    let only = |tvdb: Option<u32>, anilist: Option<u32>| ProviderIds {
        tmdb: None,
        imdb: None,
        tvdb,
        anilist,
    };
    let cases = [
        // Neither, or only one, is matched: nothing to lose.
        (ids(None, None), ids(None, None), false),
        (ids(Some(1), None), ids(None, None), false),
        (ids(None, None), ids(None, Some("tt1")), false),
        (only(None, Some(3)), ids(None, None), false),
        // An id agreeing and none disagreeing: the same entry.
        (ids(Some(1), None), ids(Some(1), None), false),
        (ids(Some(1), Some("tt1")), ids(Some(1), None), false),
        (only(Some(7), None), only(Some(7), Some(3)), false),
        (only(None, Some(3)), only(None, Some(3)), false),
        // An id disagreeing, however many agree.
        (ids(Some(1), None), ids(Some(2), None), true),
        (ids(Some(1), Some("tt1")), ids(Some(1), Some("tt2")), true),
        (only(Some(7), Some(3)), only(Some(7), Some(4)), true),
        (only(Some(7), None), only(Some(8), None), true),
        // No id both carry: nothing says they are one.
        (ids(Some(1), None), ids(None, Some("tt1")), true),
        (only(Some(7), None), only(None, Some(3)), true),
    ];
    for (a, b, conflict) in cases {
        assert_eq!(provider_ids_conflict(a, b), conflict, "{a:?} / {b:?}");
        assert_eq!(provider_ids_conflict(b, a), conflict, "{b:?} / {a:?}");
    }
}

/// A pair kept apart by a provider-id conflict is settled by correcting a
/// match, as the warning says: at the next process start the two merge, and a
/// part held on the stale title -- stamped as classified, so reclassification
/// never reads its part -- ends up as the part its name says, so the parts
/// are one source again. Whichever title is older survives, so both
/// directions are checked: the held file moved onto the holder, and the
/// holder's file moved onto the held file's title.
#[tokio::test]
async fn correcting_a_match_merges_a_held_part_as_its_part() {
    for stale_is_older in [false, true] {
        correcting_a_match_settles_a_held_part(stale_is_older).await;
    }
}

async fn correcting_a_match_settles_a_held_part(stale_is_older: bool) {
    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let (holder_at, stale_at) = if stale_is_older {
        (base + chrono::Duration::days(1), base)
    } else {
        (base, base + chrono::Duration::days(1))
    };
    let cd1 = "Movie (2019)/Movie (2019) - CD1.avi";
    let cd2 = "Movie (2019)/Movie (2019) - CD2.avi";
    let holder = h
        .keyed_movie("Movie", Some(2019), "movie|2019", &[cd1], holder_at)
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
            &[cd2],
            stale_at,
        )
        .await;
    h.enrich_movie(stale, 2019, 2).await;

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(
        h.movie_repo.movies.lock().unwrap()[&stale].identity_key,
        Some("movie cd2|2019".to_string()),
        "the conflict holds the pair apart (stale older: {stale_is_older})"
    );
    assert_eq!(
        h.file_version(cd2),
        CLASSIFIER_VERSION,
        "and holds CD2 as classified (stale older: {stale_is_older})"
    );

    // The administrator corrects the stale title's match to the holder's film,
    // and the server restarts.
    h.enrich_movie(stale, 2019, 1).await;
    h.service
        .identity_passes_succeeded
        .store(false, Ordering::SeqCst);
    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let movie = h.only_movie();
    let older = if stale_is_older { stale } else { holder };
    assert_eq!(
        movie.id, older,
        "of two matched titles, the older is kept (stale older: {stale_is_older})"
    );
    assert_eq!(movie.identity_key.as_deref(), Some("movie|2019"));
    assert_eq!(
        h.stacked_sources(movie.id).await,
        vec![vec![h.root.join(cd1), h.root.join(cd2)]],
        "CD2 is part 2, so the parts play as one source (stale older: {stale_is_older})"
    );
}

/// A held title can also be settled by the holder going: its files purged,
/// the holder is retired, and at the next process start the held title takes
/// the key its files spell in place. Its held part is then read as the part
/// its name says, so the missing part, once back, joins it as one source.
#[tokio::test]
async fn a_held_title_that_takes_its_key_in_place_reads_its_held_part() {
    let h = Harness::purging_at_once().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let cd1 = "Movie (2019)/Movie (2019) - CD1.avi";
    let cd2 = "Movie (2019)/Movie (2019) - CD2.avi";
    let holder = h
        .keyed_movie("Movie", Some(2019), "movie|2019", &[cd1], base)
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
            &[cd2],
            base + chrono::Duration::days(1),
        )
        .await;
    h.enrich_movie(stale, 2019, 2).await;

    // The pair is held apart, and CD1 -- the holder's only file -- is gone.
    h.remove(cd1);
    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(h.file_version(cd2), CLASSIFIER_VERSION, "CD2 is held");
    assert!(
        h.movie_repo.find_by_id(holder).await.unwrap().is_none(),
        "the holder is retired with its file"
    );

    // The server restarts, and CD1 comes back.
    h.service
        .identity_passes_succeeded
        .store(false, Ordering::SeqCst);
    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(
        h.movie_repo.movies.lock().unwrap()[&stale].identity_key,
        Some("movie|2019".to_string()),
        "the held title takes its key in place"
    );
    h.write(cd1);
    h.scan().await;

    let movie = h.only_movie();
    assert_eq!(movie.id, stale);
    assert_eq!(
        h.stacked_sources(stale).await,
        vec![vec![h.root.join(cd1), h.root.join(cd2)]],
        "CD2 is part 2, so CD1 joins it as one source"
    );
}

/// The show counterpart: two shows matched to different entries are never
/// merged by a rekey, and a scan leaves each its episodes.
#[tokio::test]
async fn a_rekey_never_merges_shows_matched_to_different_entries() {
    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let scene_file = "Greys.Anatomy.S01E01.720p.mkv";
    let folder_file = "Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv";
    let scene = h
        .keyed_show("Greys Anatomy", "greys anatomy|", &[scene_file], base)
        .await;
    h.enrich_show(scene, 1416).await;
    let folder = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &[folder_file],
            base + chrono::Duration::days(1),
        )
        .await;
    h.enrich_show(folder, 9999).await;

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let show = |id| h.show_repo.shows.lock().unwrap()[&id].clone();
    assert_eq!(
        (show(scene).identity_key, show(scene).tmdb_id),
        (Some("greys anatomy|".to_string()), Some(1416))
    );
    assert_eq!(
        (show(folder).identity_key, show(folder).tmdb_id),
        (Some("grey s anatomy|".to_string()), Some(9999)),
        "the stale show keeps its key and its match"
    );
    assert_eq!(h.season_one(scene), vec![(1, vec![scene_file.to_string()])]);
    assert_eq!(
        h.season_one(folder),
        vec![(1, vec![folder_file.to_string()])]
    );
    let warning = h
        .admin_log_details("different provider entries")
        .await
        .expect("the administrator is told");
    assert_eq!(
        warning["conflicting_shows"],
        serde_json::json!([{ "stale": folder, "holder": scene }])
    );
}

/// What a merge or a reclassification carries to the title kept: each
/// user's list entries and each viewer's watch state.
#[path = "index_identity_carry_tests.rs"]
mod carry;

/// The rekey pass at its edges -- what an administrator set, a survivor
/// that loses the key, husks, ambiguous files -- and reclassification
/// waiting for the identity passes.
#[path = "index_identity_pass_tests.rs"]
mod passes;

/// The in-memory file repository, refusing every write of a file's
/// classification: the pass that tries one stops there, as a pass the process
/// dies in does.
#[derive(Debug)]
struct ClassificationRefused(Arc<InMemoryFileRepository>);

#[async_trait::async_trait]
impl FileRepository for ClassificationRefused {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>, DbErr> {
        self.0.find_by_id(id).await
    }
    async fn find_by_path(&self, path: &str) -> Result<Option<MediaFile>, DbErr> {
        self.0.find_by_path(path).await
    }
    async fn find_by_hash(&self, hash: u64) -> Result<Vec<MediaFile>, DbErr> {
        self.0.find_by_hash(hash).await
    }
    async fn find_all_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        self.0.find_all_by_library(library_id).await
    }
    async fn find_page_by_library(
        &self,
        library_id: Uuid,
        after: Option<beam_domain::models::file::LibraryFilePosition>,
        limit: std::num::NonZeroU32,
    ) -> Result<Vec<MediaFile>, DbErr> {
        self.0.find_page_by_library(library_id, after, limit).await
    }
    async fn find_all_by_library_including_missing(
        &self,
        library_id: Uuid,
    ) -> Result<Vec<MediaFile>, DbErr> {
        self.0
            .find_all_by_library_including_missing(library_id)
            .await
    }
    async fn find_all_under(&self, library_id: Uuid, dir: &Path) -> Result<Vec<MediaFile>, DbErr> {
        self.0.find_all_under(library_id, dir).await
    }
    async fn find_by_library_and_hash_including_missing(
        &self,
        library_id: Uuid,
        hash: u64,
    ) -> Result<Vec<MediaFile>, DbErr> {
        self.0
            .find_by_library_and_hash_including_missing(library_id, hash)
            .await
    }
    async fn find_beneath_including_missing(
        &self,
        library_id: Uuid,
        dir: &Path,
    ) -> Result<Vec<MediaFile>, DbErr> {
        self.0.find_beneath_including_missing(library_id, dir).await
    }
    async fn find_by_movie_entry_id(&self, movie_entry_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        self.0.find_by_movie_entry_id(movie_entry_id).await
    }
    async fn find_by_episode_id(&self, episode_id: Uuid) -> Result<Vec<MediaFile>, DbErr> {
        self.0.find_by_episode_id(episode_id).await
    }
    async fn create(&self, create: CreateMediaFile) -> Result<MediaFile, DbErr> {
        self.0.create(create).await
    }
    async fn update(&self, update: UpdateMediaFile) -> Result<MediaFile, DbErr> {
        self.0.update(update).await
    }
    async fn set_classification(
        &self,
        _id: Uuid,
        _classification: FileClassification,
    ) -> Result<MediaFile, DbErr> {
        Err(DbErr::Custom("connection reset".to_string()))
    }
    async fn mark_missing(&self, ids: Vec<Uuid>, at: DateTime<Utc>) -> Result<u64, DbErr> {
        self.0.mark_missing(ids, at).await
    }
    async fn relink(
        &self,
        relinks: Vec<FileRelink>,
        displaced: Vec<Uuid>,
        at: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        self.0.relink(relinks, displaced, at).await
    }
    async fn restore(&self, id: Uuid) -> Result<(), DbErr> {
        self.0.restore(id).await
    }
    async fn purge_missing(&self, ids: Vec<Uuid>) -> Result<u64, DbErr> {
        self.0.purge_missing(ids).await
    }
    async fn count_all(&self) -> Result<u64, DbErr> {
        self.0.count_all().await
    }
}
