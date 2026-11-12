use super::*;

/// A renamed file is the same row: same id, same title, no probe, and
/// nothing marked missing, since the row moved with the file.
#[tokio::test]
async fn a_renamed_file_keeps_its_row_through_a_scan() {
    let h = Harness::new().await;
    let old = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&old).await;
    assert!(before.content.is_some(), "classified as a movie");
    let probes = h.probes();

    let new = h.mv(&old, "Heat.1995.1080p.mkv");
    let progress = h.scan().await;

    assert_eq!(
        (progress.relinked, progress.added, progress.marked_missing),
        (1, 0, 0)
    );
    let after = h.present(&new).await;
    assert_eq!(after.id, before.id);
    assert_eq!(after.content, before.content, "the title is kept");
    assert_eq!(
        (after.duration, after.status, after.classifier_version),
        (before.duration, before.status, before.classifier_version)
    );
    assert_eq!(
        (after.hash, after.size_bytes),
        (before.hash, before.size_bytes),
        "the row carries the content it followed"
    );
    assert_eq!(after.size_bytes, std::fs::metadata(&new).unwrap().len());
    assert!(
        h.row(&old).await.is_none(),
        "nothing is left at the old path"
    );
    assert_eq!(h.probes(), probes, "a relinked file is not probed");
    assert_eq!(h.row_count().await, 1);

    let logs = h.move_logs().await;
    assert_eq!(logs.len(), 1, "the move is logged once");
    let details = logs[0].details.as_ref().unwrap();
    assert_eq!(details["file_id"], serde_json::json!(before.id.to_string()));
    assert_eq!(
        details["from"],
        serde_json::json!(old.display().to_string())
    );
    assert_eq!(details["to"], serde_json::json!(new.display().to_string()));
    let completion = h
        .admin_log_repo
        .list(100, 0)
        .await
        .unwrap()
        .into_iter()
        .find(|log| log.message.contains("scan completed"))
        .and_then(|log| log.details)
        .unwrap();
    assert_eq!(completion["relinked"], serde_json::json!(1));
}

#[tokio::test]
async fn a_file_moved_to_another_directory_keeps_its_row() {
    let h = Harness::new().await;
    let old = h.write("Heat (1995)/Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&old).await;

    let new = h.mv(&old, "Crime/Heat (1995).mkv");
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (1, 0));
    assert_eq!(h.present(&new).await.id, before.id);
    assert!(h.row(&old).await.is_none());
}

/// Moved out of the library a file is missing, not deleted; moved back --
/// to its old path or to a new one -- it is the same row again.
#[tokio::test]
async fn a_file_moved_out_and_back_in_is_the_same_row() {
    let h = Harness::new().await;
    let old = h.write("Heat (1995).mkv", "heat");
    h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let before = h.present(&old).await;
    let outside = h.dir.path().join("outside/Heat (1995).mkv");

    std::fs::rename(&old, &outside).unwrap();
    let progress = h.scan().await;
    assert_eq!(progress.marked_missing, 1);
    let missing = h.row(&old).await.expect("marked missing, not deleted");
    assert!(missing.missing_since.is_some());

    let back = h.mv(&outside, "Films/Heat (1995).mkv");
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (1, 0));
    assert_eq!(h.present(&back).await.id, before.id, "visible again");
}

/// A copy leaves the original where it is: the copy is a new file, and a
/// duplicate of it.
#[tokio::test]
async fn a_copied_file_is_a_new_row_and_a_duplicate() {
    let h = Harness::new().await;
    let original = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&original).await;

    let copy = h.write("Backup/Heat (1995).mkv", "heat");
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (0, 1));
    assert_eq!(h.present(&original).await.id, before.id);
    assert_ne!(h.present(&copy).await.id, before.id);
    assert!(
        h.notifications
            .published_events()
            .iter()
            .any(|event| event.message.starts_with("Duplicate content")),
        "the copy is reported as a duplicate"
    );
}

/// A file moved into another library is that library's new file; the row
/// it left behind is not relinked across libraries.
#[tokio::test]
async fn a_file_moved_to_another_library_is_not_relinked_across() {
    let h = Harness::new().await;
    let old = h.write("Heat (1995).mkv", "heat");
    h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let before = h.present(&old).await;
    let other_root = h.dir.path().join("other");
    std::fs::create_dir_all(&other_root).unwrap();
    let other = h
        .library_repo
        .create(CreateLibrary {
            name: "Other".to_string(),
            root_path: other_root.clone(),
            description: None,
        })
        .await
        .unwrap();

    let moved = other_root.join("Heat (1995).mkv");
    std::fs::rename(&old, &moved).unwrap();
    let progress = h.scan_library(other.id).await;

    assert_eq!((progress.relinked, progress.added), (0, 1));
    let theirs = h.present(&moved).await;
    assert_ne!(theirs.id, before.id);
    assert_eq!(theirs.library_id, other.id);
    assert_eq!(
        h.row(&old).await.map(|row| row.id),
        Some(before.id),
        "the old library's row is left for its own scan"
    );
}

/// Two copies of one file moved together each keep their own row, matched
/// by name.
#[tokio::test]
async fn identical_files_moved_together_keep_their_own_rows() {
    let h = Harness::new().await;
    let one = h.write("Copies/One.mkv", "same");
    let two = h.write("Copies/Two.mkv", "same");
    h.scan().await;
    let (one_id, two_id) = (h.present(&one).await.id, h.present(&two).await.id);

    std::fs::rename(h.root.join("Copies"), h.root.join("Moved")).unwrap();
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (2, 0));
    assert_eq!(h.present(&h.root.join("Moved/One.mkv")).await.id, one_id);
    assert_eq!(h.present(&h.root.join("Moved/Two.mkv")).await.id, two_id);
}

/// The watcher reports the two halves of a rename as two events, in no
/// particular order. Either way the new path keeps the row.
#[tokio::test]
async fn a_renamed_file_keeps_its_row_whichever_watcher_event_comes_first() {
    for old_first in [true, false] {
        let h = Harness::new().await;
        let old = h.write("Heat (1995).mkv", "heat");
        h.scan().await;
        let before = h.present(&old).await;
        let probes = h.probes();

        let new = h.mv(&old, "Heat (1995) 1080p.mkv");
        let (first, second) = if old_first {
            ((&old, FsEventKind::Removed), (&new, FsEventKind::Created))
        } else {
            ((&new, FsEventKind::Created), (&old, FsEventKind::Removed))
        };
        assert_eq!(h.reconcile(first.0, first.1).await, ReconcileOutcome::Done);
        assert_eq!(
            h.reconcile(second.0, second.1).await,
            ReconcileOutcome::Done
        );

        let after = h.present(&new).await;
        assert_eq!(after.id, before.id, "old first: {old_first}");
        assert_eq!(
            (after.hash, after.size_bytes),
            (before.hash, before.size_bytes),
            "old first: {old_first}"
        );
        assert!(h.row(&old).await.is_none());
        assert_eq!(h.probes(), probes);
        assert_eq!(h.row_count().await, 1);
    }
}

/// A watcher sees a copy as a new file beside the original.
#[tokio::test]
async fn a_watcher_event_for_a_copy_indexes_a_new_row() {
    let h = Harness::new().await;
    let original = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&original).await;

    let copy = h.write("Heat (1995) copy.mkv", "heat");
    h.reconcile(&copy, FsEventKind::Created).await;

    assert_ne!(h.present(&copy).await.id, before.id);
    assert_eq!(h.present(&original).await.id, before.id);
}

/// A row read back at microsecond precision still matches its file when the
/// filesystem keeps nanoseconds: the original has not moved, so the
/// watcher indexes a copy of it rather than leaving the copy to the scan
/// (issue #229).
#[tokio::test]
async fn a_row_at_microsecond_precision_still_matches_its_file_for_the_watcher() {
    let h = Harness::new().await;
    let original = h.write("Heat (1995).mkv", "heat");
    std::fs::File::options()
        .write(true)
        .open(&original)
        .unwrap()
        .set_modified(std::time::SystemTime::from(
            instant(-60) + chrono::TimeDelta::nanoseconds(802_029_432),
        ))
        .unwrap();
    h.scan().await;
    let before = h.present(&original).await;
    assert_eq!(
        before.mtime,
        Some(instant(-60) + chrono::TimeDelta::microseconds(802_029)),
        "the row keeps microseconds"
    );

    let copy = h.write("Heat (1995) copy.mkv", "heat");
    let outcome = h.reconcile(&copy, FsEventKind::Created).await;

    assert_eq!(outcome, ReconcileOutcome::Done);
    assert_ne!(h.present(&copy).await.id, before.id, "indexed as a new row");
    assert_eq!(h.present(&original).await.id, before.id);
}

/// A renamed season folder arrives as an event for each directory name. In
/// either order every episode keeps its row, and nothing is left missing.
#[tokio::test]
async fn a_renamed_directory_keeps_its_files_rows_in_either_event_order() {
    for old_first in [true, false] {
        let h = Harness::new().await;
        let e1 = h.write("Show/S01/Show S01E01.mkv", "one");
        let e2 = h.write("Show/S01/Show S01E02.mkv", "two");
        h.scan().await;
        let (id1, id2) = (h.present(&e1).await.id, h.present(&e2).await.id);
        let probes = h.probes();

        let old_dir = h.root.join("Show/S01");
        let new_dir = h.root.join("Show/Season 1");
        std::fs::rename(&old_dir, &new_dir).unwrap();
        let events = if old_first {
            [
                (&old_dir, FsEventKind::Modified),
                (&new_dir, FsEventKind::Modified),
            ]
        } else {
            [
                (&new_dir, FsEventKind::Modified),
                (&old_dir, FsEventKind::Modified),
            ]
        };
        for (path, kind) in events {
            assert_eq!(h.reconcile(path, kind).await, ReconcileOutcome::Done);
        }

        let moved1 = h.present(&new_dir.join("Show S01E01.mkv")).await;
        let moved2 = h.present(&new_dir.join("Show S01E02.mkv")).await;
        assert_eq!((moved1.id, moved2.id), (id1, id2), "old first: {old_first}");
        assert_eq!(h.row_count().await, 2, "no row left behind");
        assert_eq!(h.probes(), probes, "nothing probed");
    }
}

/// A removed directory marks the rows beneath it missing -- by whole path
/// components, so `S1` is not `S10` -- and deletes none.
#[tokio::test]
async fn a_removed_directory_marks_only_its_own_files_missing() {
    let h = Harness::new().await;
    let inside = h.write("Show/S1/Show S01E01.mkv", "one");
    let sibling = h.write("Show/S10/Show S10E01.mkv", "ten");
    h.scan().await;

    std::fs::remove_dir_all(h.root.join("Show/S1")).unwrap();
    h.reconcile(&h.root.join("Show/S1"), FsEventKind::Removed)
        .await;

    let gone = h.row(&inside).await.expect("kept, not deleted");
    assert!(gone.missing_since.is_some());
    h.present(&sibling).await;
}

/// Relinking a moved file, and reconciling a renamed directory and a removed
/// one, write nothing into the library (FR-202). Each step is a move the
/// test makes and then the indexer's reaction to it -- the watcher's events
/// and a scan -- and the tree after the reaction is the tree the move left.
#[tokio::test]
async fn relinks_renames_and_removals_never_write_into_the_library() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995)/Heat (1995).mkv", "heat");
    let episode = h.write("Show/S01/Show S01E01.mkv", "one");
    h.write("Show/S01/Show S01E02.mkv", "two");
    let ronin = h.write("Ronin (1998)/Ronin (1998).mkv", "ronin");
    h.scan().await;
    let (heat_id, episode_id) = (h.present(&heat).await.id, h.present(&episode).await.id);

    let moved = h.mv(&heat, "Crime/Heat (1995).mkv");
    let before = snapshot(&h.root);
    h.reconcile(&heat, FsEventKind::Removed).await;
    h.reconcile(&moved, FsEventKind::Created).await;
    h.scan().await;
    assert_eq!(
        h.present(&moved).await.id,
        heat_id,
        "the moved file relinked"
    );
    assert_eq!(snapshot(&h.root), before, "relinking a moved file");

    let old_dir = h.root.join("Show/S01");
    let new_dir = h.root.join("Show/Season 1");
    std::fs::rename(&old_dir, &new_dir).unwrap();
    let before = snapshot(&h.root);
    h.reconcile(&old_dir, FsEventKind::Modified).await;
    h.reconcile(&new_dir, FsEventKind::Modified).await;
    h.scan().await;
    assert_eq!(
        h.present(&new_dir.join("Show S01E01.mkv")).await.id,
        episode_id,
        "the renamed directory's files relinked"
    );
    assert_eq!(snapshot(&h.root), before, "reconciling a renamed directory");

    std::fs::remove_dir_all(h.root.join("Ronin (1998)")).unwrap();
    let before = snapshot(&h.root);
    h.reconcile(&h.root.join("Ronin (1998)"), FsEventKind::Removed)
        .await;
    h.scan().await;
    assert!(
        h.row(&ronin).await.expect("kept").missing_since.is_some(),
        "the removed directory's file is missing"
    );
    assert_eq!(snapshot(&h.root), before, "reconciling a removed directory");
}

/// A directory moved in from outside the library is walked and its files
/// indexed.
#[tokio::test]
async fn a_directory_moved_into_the_library_is_indexed() {
    let h = Harness::new().await;
    h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let outside = h.dir.path().join("outside/Heat (1995)");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("Heat (1995).mkv"), "heat").unwrap();

    let moved_in = h.root.join("Heat (1995)");
    std::fs::rename(&outside, &moved_in).unwrap();
    assert_eq!(
        h.reconcile(&moved_in, FsEventKind::Created).await,
        ReconcileOutcome::Done
    );

    let row = h.present(&moved_in.join("Heat (1995).mkv")).await;
    assert!(row.content.is_some(), "probed and classified as new");
}

/// A directory renamed into one the path policy excludes takes its files
/// out of the library: they are marked missing, not indexed at their new
/// paths.
#[tokio::test]
async fn a_directory_renamed_into_an_excluded_one_marks_its_files_missing() {
    let h = Harness::new().await;
    let film = h.write("Heat (1995)/Heat (1995).mkv", "heat");
    h.write("Heat (1995)/Clips/Opening.mkv", "opening");
    h.scan().await;
    let scene = h.root.join("Heat (1995)/Clips/Opening.mkv");
    assert!(h.row(&scene).await.is_some());

    let extras = h.root.join("Heat (1995)/Extras");
    std::fs::rename(h.root.join("Heat (1995)/Clips"), &extras).unwrap();
    h.reconcile(&extras, FsEventKind::Created).await;
    h.reconcile(&h.root.join("Heat (1995)/Clips"), FsEventKind::Removed)
        .await;

    assert!(h.row(&extras.join("Opening.mkv")).await.is_none());
    assert!(h.row(&scene).await.unwrap().missing_since.is_some());
    h.present(&film).await;
}

/// A file whose size or modification time moved but whose content did not,
/// and whose probe keeps failing, is hashed once: its new size and
/// modification time are recorded, so the next visit does not hash it again
/// to find the same content (PR #224 r2).
#[tokio::test]
async fn an_unprobed_file_touched_is_hashed_once_not_on_every_visit() {
    let h = Harness::new().await;
    h.prober.fails.store(true, Ordering::SeqCst);
    let path = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    assert_eq!(h.hashes(), 1);
    assert_eq!(h.present(&path).await.duration, None, "the probe failed");

    let touched = std::time::SystemTime::now() + Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(touched)
        .unwrap();
    h.scan().await;
    assert_eq!(h.hashes(), 2, "a moved mtime is hashed once");
    let (_, mtime) = read_fs_meta(path.parent().unwrap(), &path).unwrap();
    assert_eq!(h.present(&path).await.mtime, mtime, "and recorded");

    h.scan().await;
    assert_eq!(h.hashes(), 2, "and not again");
    assert_eq!(h.probes(), 3, "while the probe is still retried");
}

/// The same for a file whose identity alone moved: a `chmod` leaves its
/// size and mtime as recorded but moves its ctime. The file is hashed once
/// and its new identity recorded, so the next visit -- which probes it
/// again -- does not hash it again (issue #228).
#[cfg(unix)]
#[tokio::test]
async fn an_unprobed_file_chmodded_is_hashed_once_not_on_every_visit() {
    let h = Harness::new().await;
    h.prober.fails.store(true, Ordering::SeqCst);
    let path = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&path).await;
    assert_eq!(before.duration, None, "the probe failed");
    let hashes = h.hashes();

    chmod_until_ctime_moves(&path, before.identity.expect("recorded").ctime);
    h.scan().await;
    assert_eq!(h.hashes(), hashes + 1, "a moved ctime is hashed once");
    let after = h.present(&path).await;
    assert_eq!((after.id, after.hash), (before.id, before.hash));
    assert_eq!(
        after.identity,
        read_stat(path.parent().unwrap(), &path, Inodes::Stable)
            .unwrap()
            .identity,
        "and the identity found is recorded"
    );

    h.scan().await;
    assert_eq!(h.hashes(), hashes + 1, "and not again");
    assert_eq!(h.probes(), 3, "while the probe is still retried");
}

/// An unprobed row recorded before identities were is given its file's
/// identity by the next visit, which probes it again and fails, without a
/// hash -- so a same-size, same-mtime swap involving it can be seen from
/// then on (issue #228).
#[cfg(unix)]
#[tokio::test]
async fn an_unprobed_row_without_an_identity_is_given_one_without_a_hash() {
    let h = Harness::new().await;
    h.prober.fails.store(true, Ordering::SeqCst);
    let path = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    for row in h.file_repo.files.lock().unwrap().values_mut() {
        row.identity = None;
    }
    let before = h.present(&path).await;
    assert_eq!(before.duration, None, "the probe failed");
    let hashes = h.hashes();

    h.scan().await;

    assert_eq!(h.hashes(), hashes, "nothing is hashed");
    let after = h.present(&path).await;
    assert_eq!((after.id, after.hash), (before.id, before.hash));
    let found = read_stat(path.parent().unwrap(), &path, Inodes::Stable)
        .unwrap()
        .identity;
    assert!(found.is_some(), "a Unix file has an identity");
    assert_eq!(after.identity, found, "and the identity found is recorded");
}

/// An event for the library root itself is left to the scan: a root that
/// reads as empty may be a volume that is not mounted, and only the scan's
/// empty-root guard can tell. Nothing beneath it is marked missing.
#[tokio::test]
async fn a_directory_event_for_the_library_root_marks_nothing_missing() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995).mkv", "heat");
    let ronin = h.write("Films/Ronin (1998).mkv", "ronin");
    h.scan().await;

    // What an unmounted volume leaves behind: its empty mount point.
    std::fs::remove_file(&heat).unwrap();
    std::fs::remove_dir_all(h.root.join("Films")).unwrap();
    assert_eq!(
        h.reconcile(&h.root, FsEventKind::Modified).await,
        ReconcileOutcome::Done
    );

    h.present(&heat).await;
    h.present(&ronin).await;
}

/// A directory event covering a file still being written is deferred as a
/// whole, so the watcher comes back to the directory once the file has
/// settled; the files that had settled are reconciled meanwhile.
#[tokio::test]
async fn a_directory_event_with_a_file_still_being_written_is_deferred() {
    // A whole second: an mtime is read at a row's microsecond precision, so
    // a clock with nanoseconds would read the file as a fraction older.
    let now = instant(0);
    let window = Duration::from_secs(60);
    let h = Harness::with_service(|service| {
        service
            .with_clock(Arc::new(TestClock::starting_at(now)))
            .with_settle_window(window)
    })
    .await;
    let settled = h.write("Show/S01/Show S01E01.mkv", "one");
    let copying = h.write("Show/S01/Show S01E02.mkv", "two");
    let set_mtime = |path: &Path, at: DateTime<Utc>| {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::from(at))
            .unwrap();
    };
    set_mtime(&settled, now - chrono::TimeDelta::hours(1));
    set_mtime(&copying, now);

    let outcome = h
        .reconcile(&h.root.join("Show/S01"), FsEventKind::Created)
        .await;

    assert_eq!(
        outcome,
        ReconcileOutcome::Deferred {
            retry_after: window
        }
    );
    h.present(&settled).await;
    assert!(h.row(&copying).await.is_none(), "not indexed while written");
}

/// A watcher event's kind is a hint only. A file removed and written again
/// within one debounce window arrives as a `Removed` event for a path that
/// is there, and its row stays present.
#[tokio::test]
async fn a_removed_event_for_a_path_that_is_back_leaves_its_row_present() {
    let h = Harness::new().await;
    let path = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&path).await;

    std::fs::remove_file(&path).unwrap();
    h.write("Heat (1995).mkv", "heat");
    assert_eq!(
        h.reconcile(&path, FsEventKind::Removed).await,
        ReconcileOutcome::Done
    );

    assert_eq!(h.present(&path).await.id, before.id);
}

/// A removed directory is not believed while the library root holds no
/// video file at all -- what an unmounted volume looks like, as the scan's
/// empty-root guard reads it. Its rows are left for the next scan.
#[tokio::test]
async fn a_removed_directory_under_a_root_with_no_video_is_left_to_the_scan() {
    let h = Harness::new().await;
    let episode = h.write("Show/S1/Show S01E01.mkv", "one");
    let film = h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;

    std::fs::remove_dir_all(h.root.join("Show")).unwrap();
    std::fs::remove_file(&film).unwrap();
    std::fs::write(h.root.join(".not_mounted"), "").unwrap();
    assert_eq!(
        h.reconcile(&h.root.join("Show/S1"), FsEventKind::Removed)
            .await,
        ReconcileOutcome::Done
    );

    h.present(&episode).await;
}

/// A removed path the policy never indexes anything beneath -- a hidden
/// partial download, a sidecar, an extras folder -- has no row of its own,
/// and the rows of the library are not read to look beneath it.
#[tokio::test]
async fn a_removed_path_the_policy_never_indexes_is_not_looked_beneath() {
    use crate::services::hash::MockHashService;
    use crate::services::media_info::MockMediaInfoService;
    use beam_domain::repositories::file::MockFileRepository;

    let dir = TempDir::new().unwrap();
    let library_repo = Arc::new(InMemoryLibraryRepository::default());
    let library = library_repo
        .create(CreateLibrary {
            name: "Films".to_string(),
            root_path: dir.path().to_path_buf(),
            description: None,
        })
        .await
        .unwrap();
    let mut files = MockFileRepository::new();
    files.expect_find_by_path().returning(|_| Ok(None));
    files.expect_find_beneath_including_missing().never();
    let service = LocalIndexService::new(
        library_repo,
        Arc::new(files),
        Arc::new(InMemoryMovieRepository::default()),
        Arc::new(InMemoryShowRepository::default()),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(
            InMemoryAdminLogRepository::default(),
        ),
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    );

    for removed in [
        ".Heat (1995).mkv.part",
        "Heat (1995).srt",
        "Heat (1995)/Extras",
    ] {
        assert_eq!(
            service
                .reconcile_path(library.id, dir.path().join(removed), FsEventKind::Removed)
                .await
                .unwrap(),
            ReconcileOutcome::Done,
            "{removed}"
        );
    }
}

// ─── same size, same mtime (issue #228) ──────────────────────────────────────

/// Set `path`'s modification time to `at`, as `cp -p` or `rsync -a` would
/// keep it from a source.
fn set_mtime(path: &Path, at: DateTime<Utc>) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::from(at))
        .unwrap();
}

/// Change `path`'s ctime and nothing else, as a `chmod` does, from
/// `recorded`. The kernel stamps a ctime at its clock tick, so a chmod in the
/// tick `recorded` was stamped in leaves it as it was: the mode is toggled
/// until the ctime moves, which it does within one tick.
#[cfg(unix)]
fn chmod_until_ctime_moves(path: &Path, recorded: DateTime<Utc>) {
    use std::os::unix::fs::PermissionsExt;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    for mode in [0o600, 0o644].into_iter().cycle() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        let identity = read_stat(path.parent().unwrap(), path, Inodes::Stable)
            .unwrap()
            .identity;
        if identity.expect("a Unix file has an identity").ctime != recorded {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the ctime of {} never moved",
            path.display()
        );
    }
}

/// Swaps, rotations and paths taken over (issue #180), and files that
/// keep their size and mtime across a change (issue #228).
#[path = "index_relink_swap_tests.rs"]
mod swaps;
