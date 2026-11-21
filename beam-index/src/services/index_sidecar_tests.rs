use super::*;

/// A subtitle added or removed while a scan holds the library is deferred the
/// same way, and recorded or forgotten by the retry.
#[tokio::test]
async fn a_subtitle_event_while_the_library_is_held_is_deferred_and_applied_on_retry() {
    let h = Harness::new().await;
    h.video("Movie/Movie.mkv");
    h.scan().await;
    h.write("Movie/Movie.fr.srt", "1");

    let held = h
        .service
        .scans
        .try_acquire_for_reconcile(h.library.id)
        .expect("the library is free");
    assert_eq!(
        h.event_outcome("Movie/Movie.fr.srt", FsEventKind::Created)
            .await,
        ReconcileOutcome::Deferred {
            retry_after: LIBRARY_BUSY_RETRY
        }
    );
    assert!(h.subtitles_of("Movie/Movie.mkv").await.is_empty());
    drop(held);
    h.event("Movie/Movie.fr.srt", FsEventKind::Created).await;
    assert_eq!(h.subtitles_of("Movie/Movie.mkv").await.len(), 1);

    std::fs::remove_file(h.root.join("Movie/Movie.fr.srt")).unwrap();
    let held = h
        .service
        .scans
        .try_acquire_for_reconcile(h.library.id)
        .expect("the library is free");
    assert!(matches!(
        h.event_outcome("Movie/Movie.fr.srt", FsEventKind::Removed)
            .await,
        ReconcileOutcome::Deferred { .. }
    ));
    assert_eq!(h.subtitles_of("Movie/Movie.mkv").await.len(), 1);
    drop(held);
    h.event("Movie/Movie.fr.srt", FsEventKind::Removed).await;
    assert!(h.subtitles_of("Movie/Movie.mkv").await.is_empty());
}

#[tokio::test]
async fn subtitles_beside_a_video_are_indexed_as_its_subtitles() {
    let h = Harness::new().await;
    h.video("Movie (2000)/Movie (2000).mkv");
    h.write("Movie (2000)/Movie (2000).en.forced.srt", "1");
    h.write("Movie (2000)/Movie (2000).pt-BR.vtt", "2");
    h.write("Movie (2000)/Subs/English.SDH.ass", "3");
    h.write("Movie (2000)/Other.en.srt", "no video of that name");
    h.write("Movie (2000)/Movie (2000).en.sup", "image-based");

    h.scan().await;

    let subtitles = h.subtitles_of("Movie (2000)/Movie (2000).mkv").await;
    let seen: Vec<(String, SubtitleFormat, Option<String>, bool, bool)> = subtitles
        .iter()
        .map(|s| {
            (
                s.path
                    .strip_prefix(&h.root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                s.info.format,
                s.info.language.clone(),
                s.info.is_forced,
                s.info.is_sdh,
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (
                "Movie (2000)/Movie (2000).en.forced.srt".to_string(),
                SubtitleFormat::Srt,
                Some("eng".to_string()),
                true,
                false
            ),
            (
                "Movie (2000)/Movie (2000).pt-BR.vtt".to_string(),
                SubtitleFormat::Vtt,
                Some("por".to_string()),
                false,
                false
            ),
            (
                "Movie (2000)/Subs/English.SDH.ass".to_string(),
                SubtitleFormat::Ass,
                Some("eng".to_string()),
                false,
                true
            ),
        ]
    );
    assert_eq!(
        h.sidecar_repo.rows.lock().unwrap().len(),
        3,
        "a subtitle no video owns is not indexed"
    );
    let forced = &subtitles[0];
    assert_eq!(forced.size_bytes, 1);
}

#[tokio::test]
async fn a_rescan_updates_a_changed_subtitle_and_deletes_a_vanished_one() {
    let h = Harness::new().await;
    h.video("Movie/Movie.mkv");
    h.write("Movie/Movie.en.srt", "1");
    h.write("Movie/Movie.de.srt", "2");
    h.scan().await;
    let before = h.subtitles_of("Movie/Movie.mkv").await;
    assert_eq!(before.len(), 2);

    std::fs::remove_file(h.root.join("Movie/Movie.de.srt")).unwrap();
    h.write("Movie/Movie.en.srt", "a longer subtitle");
    h.scan().await;

    let after = h.subtitles_of("Movie/Movie.mkv").await;
    assert_eq!(after.len(), 1);
    let english = before
        .iter()
        .find(|s| s.path.ends_with("Movie.en.srt"))
        .unwrap();
    assert_eq!(after[0].id, english.id, "updated in place");
    assert_eq!(after[0].size_bytes, "a longer subtitle".len() as u64);
}

#[tokio::test]
async fn the_watcher_indexes_a_new_subtitle_and_forgets_a_removed_one() {
    let h = Harness::new().await;
    h.video("Movie/Movie.mkv");
    h.scan().await;

    h.write("Movie/Movie.fr.srt", "1");
    h.event("Movie/Movie.fr.srt", FsEventKind::Created).await;
    let added = h.subtitles_of("Movie/Movie.mkv").await;
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].info.language.as_deref(), Some("fre"));

    std::fs::remove_file(h.root.join("Movie/Movie.fr.srt")).unwrap();
    h.event("Movie/Movie.fr.srt", FsEventKind::Removed).await;
    assert!(h.subtitles_of("Movie/Movie.mkv").await.is_empty());
}

#[tokio::test]
async fn a_subtitle_for_a_video_not_yet_indexed_waits_for_the_video() {
    let h = Harness::new().await;
    h.write("Movie/Movie.en.srt", "1");
    h.event("Movie/Movie.en.srt", FsEventKind::Created).await;
    assert!(h.sidecar_repo.rows.lock().unwrap().is_empty());

    h.video("Movie/Movie.mkv");
    h.event("Movie/Movie.mkv", FsEventKind::Created).await;
    assert_eq!(h.subtitles_of("Movie/Movie.mkv").await.len(), 1);
}

/// A row keeps a subtitle's mtime in whole microseconds and the filesystem
/// reports nanoseconds (issue #229): a rescan still finds an untouched
/// subtitle unchanged, and writes nothing for it.
#[tokio::test]
async fn a_rescan_writes_nothing_for_a_subtitle_with_a_sub_microsecond_mtime() {
    use chrono::Timelike;

    let h = Harness::new().await;
    h.video("Movie/Movie.mkv");
    let subtitle = h.write("Movie/Movie.fr.srt", "1");
    h.set_mtime(
        "Movie/Movie.fr.srt",
        std::time::UNIX_EPOCH + Duration::new(1_790_000_000, 123_456_789),
    );
    let on_disk: DateTime<Utc> = std::fs::metadata(&subtitle)
        .unwrap()
        .modified()
        .unwrap()
        .into();
    assert_eq!(
        on_disk.nanosecond(),
        123_456_789,
        "the filesystem keeps the sub-microsecond part, or this proves nothing"
    );
    h.scan().await;

    // Mark the row, so a rewrite -- which stamps it afresh -- shows.
    let marker = DateTime::<Utc>::UNIX_EPOCH;
    h.sidecar_repo
        .rows
        .lock()
        .unwrap()
        .get_mut(&subtitle)
        .expect("the subtitle is recorded")
        .updated_at = marker;
    h.scan().await;

    let rows = h.subtitles_of("Movie/Movie.mkv").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].updated_at, marker,
        "the unchanged row is not rewritten"
    );
}

/// A video the watcher indexes picks up the subtitles already beside it --
/// in its folder and in a `Subs/` folder beside it.
#[tokio::test]
async fn a_video_the_watcher_indexes_picks_up_its_subtitles_and_its_subs_folder() {
    let h = Harness::new().await;
    h.write("Movie (2000)/Movie (2000).en.srt", "1");
    h.write("Movie (2000)/Subs/French.srt", "2");
    h.video("Movie (2000)/Movie (2000).mkv");

    h.event("Movie (2000)/Movie (2000).mkv", FsEventKind::Created)
        .await;

    let languages: Vec<Option<String>> = h
        .subtitles_of("Movie (2000)/Movie (2000).mkv")
        .await
        .into_iter()
        .map(|s| s.info.language)
        .collect();
    assert_eq!(
        languages,
        vec![Some("eng".to_string()), Some("fre".to_string())]
    );
}

/// A subtitle under a folder the walk cannot read is not a subtitle the
/// walk found gone: its record stays, and is still its video's.
#[cfg(unix)]
#[tokio::test]
async fn a_subtitle_under_an_unreadable_folder_is_kept() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::new().await;
    h.video("Movie (2000)/Movie (2000).mkv");
    h.write("Movie (2000)/Subs/Movie (2000).en.srt", "1");
    h.scan().await;
    assert_eq!(
        h.subtitles_of("Movie (2000)/Movie (2000).mkv").await.len(),
        1
    );

    let locked = h.root.join("Movie (2000)/Subs");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // Running as root: permissions do not bind, so the walk cannot fail.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: running as root, which ignores file permissions");
        return;
    }
    let scanned = h.service.scan_library(h.library.id).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    scanned.unwrap();

    let kept = h.subtitles_of("Movie (2000)/Movie (2000).mkv").await;
    assert_eq!(
        kept.iter()
            .map(|s| s.path.strip_prefix(&h.root).unwrap().to_path_buf())
            .collect::<Vec<_>>(),
        [PathBuf::from("Movie (2000)/Subs/Movie (2000).en.srt")],
        "the unreadable folder's subtitle is kept"
    );
}

/// The watcher relinking a moved video, before it hears of the subtitle that
/// moved with it, records the subtitle beside the video's new path and
/// forgets the row of the one no longer beside it; the subtitle's and the
/// NFO's own events then change nothing further, and the title keeps its
/// pin.
#[tokio::test]
async fn the_watcher_relinks_a_moved_video_with_the_subtitles_beside_its_new_path() {
    let (h, id) = a_moved_movie_folder().await;

    h.event("Heat (1995)/Heat.mkv", FsEventKind::Created).await;

    assert_eq!(
        h.file("Heat (1995)/Heat.mkv").id,
        id,
        "relinked, not re-added"
    );
    let subtitle_paths = || async {
        h.subtitles_of("Heat (1995)/Heat.mkv")
            .await
            .into_iter()
            .map(|row| row.path)
            .collect::<Vec<PathBuf>>()
    };
    assert_eq!(
        subtitle_paths().await,
        vec![h.root.join("Heat (1995)/Heat.en.srt")],
        "the subtitle beside the new path, and not the one gone from the old"
    );

    h.event("Heat/Heat.en.srt", FsEventKind::Removed).await;
    h.event("Heat (1995)/Heat.en.srt", FsEventKind::Created)
        .await;
    h.event("Heat/Heat.nfo", FsEventKind::Removed).await;
    h.event("Heat (1995)/Heat.nfo", FsEventKind::Created).await;

    assert_eq!(
        subtitle_paths().await,
        vec![h.root.join("Heat (1995)/Heat.en.srt")]
    );
    assert!(h.applied("Heat/Heat.nfo").await.is_none());
    assert!(h.applied("Heat (1995)/Heat.nfo").await.is_some());
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}
