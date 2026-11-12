use super::*;

#[tokio::test]
async fn an_nfo_pins_the_new_movie_shows_its_title_and_queues_a_fetch_by_the_pin() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.1080p.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);

    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.1080p.mkv");
    assert_eq!(movie.pinned_ref.as_deref(), Some("tmdb:603"));
    assert_eq!(movie.title, "The Matrix", "the NFO names the new movie");
    assert_eq!(movie.year, Some(1999));
    assert_eq!(
        movie.identity_key.as_deref(),
        Some("matrix|"),
        "the key is the path's"
    );
    let row = h.enrichment_of(EnrichmentTargetId::Movie(movie.id)).await;
    assert!(row.force_refresh, "the pinned movie is fetched afresh");
    assert_eq!(row.matched_ref, None, "by its pin, not an old match");
}

/// An NFO that is there but does not parse is ignored: the movie is the
/// one its path names, with no pin -- and once the NFO is fixed, the next
/// scan applies it.
#[tokio::test]
async fn an_nfo_that_does_not_parse_is_ignored_until_it_is_fixed() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.1080p.mkv");
    h.write(
        "Matrix/movie.nfo",
        r#"<movie><title>The Matrix</title><uniqueid type="tmdb">603"#,
    );

    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.1080p.mkv");
    assert_eq!(movie.pinned_ref, None, "a broken NFO pins nothing");
    assert_ne!(movie.title, "The Matrix", "nor names the title");
    assert_eq!(movie.identity_key.as_deref(), Some("matrix|"));

    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;

    let fixed = h.movie_of("Matrix/matrix.1080p.mkv");
    assert_eq!(fixed.id, movie.id, "the same title, now pinned");
    assert_eq!(fixed.pinned_ref.as_deref(), Some("tmdb:603"));
}

#[tokio::test]
async fn a_second_file_whose_nfo_names_the_pin_joins_the_pinned_movie() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.1080p.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.video("Elsewhere/The Matrix 4K.mkv");
    h.write("Elsewhere/The Matrix 4K.nfo", MATRIX_NFO);

    h.scan().await;

    assert_eq!(
        h.movie_of("Matrix/matrix.1080p.mkv").id,
        h.movie_of("Elsewhere/The Matrix 4K.mkv").id,
        "two paths keyed differently, one pinned title"
    );
    assert_eq!(h.movie_repo.movies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_title_keeps_its_pin_when_another_nfo_names_another_id() {
    let h = Harness::new().await;
    h.video("Heat (1995)/Heat (1995).mkv");
    h.write(
        "Heat (1995)/Heat (1995).nfo",
        r#"<movie><uniqueid type="tmdb">949</uniqueid></movie>"#,
    );
    h.video("Heat (1995)/Heat (1995) - Remux.mkv");
    h.write(
        "Heat (1995)/Heat (1995) - Remux.nfo",
        r#"<movie><uniqueid type="tmdb">1</uniqueid></movie>"#,
    );

    h.scan().await;

    let pins = h.movie_pins();
    assert_eq!(pins.len(), 1, "one key, one movie: {pins:?}");
    assert!(pins[0].is_some());
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|w| w.contains("already pinned")),
        "the administrator is told the NFOs disagree"
    );
}

#[tokio::test]
async fn an_episode_nfo_makes_an_episode_of_a_movie_looking_name() {
    let h = Harness::new().await;
    h.video("Lost/Pilot Part One.mkv");
    h.write(
        "Lost/Pilot Part One.nfo",
        "<episodedetails><title>Pilot (1)</title><season>1</season><episode>1</episode>\
         </episodedetails>",
    );

    h.scan().await;

    let (show, season, episode) = h.show_of("Lost/Pilot Part One.mkv");
    assert_eq!((season, episode), (1, 1));
    assert_eq!(show.identity_key.as_deref(), Some("lost|"));
    assert!(h.movie_repo.movies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_tvshow_nfo_names_and_pins_the_show_its_folder_keys() {
    let h = Harness::new().await;
    h.video("GoT/Season 01/GoT.S01E01.mkv");
    h.write(
        "GoT/tvshow.nfo",
        r#"<tvshow><title>Game of Thrones</title><year>2011</year>
           <uniqueid type="tmdb" default="true">1399</uniqueid></tvshow>"#,
    );

    h.scan().await;

    let (show, _, _) = h.show_of("GoT/Season 01/GoT.S01E01.mkv");
    assert_eq!(show.title, "Game of Thrones");
    assert_eq!(show.year, Some(2011));
    assert_eq!(show.identity_key.as_deref(), Some("got|"));
    assert_eq!(show.pinned_ref.as_deref(), Some("tmdb:1399"));
}

#[tokio::test]
async fn an_nfo_larger_than_beam_reads_is_ignored() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    let padding = " ".repeat(beam_domain::utils::nfo::MAX_NFO_BYTES as usize);
    h.write("Matrix/movie.nfo", &format!("{MATRIX_NFO}{padding}"));

    h.scan().await;

    assert_eq!(h.movie_of("Matrix/matrix.mkv").pinned_ref, None);
}

#[cfg(unix)]
#[tokio::test]
async fn an_nfo_that_is_a_symlink_is_not_read() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    let outside = h._dir.path().join("outside.nfo");
    std::fs::write(&outside, MATRIX_NFO).unwrap();
    std::os::unix::fs::symlink(&outside, h.root.join("Matrix/movie.nfo")).unwrap();

    h.scan().await;

    assert_eq!(h.movie_of("Matrix/matrix.mkv").pinned_ref, None);
}

/// A folder above an NFO that is a link out of the library is not followed
/// either (FR-212): the NFO of that name outside is never read, while one
/// reached through a real folder is.
#[cfg(unix)]
#[test]
fn an_nfo_beneath_a_folder_that_is_a_symlink_is_not_read() {
    let outside = TempDir::new().unwrap();
    std::fs::write(outside.path().join("movie.nfo"), MATRIX_NFO).unwrap();
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("Matrix")).unwrap();
    std::fs::write(root.path().join("Matrix/movie.nfo"), MATRIX_NFO).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("Linked")).unwrap();

    assert!(hints::read_nfo_file(root.path(), &root.path().join("Matrix/movie.nfo")).is_some());
    assert!(hints::read_nfo_file(root.path(), &root.path().join("Linked/movie.nfo")).is_none());
}

/// An NFO added after its video was indexed pins the title at the next scan
/// -- even one copied in with its old modification time kept (`cp -p`,
/// `rsync -a`), or from a NAS whose clock runs behind the server's: whether
/// it is applied turns on what it holds, never on when it says it was
/// written.
#[tokio::test]
async fn an_nfo_added_after_indexing_pins_the_movie_on_the_next_scan() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.scan().await;
    assert_eq!(h.movie_of("Matrix/matrix.mkv").pinned_ref, None);

    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.set_mtime(
        "Matrix/movie.nfo",
        std::time::SystemTime::now() - Duration::from_secs(365 * 24 * 3600),
    );
    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(movie.pinned_ref.as_deref(), Some("tmdb:603"));
    assert!(
        h.enrichment_of(EnrichmentTargetId::Movie(movie.id))
            .await
            .force_refresh
    );
}

#[tokio::test]
async fn an_edited_nfo_repins_its_movie_when_the_watcher_sees_it() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;

    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;

    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604"),
        "the edited NFO's id replaces the one it set"
    );
}

/// A row a Postgres `FileRepository` returns holds its file's mtime in whole
/// microseconds, while the filesystem reports nanoseconds (issue #229). The
/// video beside an edited NFO is unmoved, so the NFO still re-pins on its
/// watcher event rather than being left to the next scan as though the video
/// might have moved.
#[tokio::test]
async fn an_edited_nfo_repins_on_its_event_beside_a_row_kept_to_microseconds() {
    use chrono::Timelike;

    let h = Harness::new().await;
    let video = h.video("Matrix/matrix.mkv");
    h.set_mtime(
        "Matrix/matrix.mkv",
        std::time::UNIX_EPOCH + Duration::new(1_790_000_000, 123_456_789),
    );
    let on_disk: DateTime<Utc> = std::fs::metadata(&video)
        .unwrap()
        .modified()
        .unwrap()
        .into();
    assert_eq!(
        on_disk.nanosecond(),
        123_456_789,
        "the filesystem keeps the sub-microsecond part, or this proves nothing"
    );
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;

    // The row exactly as `files.mtime`, a TIMESTAMPTZ, reads back.
    let at_micros = on_disk.with_nanosecond(123_456_000).unwrap();
    let row = h
        .file_repo
        .find_by_path(&video.to_string_lossy())
        .await
        .unwrap()
        .expect("the video is indexed");
    h.file_repo
        .update(UpdateMediaFile {
            id: row.id,
            hash: None,
            size_bytes: None,
            mtime: Some(at_micros),
            identity: None,
            probe: ProbeUpdate::Keep,
            content: None,
            status: None,
        })
        .await
        .unwrap();

    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;

    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604"),
        "the edit re-pins on its event, not at the next scan"
    );
}

/// An administrator's pin is a decision made in Beam: no NFO -- edited, or
/// named by a new file -- replaces it (FR-312).
#[tokio::test]
async fn an_nfo_never_replaces_an_administrators_pin() {
    use beam_domain::models::PinSource;
    use beam_domain::models::ProviderPin;
    use beam_domain::repositories::MovieRepository;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    assert!(
        h.movie_repo
            .set_pinned_ref(movie.id, &ProviderPin::Tmdb(624860), PinSource::Admin)
            .await
            .unwrap()
    );

    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;
    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(
        (movie.pinned_ref.as_deref(), movie.pin_source),
        (Some("tmdb:624860"), Some(PinSource::Admin)),
        "the administrator's pin stands"
    );
}

fn tmdb_show(id: u32) -> String {
    format!(r#"<tvshow><uniqueid type="tmdb">{id}</uniqueid></tvshow>"#)
}

/// A harness whose clock runs an hour ahead of the files it writes, so a scan
/// records every NFO's stat stamp and a later scan with the same stamp reads
/// no NFO bytes.
async fn harness_ahead_of_its_files() -> Harness {
    let clock = Arc::new(beam_domain::services::TestClock::starting_at(
        chrono::Utc::now() + chrono::Duration::hours(1),
    ));
    Harness::build(Probe::Double, clock).await
}

/// How many NFOs this test's thread has read the bytes of.
fn nfo_reads() -> usize {
    hints::NFO_READS.with(std::cell::Cell::get)
}

/// An NFO against what an administrator set: a fixed match outranks
/// it, and clearing the match asks the NFO again.
#[path = "index_nfo_admin_tests.rs"]
mod admin;

/// Which title an NFO describes: where it sits, which NFO wins, and an
/// NFO that describes several titles.
#[path = "index_nfo_placement_tests.rs"]
mod placement;

/// What is recorded of each NFO read (issue #184), and when that spares
/// the next scan a read.
#[path = "index_nfo_record_tests.rs"]
mod records;

/// NFOs and their videos moved, swapped and rotated: the pin follows the content.
#[path = "index_nfo_move_tests.rs"]
mod moves;
