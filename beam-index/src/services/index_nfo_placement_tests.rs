use super::*;

/// A second scan finds a new file whose own NFO names another id than the
/// title's pin. Classification keeps the pin and tells the administrator,
/// and records the NFO as applied, so the same scan's re-apply -- and every
/// later one, however the NFO's mtime moves -- does not then replace the
/// pin with it (FR-219).
#[tokio::test]
async fn a_new_file_whose_nfo_names_another_id_never_replaces_the_titles_pin() {
    let h = Harness::new().await;
    h.video("Heat (1995)/Heat (1995).mkv");
    h.write("Heat (1995)/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

    h.video("Heat (1995)/Heat (1995) - Remux.mkv");
    h.write("Heat (1995)/Heat (1995) - Remux.nfo", &tmdb_movie(1));
    h.scan().await;

    assert_eq!(
        h.movie_of("Heat (1995)/Heat (1995) - Remux.mkv").id,
        h.movie_of("Heat (1995)/Heat (1995).mkv").id,
        "one key, one movie"
    );
    assert_eq!(
        h.movie_pins(),
        vec![Some("tmdb:949".to_string())],
        "the title keeps the pin it had"
    );
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|w| w.contains("tmdb:1,") && w.contains("already pinned to tmdb:949")),
        "the administrator is told which pin was kept: {:?}",
        h.warnings().await
    );

    h.set_mtime(
        "Heat (1995)/Heat (1995) - Remux.nfo",
        std::time::SystemTime::now() + Duration::from_secs(3600),
    );
    h.scan().await;
    assert_eq!(
        h.movie_pins(),
        vec![Some("tmdb:949".to_string())],
        "a newer mtime on unchanged content applies nothing"
    );
}

/// A `tvshow.nfo` at the library root describes no show -- not even one
/// whose episodes sit directly in a series folder below the root, as a
/// season folder's do below a series folder: neither a scan nor its watcher
/// event pins anything with it (FR-219).
#[tokio::test]
async fn a_tvshow_nfo_at_the_library_root_pins_no_show() {
    let h = Harness::new().await;
    h.video("Breaking Bad/Season 01/Breaking.Bad.S01E01.mkv");
    h.video("The Wire/The.Wire.S01E01.mkv");
    h.scan().await;

    h.write("tvshow.nfo", &tmdb_show(1396));
    h.scan().await;
    h.event("tvshow.nfo", FsEventKind::Modified).await;

    assert_eq!(h.show_pins(), vec![None, None]);
}

/// A `tvshow.nfo` describes the episodes in its folder and in the folders
/// one level below it -- a series folder's season folders -- and no deeper:
/// one in a category folder above the series folders pins nothing.
#[tokio::test]
async fn a_tvshow_nfo_two_levels_above_an_episode_pins_no_show() {
    let h = Harness::new().await;
    h.video("TV/Breaking Bad/Season 01/Breaking.Bad.S01E01.mkv");
    h.scan().await;

    h.write("TV/tvshow.nfo", &tmdb_show(1438));
    h.event("TV/tvshow.nfo", FsEventKind::Created).await;
    h.scan().await;

    assert_eq!(h.show_pins(), vec![None]);
}

/// A category folder above flat show folders -- `TV/Lost/Lost.S01E01.mkv` --
/// is not a series folder: the folder above an episode is looked in only when
/// the episode's own folder is a season folder. So a `tvshow.nfo` there,
/// found by the first scan, neither merges the shows beneath it into one
/// title nor pins any of them.
#[tokio::test]
async fn a_tvshow_nfo_in_a_category_folder_above_flat_shows_describes_none_of_them() {
    let h = Harness::new().await;
    h.video("TV/Lost/Lost.S01E01.mkv");
    h.video("TV/The Wire/The.Wire.S01E01.mkv");
    h.write("TV/tvshow.nfo", &tmdb_show(1438));

    h.scan().await;

    let (lost, _, _) = h.show_of("TV/Lost/Lost.S01E01.mkv");
    let (wire, _, _) = h.show_of("TV/The Wire/The.Wire.S01E01.mkv");
    assert_ne!(lost.id, wire.id, "two shows, not one");
    assert_eq!(lost.identity_key.as_deref(), Some("lost|"));
    assert_eq!(h.show_pins(), vec![None, None]);
}

/// The same category `tvshow.nfo`, added after the flat shows beneath it were
/// indexed, pins none of them -- by its watcher event or by a scan.
#[tokio::test]
async fn a_tvshow_nfo_added_to_a_category_folder_above_flat_shows_pins_none_of_them() {
    let h = Harness::new().await;
    h.video("TV/Lost/Lost.S01E01.mkv");
    h.video("TV/The Wire/The.Wire.S01E01.mkv");
    h.scan().await;

    h.write("TV/tvshow.nfo", &tmdb_show(1438));
    h.event("TV/tvshow.nfo", FsEventKind::Created).await;
    h.scan().await;

    assert_eq!(h.show_pins(), vec![None, None]);
}

/// `<stem>.nfo` wins over `movie.nfo` for the video it is named after, so a
/// `movie.nfo` added beside it later re-pins nothing -- by scan or watcher.
#[tokio::test]
async fn a_movie_nfo_never_overrides_a_videos_own_stem_nfo() {
    let h = Harness::new().await;
    h.video("Heat/Heat (1995).mkv");
    h.write("Heat/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;

    h.write("Heat/movie.nfo", &tmdb_movie(2));
    h.scan().await;
    h.event("Heat/movie.nfo", FsEventKind::Modified).await;

    assert_eq!(
        h.movie_of("Heat/Heat (1995).mkv").pinned_ref.as_deref(),
        Some("tmdb:949")
    );
}

/// An NFO edited to an id another title holds is refused by the unique pin
/// and not recorded as applied, so once that title lets the id go the next
/// scan applies it.
#[tokio::test]
async fn an_nfo_whose_pin_another_title_holds_is_tried_again_until_it_applies() {
    let h = Harness::new().await;
    h.video("Heat/Heat.mkv");
    h.write("Heat/Heat.nfo", &tmdb_movie(949));
    h.video("Alien/Alien.mkv");
    h.write("Alien/Alien.nfo", &tmdb_movie(348));
    h.scan().await;
    let applied_before = h.applied("Alien/Alien.nfo").await.expect("recorded");

    h.write("Alien/Alien.nfo", &tmdb_movie(949));
    h.scan().await;
    assert_eq!(
        h.movie_of("Alien/Alien.mkv").pinned_ref.as_deref(),
        Some("tmdb:348"),
        "Heat holds tmdb:949"
    );
    assert_eq!(
        h.applied("Alien/Alien.nfo")
            .await
            .expect("still recorded")
            .content_hash,
        applied_before.content_hash,
        "a refused NFO is not recorded as applied"
    );

    h.write("Heat/Heat.nfo", &tmdb_movie(1));
    h.event("Heat/Heat.nfo", FsEventKind::Modified).await;
    assert_eq!(
        h.movie_of("Heat/Heat.mkv").pinned_ref.as_deref(),
        Some("tmdb:1")
    );
    h.scan().await;

    assert_eq!(
        h.movie_of("Alien/Alien.mkv").pinned_ref.as_deref(),
        Some("tmdb:949"),
        "tried again, and applied once the id was free"
    );
}

/// The warnings that say an NFO describes several titles.
async fn several_titles_warnings(h: &Harness) -> Vec<String> {
    h.warnings()
        .await
        .into_iter()
        .filter(|w| w.contains("An NFO pins several"))
        .collect()
}

/// A `movie.nfo` added to a folder of two already-indexed movies can pin only
/// one of them: the other is refused by the unique pin because its sibling now
/// holds the id -- from this very NFO, so no retry could ever succeed. The NFO
/// is settled at once: recorded after one scan, never read again while
/// unchanged, and the administrator told exactly once.
#[cfg(unix)]
#[tokio::test]
async fn a_movie_nfo_describing_two_movies_is_settled_once_and_the_administrator_told() {
    let h = harness_ahead_of_its_files().await;
    h.video("Collection/Heat (1995).mkv");
    h.video("Collection/Alien (1979).mkv");
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![None, None], "two movies");

    h.write("Collection/movie.nfo", &tmdb_movie(949));
    h.scan().await;

    assert_eq!(
        h.movie_pins(),
        vec![None, Some("tmdb:949".to_string())],
        "an id pins one title"
    );
    let pinned = [
        h.movie_of("Collection/Heat (1995).mkv"),
        h.movie_of("Collection/Alien (1979).mkv"),
    ]
    .into_iter()
    .min_by_key(|movie| movie.id)
    .unwrap()
    .pinned_ref;
    assert_eq!(
        pinned.as_deref(),
        Some("tmdb:949"),
        "the title with the lowest id, the same one every time"
    );
    assert!(
        h.applied("Collection/movie.nfo").await.is_some(),
        "recorded as applied after one scan"
    );
    let warnings = several_titles_warnings(&h).await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("movies") && warnings[0].contains("tmdb:949"));

    let before = nfo_reads();
    h.scan().await;
    h.scan().await;
    assert_eq!(nfo_reads(), before, "a settled NFO is not read again");
    assert_eq!(several_titles_warnings(&h).await.len(), 1, "told once");
    assert_eq!(h.movie_pins(), vec![None, Some("tmdb:949".to_string())]);
}

/// The same for a `tvshow.nfo` added to a flat show folder whose episodes key
/// to two shows.
#[cfg(unix)]
#[tokio::test]
async fn a_tvshow_nfo_describing_two_shows_is_settled_once_and_the_administrator_told() {
    let h = harness_ahead_of_its_files().await;
    h.video("Lost/Lost.S01E01.mkv");
    h.video("Lost/The.Wire.S01E01.mkv");
    h.scan().await;
    assert_eq!(h.show_pins(), vec![None, None], "two shows");

    h.write("Lost/tvshow.nfo", &tmdb_show(4607));
    h.scan().await;

    assert_eq!(
        h.show_pins(),
        vec![None, Some("tmdb:4607".to_string())],
        "an id pins one title"
    );
    assert!(
        h.applied("Lost/tvshow.nfo").await.is_some(),
        "recorded as applied after one scan"
    );
    let warnings = several_titles_warnings(&h).await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("shows") && warnings[0].contains("tmdb:4607"));

    let before = nfo_reads();
    h.scan().await;
    h.scan().await;
    assert_eq!(nfo_reads(), before, "a settled NFO is not read again");
    assert_eq!(several_titles_warnings(&h).await.len(), 1, "told once");
}

/// An edited `<stem>.nfo` re-pins the movie of its own video at the next
/// scan.
#[tokio::test]
async fn an_edited_stem_nfo_repins_its_movie_at_the_next_scan() {
    let h = Harness::new().await;
    h.video("Heat/Heat (1995).mkv");
    h.write("Heat/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;

    h.write("Heat/Heat (1995).nfo", &tmdb_movie(1));
    h.scan().await;

    let movie = h.movie_of("Heat/Heat (1995).mkv");
    assert_eq!(movie.pinned_ref.as_deref(), Some("tmdb:1"));
    let row = h.enrichment_of(EnrichmentTargetId::Movie(movie.id)).await;
    assert!(row.force_refresh, "re-pinned, so fetched afresh");
    assert_eq!(row.matched_ref, None);
}

/// A `tvshow.nfo` added to a series folder after its episodes were indexed
/// pins their show at the next scan -- episodes in every season folder
/// below it -- and an edit of it the watcher sees re-pins the show.
#[tokio::test]
async fn a_tvshow_nfo_added_later_pins_its_show_and_an_edit_repins_it() {
    let h = Harness::new().await;
    h.video("GoT/Season 01/GoT.S01E01.mkv");
    h.video("GoT/Season 02/GoT.S02E01.mkv");
    h.scan().await;
    assert_eq!(h.show_pins(), vec![None]);

    h.write("GoT/tvshow.nfo", &tmdb_show(1399));
    h.scan().await;
    let (show, _, _) = h.show_of("GoT/Season 02/GoT.S02E01.mkv");
    assert_eq!(show.pinned_ref.as_deref(), Some("tmdb:1399"));
    assert!(
        h.enrichment_of(EnrichmentTargetId::Show(show.id))
            .await
            .force_refresh
    );

    h.write("GoT/tvshow.nfo", &tmdb_show(1400));
    h.event("GoT/tvshow.nfo", FsEventKind::Modified).await;
    assert_eq!(h.show_pins(), vec![Some("tmdb:1400".to_string())]);
}

/// An NFO edited before a scan that stamped its start and then died -- or
/// any scan whose start post-dates the edit -- is still applied by the next
/// scan: nothing turns on scan times.
#[tokio::test]
async fn an_nfo_edited_before_a_scan_that_died_is_applied_by_the_next() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.scan().await;
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    // As a scan that started after the edit and was killed would leave it.
    h.library_repo
        .update_scan_progress(
            h.library.id,
            Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            None,
            None,
        )
        .await
        .unwrap();

    h.scan().await;

    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:603")
    );
}
