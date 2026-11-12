use super::*;

/// A merge moves each user's watchlist and favourites entries from the
/// retired title onto the kept one (issue #262), as it moves watch state: the
/// retired title goes, and its entries would go with it.
#[tokio::test]
async fn a_merge_carries_each_users_list_entries_to_the_kept_title() {
    use beam_domain::models::user_list::UserList;
    use beam_domain::models::watch_state::TitleRef;
    use beam_domain::repositories::UserListRepository;

    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
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
            &["Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv"],
            base + chrono::Duration::days(1),
        )
        .await;
    h.enrich_show(folder, 1416).await;
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
    let user = Uuid::new_v4();
    h.lists
        .add(user, UserList::Watchlist, TitleRef::Movie(newer))
        .await
        .unwrap();
    h.lists
        .add(user, UserList::Favourites, TitleRef::Show(scene))
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(h.only_movie().id, older);
    assert_eq!(h.only_show().id, folder);

    assert_eq!(
        h.lists
            .lists_of(user, TitleRef::Movie(older))
            .await
            .unwrap(),
        [UserList::Watchlist]
    );
    assert_eq!(
        h.lists
            .lists_of(user, TitleRef::Show(folder))
            .await
            .unwrap(),
        [UserList::Favourites]
    );
    assert_eq!(
        h.lists
            .count(user, UserList::Watchlist, &AccessPolicy::unrestricted())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        h.lists
            .count(user, UserList::Favourites, &AccessPolicy::unrestricted())
            .await
            .unwrap(),
        1
    );
}

/// A merge moves each viewer's watch state from the retired title onto the
/// kept one (issue #188): the retired title goes, and its rows would go with
/// it. A viewer with state on both keeps one row per title.
#[tokio::test]
async fn a_merge_carries_each_viewers_watch_state_to_the_kept_title() {
    use beam_domain::models::watch_state::{RecordProgress, WatchTarget};
    use beam_domain::repositories::WatchStateRepository;

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
    let scene_season = h.show_repo.find_seasons_by_show_id(scene).await.unwrap()[0].id;
    let mut scene_episodes = h
        .show_repo
        .find_episodes_by_season_id(scene_season)
        .await
        .unwrap();
    scene_episodes.sort_by_key(|e| e.episode_number);
    let (viewer, both) = (Uuid::new_v4(), Uuid::new_v4());
    let report = |user_id, target, position_secs| RecordProgress {
        user_id,
        target,
        file_id: Uuid::new_v4(),
        position_secs,
        duration_secs: Some(100.0),
        finishes_title: true,
    };
    let movie = |movie_id| WatchTarget::Movie { movie_id };
    for (user, target, position) in [
        (viewer, movie(newer), 40.0),
        (both, movie(newer), 99.0),
        (both, movie(older), 30.0),
        (
            viewer,
            WatchTarget::Episode {
                episode_id: scene_episodes[0].id,
                show_id: scene,
            },
            20.0,
        ),
        (
            viewer,
            WatchTarget::Episode {
                episode_id: scene_episodes[1].id,
                show_id: scene,
            },
            99.0,
        ),
    ] {
        h.watch_state
            .record_progress(report(user, target, position))
            .await
            .unwrap();
    }

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();
    assert_eq!(h.only_movie().id, older);
    assert_eq!(h.only_show().id, folder);

    let place = |row: Option<beam_domain::models::watch_state::WatchState>| {
        let row = row.expect("a row");
        (row.position_secs, row.completed, row.play_count)
    };
    assert_eq!(
        place(h.watch_state.find(viewer, movie(older)).await.unwrap()),
        (40.0, false, 0)
    );
    assert!(
        h.watch_state
            .find(viewer, movie(newer))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        place(h.watch_state.find(both, movie(older)).await.unwrap()),
        (30.0, true, 1),
        "two rows of one viewer fold into one"
    );
    let mut episodes: Vec<(u32, f64, bool)> = Vec::new();
    for row in h.watch_state.find_for_show(viewer, folder).await.unwrap() {
        let WatchTarget::Episode { episode_id, .. } = row.target else {
            panic!("a show's row is an episode's");
        };
        let episode = h
            .show_repo
            .find_episode_by_id(episode_id)
            .await
            .unwrap()
            .expect("an episode of the kept show");
        episodes.push((episode.episode_number, row.position_secs, row.completed));
    }
    episodes.sort_by_key(|(number, _, _)| *number);
    assert_eq!(
        episodes,
        vec![(1, 20.0, false), (2, 0.0, true)],
        "each episode's state is on the kept show's episode of that number"
    );
    assert!(
        h.watch_state
            .find_for_show(viewer, scene)
            .await
            .unwrap()
            .is_empty()
    );
}

/// A viewer's report against `target`.
fn played(
    user_id: Uuid,
    target: beam_domain::models::watch_state::WatchTarget,
    position_secs: f64,
) -> beam_domain::models::watch_state::RecordProgress {
    beam_domain::models::watch_state::RecordProgress {
        user_id,
        target,
        file_id: Uuid::new_v4(),
        position_secs,
        duration_secs: Some(100.0),
        finishes_title: true,
    }
}

/// A merge carries the retired movie's watch state before it moves the
/// movie's first file (issue #246): a pass that stops between the two leaves
/// the state on the title that keeps it, not on one the next scan can empty
/// and orphan cleanup retire with the state.
#[tokio::test]
async fn a_movie_merge_carries_watch_state_before_it_moves_a_file() {
    use beam_domain::models::watch_state::WatchTarget;
    use beam_domain::repositories::WatchStateRepository;

    let h = Harness::keeping_missing_files_with_files(|files| {
        Arc::new(ClassificationRefused(files)) as Arc<dyn FileRepository>
    })
    .await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
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
    let viewer = Uuid::new_v4();
    h.watch_state
        .record_progress(played(viewer, WatchTarget::Movie { movie_id: newer }, 40.0))
        .await
        .unwrap();

    h.service
        .rekey_stale_titles()
        .await
        .expect_err("the pass stops at its first file move");

    assert_eq!(
        h.watch_state
            .find(viewer, WatchTarget::Movie { movie_id: older })
            .await
            .unwrap()
            .map(|row| row.position_secs),
        Some(40.0),
        "the state is already on the movie the merge keeps"
    );
    assert!(
        h.watch_state
            .find(viewer, WatchTarget::Movie { movie_id: newer })
            .await
            .unwrap()
            .is_none()
    );
}

/// As for a movie: each episode's state is carried before its files move.
#[tokio::test]
async fn a_show_merge_carries_watch_state_before_it_moves_a_file() {
    use beam_domain::models::watch_state::WatchTarget;
    use beam_domain::repositories::WatchStateRepository;

    let h = Harness::keeping_missing_files_with_files(|files| {
        Arc::new(ClassificationRefused(files)) as Arc<dyn FileRepository>
    })
    .await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let older = h
        .keyed_show(
            "Grey's Anatomy",
            "grey s anatomy|",
            &["Grey's Anatomy/Season 1/Greys.Anatomy.S01E01.mkv"],
            base,
        )
        .await;
    let newer = h
        .keyed_show(
            "Greys Anatomy",
            "greys anatomy|",
            &["Greys.Anatomy.S01E01.720p.mkv"],
            base + chrono::Duration::days(1),
        )
        .await;
    let episode_of = |show: Uuid| {
        let season = h
            .show_repo
            .seasons
            .lock()
            .unwrap()
            .values()
            .find(|s| s.show_id == show)
            .unwrap()
            .id;
        let episode = h
            .show_repo
            .episodes
            .lock()
            .unwrap()
            .values()
            .find(|e| e.season_id == season)
            .unwrap()
            .id;
        WatchTarget::Episode {
            episode_id: episode,
            show_id: show,
        }
    };
    let viewer = Uuid::new_v4();
    h.watch_state
        .record_progress(played(viewer, episode_of(newer), 20.0))
        .await
        .unwrap();

    h.service
        .rekey_stale_titles()
        .await
        .expect_err("the pass stops at its first file move");

    assert_eq!(
        h.watch_state
            .find(viewer, episode_of(older))
            .await
            .unwrap()
            .map(|row| row.position_secs),
        Some(20.0),
        "the state is already on the kept show's episode of that number"
    );
    assert!(
        h.watch_state
            .find_for_show(viewer, newer)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Reclassifying a file older rules gave a title of its own carries that
/// title's watch state to the one the file joins, when the move leaves it
/// with no file (issue #246): orphan cleanup then retires it, and would take
/// the state with it. A husk named after a season folder is such a title.
#[tokio::test]
async fn reclassifying_a_titles_last_file_carries_its_watch_state() {
    use beam_domain::models::watch_state::WatchTarget;
    use beam_domain::repositories::WatchStateRepository;

    let h = Harness::keeping_missing_files().await;
    let husk = h
        .keyed_show(
            "Season 05",
            "season 05|",
            &["Show X/Season 05/Show.X.S05E01.mkv"],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;
    let husk_episode = {
        let season = h.show_repo.find_seasons_by_show_id(husk).await.unwrap()[0].id;
        h.show_repo
            .find_episodes_by_season_id(season)
            .await
            .unwrap()[0]
            .id
    };
    let viewer = Uuid::new_v4();
    h.watch_state
        .record_progress(played(
            viewer,
            WatchTarget::Episode {
                episode_id: husk_episode,
                show_id: husk,
            },
            20.0,
        ))
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let show = h.only_show();
    assert_ne!(show.id, husk, "the husk is retired");
    let rows = h.watch_state.find_for_show(viewer, show.id).await.unwrap();
    let [row] = rows.as_slice() else {
        panic!("one row on the series: {rows:?}");
    };
    let WatchTarget::Episode { episode_id, .. } = row.target else {
        panic!("a show's row is an episode's");
    };
    let episode = h
        .show_repo
        .find_episode_by_id(episode_id)
        .await
        .unwrap()
        .expect("an episode of the series");
    let season = h
        .show_repo
        .find_season_by_id(episode.season_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            season.season_number,
            episode.episode_number,
            row.position_secs
        ),
        (5, 1, 20.0),
        "on the episode the file now is"
    );
}

/// Reclassifying a title's last file carries its users' watchlist and
/// favourites entries to the title the file joins, as it carries watch state
/// (issue #262): orphan cleanup retires the emptied title, and would take the
/// entries with it. Lists hold the show, so a husk show's entries follow its
/// last episode's file to the series.
#[tokio::test]
async fn reclassifying_a_titles_last_file_carries_its_list_entries() {
    use beam_domain::models::user_list::UserList;
    use beam_domain::models::watch_state::TitleRef;
    use beam_domain::repositories::UserListRepository;

    let h = Harness::keeping_missing_files().await;
    let base = chrono::Utc::now() - chrono::Duration::days(30);
    let husk = h
        .keyed_show(
            "Season 05",
            "season 05|",
            &["Show X/Season 05/Show.X.S05E01.mkv"],
            base,
        )
        .await;
    // Its files name two films and neither is the one its key names.
    let emptied = h
        .keyed_movie(
            "Dune",
            None,
            "dune|",
            &["Dune.1984.mkv", "Dune.2021.mkv"],
            base,
        )
        .await;
    let user = Uuid::new_v4();
    h.lists
        .add(user, UserList::Favourites, TitleRef::Show(husk))
        .await
        .unwrap();
    h.lists
        .add(user, UserList::Watchlist, TitleRef::Movie(emptied))
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let show = h.only_show();
    assert_ne!(show.id, husk, "the husk is retired");
    assert_eq!(
        h.lists
            .lists_of(user, TitleRef::Show(show.id))
            .await
            .unwrap(),
        [UserList::Favourites],
        "on the series the file now is"
    );
    let movies = h.movies();
    assert_eq!(movies.len(), 2, "the files are two films: {movies:?}");
    assert!(movies.iter().all(|movie| movie.id != emptied), "retired");
    let mut kept = Vec::new();
    for movie in &movies {
        kept.extend(
            h.lists
                .lists_of(user, TitleRef::Movie(movie.id))
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        kept,
        [UserList::Watchlist],
        "on the film the last file joined"
    );
    assert_eq!(
        h.lists
            .count(user, UserList::Favourites, &AccessPolicy::unrestricted())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        h.lists
            .count(user, UserList::Watchlist, &AccessPolicy::unrestricted())
            .await
            .unwrap(),
        1
    );
}

/// A movie whose files the scan reclassifies to two other films, one after
/// the other, carries its watch state when the second leaves it empty: the
/// viewer's place is on one of the films, not retired with the movie.
#[tokio::test]
async fn reclassifying_every_file_of_a_movie_carries_its_watch_state() {
    use beam_domain::models::watch_state::WatchTarget;
    use beam_domain::repositories::WatchStateRepository;

    let h = Harness::keeping_missing_files().await;
    // Its files name two films and neither is the one its key names.
    let emptied = h
        .keyed_movie(
            "Dune",
            None,
            "dune|",
            &["Dune.1984.mkv", "Dune.2021.mkv"],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;
    let viewer = Uuid::new_v4();
    h.watch_state
        .record_progress(played(
            viewer,
            WatchTarget::Movie { movie_id: emptied },
            40.0,
        ))
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let movies = h.movies();
    assert_eq!(movies.len(), 2, "the files are two films: {movies:?}");
    assert!(movies.iter().all(|movie| movie.id != emptied), "retired");
    let mut kept = Vec::new();
    for movie in &movies {
        if let Some(row) = h
            .watch_state
            .find(viewer, WatchTarget::Movie { movie_id: movie.id })
            .await
            .unwrap()
        {
            kept.push(row.position_secs);
        }
    }
    assert_eq!(kept, vec![40.0], "on the film the last file joined");
}

/// A title that keeps another file after one is reclassified away is not
/// retired, and keeps its watch state.
#[tokio::test]
async fn reclassifying_one_of_a_titles_files_leaves_its_watch_state() {
    use beam_domain::models::watch_state::WatchTarget;
    use beam_domain::repositories::WatchStateRepository;

    let h = Harness::keeping_missing_files().await;
    // Its files name two films, so its key is kept, and the scan moves only
    // the 2021 file to a film of its own.
    let kept = h
        .keyed_movie(
            "Dune",
            Some(1984),
            "dune|1984",
            &["Dune.1984.mkv", "Dune.2021.mkv"],
            chrono::Utc::now() - chrono::Duration::days(30),
        )
        .await;
    let viewer = Uuid::new_v4();
    h.watch_state
        .record_progress(played(viewer, WatchTarget::Movie { movie_id: kept }, 40.0))
        .await
        .unwrap();

    h.service
        .scan_all_libraries(ScanTrigger::Periodic)
        .await
        .unwrap();

    let mut movies: Vec<(Uuid, Option<u32>)> =
        h.movies().into_iter().map(|m| (m.id, m.year)).collect();
    movies.sort_by_key(|(_, year)| *year);
    let [(first, Some(1984)), (second, Some(2021))] = movies.as_slice() else {
        panic!("the files are two films: {movies:?}");
    };
    assert_eq!(*first, kept);
    assert_eq!(
        h.watch_state
            .find(viewer, WatchTarget::Movie { movie_id: kept })
            .await
            .unwrap()
            .map(|row| row.position_secs),
        Some(40.0)
    );
    assert!(
        h.watch_state
            .find(viewer, WatchTarget::Movie { movie_id: *second })
            .await
            .unwrap()
            .is_none()
    );
}
