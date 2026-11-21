use super::*;

// ─── swaps, rotations, and paths taken over (issue #180) ─────────────────────

/// Two files that swap names -- through a temporary name, as `mv` does it --
/// each keep their own row: its playback progress stays on its content,
/// not on its old name.
#[tokio::test]
async fn two_files_that_swap_names_keep_their_own_rows() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995).mkv", "heat, a film about a heist");
    let ronin = h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let (heat_id, ronin_id) = (h.present(&heat).await.id, h.present(&ronin).await.id);
    let probes = h.probes();

    let tmp = h.mv(&heat, "tmp.mkv");
    h.mv(&ronin, "Heat (1995).mkv");
    h.mv(&tmp, "Ronin (1998).mkv");
    let progress = h.scan().await;

    assert_eq!(
        (progress.relinked, progress.changed, progress.added),
        (2, 0, 0)
    );
    assert_eq!(
        h.present(&ronin).await.id,
        heat_id,
        "Heat's row is at its bytes"
    );
    assert_eq!(h.present(&heat).await.id, ronin_id);
    assert_eq!(h.probes(), probes, "nothing was probed");
    assert_eq!(h.row_count().await, 2);
}

/// Three files rotated among three names each keep their own row.
#[tokio::test]
async fn three_files_rotated_among_their_names_keep_their_own_rows() {
    let h = Harness::new().await;
    let a = h.write("A.mkv", "first");
    let b = h.write("B.mkv", "second, longer");
    let c = h.write("C.mkv", "third, longest of all");
    h.scan().await;
    let ids = [
        h.present(&a).await.id,
        h.present(&b).await.id,
        h.present(&c).await.id,
    ];

    let tmp = h.mv(&a, "tmp.mkv");
    h.mv(&c, "A.mkv");
    h.mv(&b, "C.mkv");
    h.mv(&tmp, "B.mkv");
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.changed), (3, 0));
    assert_eq!(h.present(&b).await.id, ids[0]);
    assert_eq!(h.present(&c).await.id, ids[1]);
    assert_eq!(h.present(&a).await.id, ids[2]);
    assert_eq!(h.row_count().await, 3);
}

/// Write each of `names` under the root with its own content, all of one
/// size and one modification time, and scan them in.
async fn same_size_same_mtime(h: &Harness, names: &[&str]) -> Vec<(PathBuf, Uuid)> {
    let mut files = Vec::new();
    for (n, name) in names.iter().enumerate() {
        let path = h.write(name, &format!("content number {n}"));
        set_mtime(&path, instant(-60));
        files.push(path);
    }
    h.scan().await;
    let mut rows = Vec::new();
    for path in files {
        let id = h.present(&path).await.id;
        rows.push((path, id));
    }
    rows
}

/// Every path a size and an mtime alone would call unchanged: each still
/// has the size and the mtime its own row recorded.
async fn assert_size_and_mtime_match_every_row(h: &Harness, paths: &[&PathBuf]) {
    for path in paths {
        let row = h.present(path).await;
        assert_eq!(
            read_fs_meta(path.parent().unwrap(), path).unwrap(),
            (row.size_bytes, row.mtime),
            "{} looks unchanged by size and mtime",
            path.display()
        );
    }
}

/// Two files of one size and one mtime that swap names each keep their own
/// row, though each path still has its row's size and mtime: the swap moved
/// each path's inode. A file the swap did not touch is not hashed again.
#[cfg(unix)]
#[tokio::test]
async fn a_same_size_same_mtime_swap_keeps_each_row_on_its_content() {
    let h = Harness::new().await;
    let files = same_size_same_mtime(&h, &["Heat (1995).mkv", "Ronin (1998).mkv"]).await;
    let (heat, heat_id) = files[0].clone();
    let (ronin, ronin_id) = files[1].clone();
    let untouched = h.write("Alien (1979).mkv", "an untouched film");
    h.scan().await;
    let (hashes, probes) = (h.hashes(), h.probes());

    let tmp = h.mv(&heat, "tmp.mkv");
    h.mv(&ronin, "Heat (1995).mkv");
    h.mv(&tmp, "Ronin (1998).mkv");
    assert_size_and_mtime_match_every_row(&h, &[&heat, &ronin]).await;
    let progress = h.scan().await;

    assert_eq!(
        (progress.relinked, progress.changed, progress.added),
        (2, 0, 0)
    );
    assert_eq!(
        h.present(&ronin).await.id,
        heat_id,
        "Heat's row is at its bytes"
    );
    assert_eq!(h.present(&heat).await.id, ronin_id);
    assert_eq!(h.hashes(), hashes + 2, "only the swapped paths are hashed");
    assert_eq!(h.probes(), probes, "nothing was probed");
    h.present(&untouched).await;

    h.scan().await;
    assert_eq!(
        h.hashes(),
        hashes + 2,
        "the relinked rows record what they were found at"
    );
}

/// Three files of one size and one mtime rotated among their names each
/// keep their own row.
#[cfg(unix)]
#[tokio::test]
async fn a_same_size_same_mtime_rotation_keeps_each_row_on_its_content() {
    let h = Harness::new().await;
    let files = same_size_same_mtime(&h, &["A.mkv", "B.mkv", "C.mkv"]).await;
    let [(a, a_id), (b, b_id), (c, c_id)] = [files[0].clone(), files[1].clone(), files[2].clone()];

    let tmp = h.mv(&a, "tmp.mkv");
    h.mv(&c, "A.mkv");
    h.mv(&b, "C.mkv");
    h.mv(&tmp, "B.mkv");
    assert_size_and_mtime_match_every_row(&h, &[&a, &b, &c]).await;
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.changed), (3, 0));
    assert_eq!(h.present(&b).await.id, a_id);
    assert_eq!(h.present(&c).await.id, b_id);
    assert_eq!(h.present(&a).await.id, c_id);
    assert_eq!(h.row_count().await, 3);
}

/// A row recorded before identities were -- none of its own -- is given
/// its file's by the next scan that finds the file at its size and mtime,
/// without hashing it; from then on a same-size, same-mtime swap is seen.
#[cfg(unix)]
#[tokio::test]
async fn a_row_without_an_identity_is_given_one_without_a_hash() {
    let h = Harness::new().await;
    let files = same_size_same_mtime(&h, &["Heat (1995).mkv", "Ronin (1998).mkv"]).await;
    let (heat, heat_id) = files[0].clone();
    let (ronin, ronin_id) = files[1].clone();
    for row in h.file_repo.files.lock().unwrap().values_mut() {
        row.identity = None;
    }
    let hashes = h.hashes();

    h.scan().await;

    assert_eq!(h.hashes(), hashes, "nothing is hashed");
    for path in [&heat, &ronin] {
        let found = read_stat(path.parent().unwrap(), path, Inodes::Stable)
            .unwrap()
            .identity;
        assert!(found.is_some(), "a Unix file has an identity");
        assert_eq!(h.present(path).await.identity, found, "{}", path.display());
    }

    let tmp = h.mv(&heat, "tmp.mkv");
    h.mv(&ronin, "Heat (1995).mkv");
    h.mv(&tmp, "Ronin (1998).mkv");
    let progress = h.scan().await;

    assert_eq!(progress.relinked, 2);
    assert_eq!(h.present(&ronin).await.id, heat_id);
    assert_eq!(h.present(&heat).await.id, ronin_id);
}

/// The watcher's side of a same-size, same-mtime swap (issue #228). Each
/// path's new content is the other row's, and by size and mtime alone that
/// row's path still holds it; its identity says it does not. So both events
/// are left to the scan, not read as content changes, and the scan gives
/// each row back its content.
#[cfg(unix)]
#[tokio::test]
async fn a_same_size_same_mtime_swap_seen_by_the_watcher_is_left_to_the_scan() {
    let h = Harness::new().await;
    let files = same_size_same_mtime(&h, &["Heat (1995).mkv", "Ronin (1998).mkv"]).await;
    let (heat, heat_id) = files[0].clone();
    let (ronin, ronin_id) = files[1].clone();
    let before = [h.present(&heat).await, h.present(&ronin).await];

    let tmp = h.mv(&heat, "tmp.mkv");
    h.mv(&ronin, "Heat (1995).mkv");
    h.mv(&tmp, "Ronin (1998).mkv");
    assert_size_and_mtime_match_every_row(&h, &[&heat, &ronin]).await;
    for (path, kind) in [
        (&heat, FsEventKind::Modified),
        (&ronin, FsEventKind::Modified),
        (&tmp, FsEventKind::Removed),
    ] {
        assert_eq!(h.reconcile(path, kind).await, ReconcileOutcome::Done);
    }

    for row in &before {
        let now = h.present(&row.path).await;
        assert_eq!(
            (now.id, now.hash, now.identity),
            (row.id, row.hash, row.identity),
            "{} is left exactly as it was",
            row.path.display()
        );
    }
    let progress = h.scan().await;
    assert_eq!(
        (progress.relinked, progress.changed, progress.added),
        (2, 0, 0)
    );
    assert_eq!(h.present(&ronin).await.id, heat_id);
    assert_eq!(h.present(&heat).await.id, ronin_id);
}

/// A file replaced by a rename over its path, keeping its length and its
/// mtime, is a content change the scan records: its new inode is enough to
/// hash it, though its size and mtime are its row's. No row holds the new
/// content, so the row stays, with its id and the new hash.
#[cfg(unix)]
#[tokio::test]
async fn a_file_renamed_over_at_its_size_and_mtime_is_a_content_change() {
    let h = Harness::new().await;
    let path = h.write("Heat (1995).mkv", "heat, cut one");
    h.scan().await;
    let before = h.present(&path).await;

    // Written outside the library, so no scan sees it under another name.
    let replacement = h.dir.path().join("outside").join("replacement.mkv");
    std::fs::write(&replacement, "heat, cut two").unwrap();
    set_mtime(&replacement, before.mtime.expect("a file has an mtime"));
    std::fs::rename(&replacement, &path).unwrap();
    assert_size_and_mtime_match_every_row(&h, &[&path]).await;
    let progress = h.scan().await;

    assert_eq!(
        (progress.relinked, progress.changed, progress.added),
        (0, 1, 0)
    );
    let after = h.present(&path).await;
    assert_eq!(after.id, before.id, "the row keeps its id");
    assert_ne!(after.hash, before.hash, "and records the new content");
    assert_eq!(h.row_count().await, 1);
}

/// A `chmod` moves a file's ctime but not its content: the next scan hashes
/// it once, finds the content unchanged and records the new identity, and
/// the scan after hashes nothing. The same holds on a filesystem whose
/// inodes are not compared, since its ctime still is.
#[cfg(unix)]
async fn a_chmod_costs_one_hash(kind: FilesystemKind) {
    let h = Harness::with_service(|service| {
        service.with_filesystem_probe(Arc::new(FixedFilesystemProbe(kind)))
    })
    .await;
    let path = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let before = h.present(&path).await;
    let hashes = h.hashes();

    chmod_until_ctime_moves(&path, before.identity.expect("recorded").ctime);
    let progress = h.scan().await;

    assert_eq!(h.hashes(), hashes + 1, "a moved ctime is hashed once");
    assert_eq!((progress.relinked, progress.changed), (0, 0));
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
    assert_eq!(h.hashes(), hashes + 1, "so the next scan hashes nothing");
}

#[cfg(unix)]
#[tokio::test]
async fn a_chmod_on_a_local_library_costs_one_hash() {
    a_chmod_costs_one_hash(FilesystemKind::Local).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_chmod_on_a_network_library_costs_one_hash() {
    a_chmod_costs_one_hash(FilesystemKind::Network).await;
}

/// Give every row an inode number its file does not have, keeping its
/// ctime -- as a network or FUSE filesystem can renumber a file after a
/// remount or a cache eviction, while the file itself is untouched.
#[cfg(unix)]
fn renumber_recorded_inodes(h: &Harness) {
    for row in h.file_repo.files.lock().unwrap().values_mut() {
        let identity = row.identity.as_mut().expect("a Unix file has an identity");
        identity.inode = !identity.inode;
    }
}

/// A library on a network or FUSE filesystem is compared by size, mtime and
/// ctime, never by inode: its inode numbers can change between scans (an
/// rclone mount after a remount, SMB with `noserverino`), and comparing them
/// would hash the whole library on every periodic scan.
#[cfg(unix)]
#[tokio::test]
async fn a_network_library_whose_inodes_are_renumbered_is_not_hashed_again() {
    let h = Harness::with_service(|service| {
        service.with_filesystem_probe(Arc::new(FixedFilesystemProbe(FilesystemKind::Network)))
    })
    .await;
    let heat = h.write("Heat (1995).mkv", "heat");
    let ronin = h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let before = [h.present(&heat).await, h.present(&ronin).await];
    let hashes = h.hashes();

    renumber_recorded_inodes(&h);
    let progress = h.scan().await;

    assert_eq!(h.hashes(), hashes, "nothing is hashed");
    assert_eq!(
        (progress.relinked, progress.changed, progress.added),
        (0, 0, 0)
    );
    for row in &before {
        let now = h.present(&row.path).await;
        assert_eq!((now.id, now.hash), (row.id, row.hash));
    }
}

/// On a local filesystem an inode number is stable, so a different one is a
/// different file -- which is how a same-tick swap is seen. Each file whose
/// recorded inode is not its own is hashed once, found unchanged, and given
/// its identity; the next scan hashes nothing.
#[cfg(unix)]
#[tokio::test]
async fn a_local_library_whose_inodes_differ_hashes_each_file_once() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995).mkv", "heat");
    let ronin = h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let before = [h.present(&heat).await, h.present(&ronin).await];
    let hashes = h.hashes();

    renumber_recorded_inodes(&h);
    let progress = h.scan().await;

    assert_eq!(h.hashes(), hashes + 2, "each file is hashed");
    assert_eq!(
        (progress.relinked, progress.changed, progress.added),
        (0, 0, 0)
    );
    for row in &before {
        let now = h.present(&row.path).await;
        assert_eq!(
            (now.id, now.hash, now.identity),
            (row.id, row.hash, row.identity)
        );
    }
    h.scan().await;
    assert_eq!(h.hashes(), hashes + 2, "once");
}

/// A library whose filesystem cannot be classified is taken as local, as
/// the watcher takes it: its inode numbers are compared. At worst that
/// hashes a file that did not change; not comparing them could miss a swap.
#[cfg(unix)]
#[tokio::test]
async fn a_library_whose_filesystem_cannot_be_classified_compares_inodes() {
    use crate::services::filesystem_probe::FailingFilesystemProbe;
    let h = Harness::with_service(|service| {
        service.with_filesystem_probe(Arc::new(FailingFilesystemProbe))
    })
    .await;
    h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let hashes = h.hashes();

    renumber_recorded_inodes(&h);
    h.scan().await;

    assert_eq!(h.hashes(), hashes + 1, "the renumbered file is hashed");
}

/// A file renamed onto the path of a file deleted earlier, whose row is
/// still missing, keeps its own row and its progress; the stale row is not
/// restored with the renamed file's content, but kept aside as missing.
#[tokio::test]
async fn a_file_renamed_onto_a_missing_rows_path_keeps_its_own_row() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995).mkv", "heat");
    let stale = h.write("Old (1990).mkv", "an old film");
    h.scan().await;
    h.watch(&heat).await;
    let heat_id = h.present(&heat).await.id;
    let stale_id = h.present(&stale).await.id;
    std::fs::remove_file(&stale).unwrap();
    h.scan().await;
    assert!(h.row(&stale).await.unwrap().missing_since.is_some());

    h.mv(&heat, "Old (1990).mkv");
    let progress = h.scan().await;

    assert_eq!(
        (progress.relinked, progress.restored, progress.changed),
        (1, 0, 0)
    );
    assert_eq!(h.present(&stale).await.id, heat_id);
    assert!(
        h.row(&heat).await.is_none(),
        "nothing left at Heat's old name"
    );
    let kept = h
        .file_repo
        .find_all_by_library_including_missing(h.library.id)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == stale_id)
        .expect("the stale row is kept, not deleted");
    assert!(kept.missing_since.is_some());
    assert_eq!(
        h.progress
            .last_played_at(vec![heat_id])
            .await
            .unwrap()
            .len(),
        1,
        "Heat's progress is still Heat's"
    );
}

/// A row kept aside when another file took its path is still its file's:
/// brought back under a new name, the file is relinked to it. The move is
/// reported from the path the file had -- never from the name the row was
/// kept at, which no file ever had.
#[tokio::test]
async fn a_displaced_row_found_again_is_reported_from_its_own_path() {
    let h = Harness::new().await;
    let old = h.write("Old (1990).mkv", "an old film");
    let heat = h.write("Heat (1995).mkv", "heat");
    h.scan().await;
    let old_id = h.present(&old).await.id;
    let away = h.dir.path().join("outside").join("Old (1990).mkv");
    std::fs::rename(&old, &away).unwrap();
    h.scan().await;
    h.mv(&heat, "Old (1990).mkv");
    h.scan().await;
    assert_ne!(h.present(&old).await.id, old_id, "Heat took Old's path");

    let back = h.root.join("Old back (1990).mkv");
    std::fs::rename(&away, &back).unwrap();
    let progress = h.scan().await;

    assert_eq!((progress.relinked, progress.added), (1, 0));
    assert_eq!(h.present(&back).await.id, old_id);
    let found_again = h
        .move_logs()
        .await
        .into_iter()
        .find(|log| log.details.as_ref().unwrap()["file_id"] == old_id.to_string())
        .expect("the move is logged");
    assert_eq!(
        found_again.details.as_ref().unwrap()["from"],
        serde_json::json!(old.display().to_string())
    );
    assert_eq!(
        found_again.message,
        format!(
            "File moved: '{}' is now '{}'",
            old.display(),
            back.display()
        )
    );
    let logged = h.admin_log_repo.list(100, 0).await.unwrap();
    let published = h.notifications.published_events();
    let told = logged
        .iter()
        .map(|log| format!("{} {:?}", log.message, log.details))
        .chain(published.iter().map(|event| event.message.clone()));
    for text in told {
        assert!(!text.contains(".beam-displaced-"), "{text}");
    }
}

/// Replacing a watched film by renaming another file onto its name -- a
/// better copy, say -- keeps the watched row and everyone's place in it:
/// its content changed. The renamed file's row, never played, is marked
/// missing rather than taking the path.
#[tokio::test]
async fn a_file_renamed_onto_a_watched_files_path_replaces_its_content() {
    let h = Harness::new().await;
    let m = h.write("Heat (1995).mkv", "heat");
    let t = h.write("Heat (1995) 1080p.mkv", "heat, a better copy");
    h.scan().await;
    h.watch(&m).await;
    let m_id = h.present(&m).await.id;
    let t_id = h.present(&t).await.id;

    h.mv(&t, "Heat (1995).mkv");
    let progress = h.scan().await;

    assert_eq!(
        (progress.relinked, progress.changed, progress.marked_missing),
        (0, 1, 1)
    );
    let kept = h.present(&m).await;
    assert_eq!(kept.id, m_id, "the path keeps its own row");
    assert_eq!(
        kept.hash,
        h.row(&t).await.unwrap().hash,
        "with the new content"
    );
    assert!(
        h.row(&t).await.unwrap().missing_since.is_some(),
        "the renamed file's row is released as gone"
    );
    assert_eq!(h.row(&t).await.unwrap().id, t_id);
    assert_eq!(
        h.progress.last_played_at(vec![m_id]).await.unwrap().len(),
        1,
        "the progress stays on the path's row"
    );
}

/// One of two identical copies moved, the other deleted: the row someone
/// was watching keeps the file, and the other is marked missing.
#[tokio::test]
async fn of_two_identical_copies_the_watched_row_keeps_the_moved_file() {
    for watch_first in [true, false] {
        let h = Harness::new().await;
        let first = h.write("First/Film.mkv", "the same film");
        let second = h.write("Second/Film.mkv", "the same film");
        h.scan().await;
        let (watched, other) = if watch_first {
            (&first, &second)
        } else {
            (&second, &first)
        };
        h.watch(watched).await;
        let watched_id = h.present(watched).await.id;
        let other_id = h.present(other).await.id;

        h.mv(&first, "Moved/Film.mkv");
        std::fs::remove_file(&second).unwrap();
        let progress = h.scan().await;

        assert_eq!((progress.relinked, progress.marked_missing), (1, 1));
        assert_eq!(
            h.present(&h.root.join("Moved/Film.mkv")).await.id,
            watched_id,
            "watched first: {watch_first}"
        );
        let missing = h
            .file_repo
            .find_all_by_library_including_missing(h.library.id)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.id == other_id)
            .unwrap();
        assert!(missing.missing_since.is_some());
    }
}

/// A watcher event cannot tell a swap from a change: the two halves arrive
/// apart. A file whose new content is another file's that may have moved is
/// left as it is -- not restored, not marked changed -- and the next scan,
/// which sees both paths, gives each row back its content.
#[tokio::test]
async fn a_swap_seen_by_the_watcher_is_left_to_the_scan() {
    let h = Harness::new().await;
    let heat = h.write("Heat (1995).mkv", "heat, a film about a heist");
    let ronin = h.write("Ronin (1998).mkv", "ronin");
    h.scan().await;
    let (heat_row, ronin_row) = (h.present(&heat).await, h.present(&ronin).await);

    let tmp = h.mv(&heat, "tmp.mkv");
    h.mv(&ronin, "Heat (1995).mkv");
    h.mv(&tmp, "Ronin (1998).mkv");
    for path in [&heat, &ronin, &tmp] {
        let kind = if path == &tmp {
            FsEventKind::Removed
        } else {
            FsEventKind::Modified
        };
        assert_eq!(h.reconcile(path, kind).await, ReconcileOutcome::Done);
    }

    let (at_heat, at_ronin) = (h.present(&heat).await, h.present(&ronin).await);
    let recorded = |row: &MediaFile| {
        (
            row.id,
            row.hash,
            row.size_bytes,
            row.mtime,
            row.status,
            row.duration,
            row.missing_since,
        )
    };
    assert_eq!(
        recorded(&at_heat),
        recorded(&heat_row),
        "left exactly as it was"
    );
    assert_eq!(recorded(&at_ronin), recorded(&ronin_row));
    assert!(
        h.row(&tmp).await.is_none(),
        "the passing name is not indexed"
    );

    let progress = h.scan().await;
    assert_eq!((progress.relinked, progress.changed), (2, 0));
    assert_eq!(h.present(&ronin).await.id, heat_row.id);
    assert_eq!(h.present(&heat).await.id, ronin_row.id);
}

/// A new name whose content is a file whose own path now holds other
/// content -- one half of a rotation -- is left to the scan by the watcher,
/// rather than indexed as a copy beside the row it belongs to.
#[tokio::test]
async fn a_new_name_holding_a_file_whose_path_changed_is_left_to_the_scan() {
    let h = Harness::new().await;
    let a = h.write("A.mkv", "first");
    let c = h.write("C.mkv", "third, longer");
    h.scan().await;
    let a_id = h.present(&a).await.id;

    let n = h.mv(&a, "N.mkv");
    h.mv(&c, "A.mkv");
    assert_eq!(
        h.reconcile(&n, FsEventKind::Created).await,
        ReconcileOutcome::Done
    );
    assert!(h.row(&n).await.is_none(), "not indexed as a copy");

    let progress = h.scan().await;
    assert_eq!((progress.relinked, progress.added), (2, 0));
    assert_eq!(h.present(&n).await.id, a_id);
}
