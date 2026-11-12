use super::*;

#[tokio::test]
async fn container_tags_place_an_episode_the_path_could_not() {
    let h = Harness::new().await;
    h.video("The Office/The Dundies.m4v");
    h.tag(
        "The Office/The Dundies.m4v",
        &[
            ("show", "The Office"),
            ("season_number", "2"),
            ("episode_sort", "1"),
        ],
    );

    h.scan().await;

    let (show, season, episode) = h.show_of("The Office/The Dundies.m4v");
    assert_eq!((season, episode), (2, 1));
    assert_eq!(show.identity_key.as_deref(), Some("the office|"));
}

/// A file placed only by its container tags keeps its place when a
/// classifier-version bump reclassifies it: its probe stored the tags with
/// it, and the reclassification reads them rather than probing again.
#[tokio::test]
async fn a_file_placed_by_its_tags_keeps_its_place_when_it_is_reclassified() {
    let h = Harness::new().await;
    h.video("The Office/The Dundies.m4v");
    h.tag(
        "The Office/The Dundies.m4v",
        &[
            ("show", "The Office"),
            ("season_number", "2"),
            ("episode_sort", "1"),
        ],
    );
    h.scan().await;
    let (placed, _, _) = h.show_of("The Office/The Dundies.m4v");

    // As the next classifier version would find the row; and a re-probe
    // would no longer see the tags, so only the stored ones can place it.
    h.tags.lock().unwrap().clear();
    let id = h.file("The Office/The Dundies.m4v").id;
    h.file_repo
        .files
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .classifier_version = CLASSIFIER_VERSION - 1;
    h.scan().await;

    let file = h.file("The Office/The Dundies.m4v");
    assert_eq!(file.classifier_version, CLASSIFIER_VERSION, "reclassified");
    let (show, season, episode) = h.show_of("The Office/The Dundies.m4v");
    assert_eq!((season, episode), (2, 1));
    assert_eq!(show.id, placed.id, "the same show as before");
}

/// A text tag longer than Beam keeps is stored cut to the cap, on a character
/// boundary: one file cannot store an unbounded value with its row.
#[tokio::test]
async fn a_container_tag_longer_than_the_cap_is_stored_cut_to_it() {
    use beam_domain::utils::classification::MAX_TAG_VALUE_BYTES;

    let h = Harness::new().await;
    h.video("The Office/The Dundies.m4v");
    // Three-byte characters, so the cap falls inside one.
    let show = "\u{4e2d}".repeat(MAX_TAG_VALUE_BYTES);
    h.tag(
        "The Office/The Dundies.m4v",
        &[
            ("show", show.as_str()),
            ("season_number", "2"),
            ("episode_sort", "1"),
        ],
    );

    h.scan().await;

    let stored = h
        .file("The Office/The Dundies.m4v")
        .container_tags
        .expect("the probe's tags are stored")
        .show
        .expect("the show tag is kept");
    assert_eq!(
        stored,
        "\u{4e2d}".repeat(MAX_TAG_VALUE_BYTES / 3),
        "cut to the whole characters that fit"
    );
}

/// A file whose first probe failed is classified when a later probe succeeds
/// (issue #181) -- and by the tags that probe read, exactly as a new file
/// would be, not by its path alone.
#[tokio::test]
async fn a_reprobe_that_succeeds_classifies_by_the_tags_it_read() {
    let h = Harness::new().await;
    h.video("The Office/The Dundies.m4v");
    h.tag(
        "The Office/The Dundies.m4v",
        &[
            ("show", "The Office"),
            ("season_number", "2"),
            ("episode_sort", "1"),
        ],
    );
    h.probe_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    h.scan().await;
    assert_eq!(h.file("The Office/The Dundies.m4v").content, None);

    h.probe_fails
        .store(false, std::sync::atomic::Ordering::SeqCst);
    h.scan().await;

    let (show, season, episode) = h.show_of("The Office/The Dundies.m4v");
    assert_eq!((season, episode), (2, 1));
    assert_eq!(show.identity_key.as_deref(), Some("the office|"));
}

/// A probe records the chapters it read (issue #265), by position, and the
/// next probe of the file replaces them; a file whose content changed and
/// no longer probes is left with none, since they described the old content.
#[tokio::test]
async fn a_probe_records_the_chapters_it_read_and_a_failed_reprobe_clears_them() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.chapter(
        "Matrix/matrix.mkv",
        &[(0, 90_000, Some("Opening")), (90_000, 600_000, None)],
    );

    h.scan().await;

    assert_eq!(
        h.chapters_of("Matrix/matrix.mkv").await,
        vec![
            Chapter {
                index: 0,
                start_ms: 0,
                end_ms: 90_000,
                title: Some("Opening".into()),
            },
            Chapter {
                index: 1,
                start_ms: 90_000,
                end_ms: 600_000,
                title: None,
            },
        ]
    );

    // A remux: new content, one chapter.
    h.write("Matrix/matrix.mkv", "Matrix/matrix.mkv, remuxed");
    h.chapter("Matrix/matrix.mkv", &[(0, 600_000, Some("Whole"))]);
    h.scan().await;
    assert_eq!(
        h.chapters_of("Matrix/matrix.mkv").await,
        vec![Chapter {
            index: 0,
            start_ms: 0,
            end_ms: 600_000,
            title: Some("Whole".into()),
        }]
    );

    // Changed again, and now it does not probe.
    h.write("Matrix/matrix.mkv", "Matrix/matrix.mkv, half-copied over");
    h.probe_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    h.scan().await;
    assert!(h.chapters_of("Matrix/matrix.mkv").await.is_empty());
}

/// A library with everything the scan reads beside the media.
fn furnished(h: &Harness) {
    h.video("Matrix/matrix.1080p.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.write("Matrix/matrix.1080p.en.srt", "1");
    h.write("Matrix/Subs/French.srt", "2");
    h.video("GoT/Season 01/GoT.S01E01.mkv");
    h.write(
        "GoT/tvshow.nfo",
        "<tvshow><title>Game of Thrones</title></tvshow>",
    );
    h.write(
        "GoT/Season 01/GoT.S01E01.nfo",
        "<episodedetails><season>1</season><episode>1</episode></episodedetails>",
    );
    h.write("GoT/Season 01/GoT.S01E01.eng.forced.ass", "3");
}

/// Everything the indexer does with a furnished library: the watcher
/// indexes a new video -- attaching the subtitles beside it and in its
/// `Subs/` folder -- then two scans, and watcher events for an NFO and a
/// subtitle.
async fn index_furnished(h: &Harness) {
    h.event("Matrix/matrix.1080p.mkv", FsEventKind::Created)
        .await;
    h.scan().await;
    h.scan().await;
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;
    h.event("Matrix/matrix.1080p.en.srt", FsEventKind::Modified)
        .await;
}

async fn assert_furnished_indexed(h: &Harness) {
    assert_eq!(
        h.movie_of("Matrix/matrix.1080p.mkv").pinned_ref.as_deref(),
        Some("tmdb:603")
    );
    assert_eq!(h.subtitles_of("Matrix/matrix.1080p.mkv").await.len(), 2);
    assert_eq!(
        h.subtitles_of("GoT/Season 01/GoT.S01E01.mkv").await.len(),
        1
    );
    assert_eq!(
        h.show_of("GoT/Season 01/GoT.S01E01.mkv").0.title,
        "Game of Thrones"
    );
}

/// A library holding a real container -- opened by the real FFmpeg prober,
/// tags and all, and hashed by the real hasher -- with an NFO and a subtitle
/// in a `Subs/` folder beside it.
fn furnished_with_a_real_container(h: &Harness) {
    let clip = h.root.join("Clip (2020)/Clip (2020).mkv");
    std::fs::create_dir_all(clip.parent().unwrap()).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/h264.mkv"),
        &clip,
    )
    .unwrap();
    h.write(
        "Clip (2020)/Clip (2020).nfo",
        r#"<movie><uniqueid type="tmdb">949</uniqueid></movie>"#,
    );
    h.write("Clip (2020)/Subs/English.srt", "1");
}

async fn index_the_real_container(h: &Harness) {
    h.event("Clip (2020)/Clip (2020).mkv", FsEventKind::Created)
        .await;
    h.scan().await;
    h.scan().await;
    h.event("Clip (2020)/Clip (2020).nfo", FsEventKind::Modified)
        .await;
}

async fn assert_the_real_container_indexed(h: &Harness) {
    let file = h.file("Clip (2020)/Clip (2020).mkv");
    assert_eq!(file.status, FileStatus::Known, "probed by the real prober");
    assert!(file.duration.is_some());
    assert_eq!(
        h.movie_of("Clip (2020)/Clip (2020).mkv")
            .pinned_ref
            .as_deref(),
        Some("tmdb:949")
    );
    assert_eq!(h.subtitles_of("Clip (2020)/Clip (2020).mkv").await.len(), 1);
}

#[tokio::test]
async fn a_scan_never_writes_into_the_library_root() {
    let h = Harness::new().await;
    furnished(&h);
    let before = snapshot(&h.root);

    index_furnished(&h).await;

    assert_furnished_indexed(&h).await;
    assert_eq!(
        snapshot(&h.root),
        before,
        "the library tree is byte-for-byte unchanged"
    );
}

/// The real prober opens the container to read its streams and tags, and
/// the real hasher reads it whole: neither writes a byte either.
#[tokio::test]
async fn probing_and_hashing_a_real_container_never_write_into_the_library_root() {
    let h = Harness::with_real_prober().await;
    furnished_with_a_real_container(&h);
    let before = snapshot(&h.root);

    index_the_real_container(&h).await;

    assert_the_real_container_indexed(&h).await;
    assert_eq!(
        snapshot(&h.root),
        before,
        "the library tree is byte-for-byte unchanged"
    );
}

/// Run `indexing` with every write permission under `h`'s library removed,
/// restoring them after. `false` -- having run nothing, and said why -- when
/// the test runs as root, which ignores permissions, so the premise of a
/// read-only library does not hold.
#[cfg(unix)]
async fn with_the_library_read_only(h: &Harness, indexing: impl Future<Output = ()>) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let set_mode = |mode: u32| {
        for entry in walkdir::WalkDir::new(&h.root).contents_first(true) {
            let entry = entry.unwrap();
            let mode = if entry.file_type().is_dir() {
                mode | 0o111
            } else {
                mode
            };
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        }
    };
    set_mode(0o444);
    let root_user = std::fs::File::create(h.root.join("probe")).is_ok();
    if root_user {
        set_mode(0o755);
        eprintln!(
            "skipped: running as root, which ignores file permissions, so a read-only \
             library cannot be made"
        );
        return false;
    }
    indexing.await;
    set_mode(0o755);
    true
}

/// A read-only library -- a `:ro` mount, a share the server may not write --
/// indexes exactly as a writable one: nothing the scan does needs a write.
#[cfg(unix)]
#[tokio::test]
async fn a_read_only_library_is_indexed_in_full() {
    let h = Harness::new().await;
    furnished(&h);
    if with_the_library_read_only(&h, index_furnished(&h)).await {
        assert_furnished_indexed(&h).await;
    }
}

/// The same, with the real prober and hasher opening a real container.
#[cfg(unix)]
#[tokio::test]
async fn a_real_container_in_a_read_only_library_is_indexed_in_full() {
    let h = Harness::with_real_prober().await;
    furnished_with_a_real_container(&h);
    if with_the_library_read_only(&h, index_the_real_container(&h)).await {
        assert_the_real_container_indexed(&h).await;
    }
}
