use super::*;

/// An NFO whose stat stamp is the one recorded was not written since, and is
/// not read again; one with no recorded stamp is.
#[cfg(unix)]
#[tokio::test]
async fn an_nfo_whose_stat_stamp_is_unchanged_is_not_read_again() {
    use beam_domain::models::{PinSource, ProviderPin};
    use beam_domain::repositories::MovieRepository;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    assert!(
        h.movie_repo
            .set_pinned_ref(movie.id, &ProviderPin::Tmdb(1), PinSource::Nfo)
            .await
            .unwrap()
    );
    // A record whose content no longer matches the NFO, but whose stamp
    // does: only a read would find the difference.
    let nfo = h.root.join("Matrix/movie.nfo");
    let stamp = hints::change_stamp(&crate::library_file::FileMeta::from(
        &std::fs::symlink_metadata(&nfo).unwrap(),
    ));
    let stale = |stamp: Option<String>| {
        let mut rows = h.applied_nfo_repo.rows.lock().unwrap();
        let row = rows.get_mut(&nfo).unwrap();
        row.content_hash = "stale".to_string();
        row.change_stamp = stamp;
    };

    stale(stamp);
    h.scan().await;
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:1"),
        "not read, so not re-applied"
    );

    stale(None);
    h.scan().await;
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:603"),
        "read, found changed, re-applied"
    );
}

/// An NFO written moments before it was read is recorded without its stat
/// stamp: a write within the same timestamp tick would not move the stamp,
/// so the next scan reads it again rather than trust it.
#[tokio::test]
async fn an_nfo_written_just_before_it_is_read_is_recorded_without_a_stamp() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);

    h.scan().await;

    let record = h
        .applied("Matrix/movie.nfo")
        .await
        .expect("classification recorded the NFO it read");
    assert_eq!(record.change_stamp, None);
    assert_eq!(record.size_bytes, MATRIX_NFO.len() as u64);
}

/// The record of an NFO that is deleted goes with it -- by the watcher, and
/// by the next scan -- so a record is kept only for an NFO on disk.
#[tokio::test]
async fn a_deleted_nfos_record_is_forgotten() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.video("Heat/Heat.mkv");
    h.write("Heat/Heat.nfo", &tmdb_movie(949));
    h.scan().await;
    assert!(h.applied("Matrix/movie.nfo").await.is_some());
    assert!(h.applied("Heat/Heat.nfo").await.is_some());

    std::fs::remove_file(h.root.join("Matrix/movie.nfo")).unwrap();
    h.event("Matrix/movie.nfo", FsEventKind::Removed).await;
    assert!(h.applied("Matrix/movie.nfo").await.is_none());

    std::fs::remove_file(h.root.join("Heat/Heat.nfo")).unwrap();
    h.scan().await;
    assert!(h.applied("Heat/Heat.nfo").await.is_none());
    assert_eq!(
        h.movie_of("Heat/Heat.mkv").pinned_ref.as_deref(),
        Some("tmdb:949"),
        "deleting an NFO leaves its pin"
    );
}

/// An NFO watcher event reads only the files beneath the NFO's folder -- one
/// query -- never the whole library's file list.
#[tokio::test]
async fn an_nfo_event_reads_only_the_files_beneath_its_folder() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.video("Heat/Heat.mkv");
    h.scan().await;
    h.file_reads
        .whole_library_reads
        .store(0, std::sync::atomic::Ordering::SeqCst);
    h.file_reads
        .folder_reads
        .store(0, std::sync::atomic::Ordering::SeqCst);

    h.write("Matrix/movie.nfo", &tmdb_movie(604));
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;

    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604")
    );
    assert_eq!(
        (
            h.file_reads
                .whole_library_reads
                .load(std::sync::atomic::Ordering::SeqCst),
            h.file_reads
                .folder_reads
                .load(std::sync::atomic::Ordering::SeqCst)
        ),
        (0, 1),
        "(whole-library reads, folder reads)"
    );
}

/// A scan well after an NFO's last write -- by the injected clock -- records
/// its stat stamp, and a later scan that finds the same stamp does not read
/// the NFO again; a write moves the stamp, and the next scan reads it.
#[cfg(unix)]
#[tokio::test]
async fn a_settled_nfos_stamp_is_recorded_and_spares_the_next_scan_a_read() {
    let clock = Arc::new(beam_domain::services::TestClock::starting_at(
        chrono::Utc::now() + chrono::Duration::hours(1),
    ));
    let h = Harness::build(Probe::Double, clock).await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);

    h.scan().await;

    let nfo = h.root.join("Matrix/movie.nfo");
    let stamp = hints::change_stamp(&crate::library_file::FileMeta::from(
        &std::fs::symlink_metadata(&nfo).unwrap(),
    ));
    assert!(stamp.is_some());
    assert_eq!(
        h.applied("Matrix/movie.nfo")
            .await
            .expect("recorded")
            .change_stamp,
        stamp,
        "written long before the clock's now, so its stamp is vouched for"
    );

    let before = nfo_reads();
    h.scan().await;
    assert_eq!(nfo_reads(), before, "the same stamp: not read again");

    h.write("Matrix/movie.nfo", &tmdb_movie(604));
    h.scan().await;
    assert!(nfo_reads() > before, "a write moved the stamp: read");
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604")
    );
}

/// Two NFOs of one size and one mtime swapped between their folders after
/// their stamps were recorded -- the blind spot of issue #228, for NFOs
/// (issue #246): each path's size and mtime still match its record, but a
/// rename moves the change time the stamp holds, so the next scan reads both
/// rather than skipping them as unwritten. (What it then makes of a swap is
/// FR-219's to say; this pins only that the swap is not missed.)
#[cfg(unix)]
#[tokio::test]
async fn a_same_size_same_mtime_swap_of_nfos_is_read_by_the_next_scan() {
    let h = harness_ahead_of_its_files().await;
    h.video("Matrix/matrix.mkv");
    h.video("Heat/heat.mkv");
    h.write("Matrix/movie.nfo", &tmdb_movie(603));
    h.write("Heat/movie.nfo", &tmdb_movie(604));
    let written = std::time::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    h.set_mtime("Matrix/movie.nfo", written);
    h.set_mtime("Heat/movie.nfo", written);
    h.scan().await;
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:603")
    );
    let recorded = h.applied("Matrix/movie.nfo").await.expect("recorded");
    assert!(recorded.change_stamp.is_some(), "the stamp is vouched for");

    let (matrix, heat) = (
        h.root.join("Matrix/movie.nfo"),
        h.root.join("Heat/movie.nfo"),
    );
    let between = h.root.join("between.nfo");
    std::fs::rename(&matrix, &between).unwrap();
    std::fs::rename(&heat, &matrix).unwrap();
    std::fs::rename(&between, &heat).unwrap();
    for path in [&matrix, &heat] {
        let meta = std::fs::metadata(path).unwrap();
        assert_eq!(meta.len(), recorded.size_bytes, "{}", path.display());
        assert_eq!(meta.modified().unwrap(), written, "{}", path.display());
    }
    let before = nfo_reads();
    h.scan().await;

    // An unswapped rescan reads none (see
    // `a_settled_nfos_stamp_is_recorded_and_spares_the_next_scan_a_read`).
    assert!(
        nfo_reads() - before >= 2,
        "each swapped NFO is read, its stamp not vouching for it"
    );
}

/// A walk that cannot read the folder a kept, conflicting NFO lives in says
/// nothing about that NFO: its record is kept, so once the folder is readable
/// again the NFO is not taken for a new one and does not replace the pin.
#[cfg(unix)]
#[tokio::test]
async fn a_walk_failure_where_a_kept_nfo_lives_keeps_its_record_and_the_pin() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::new().await;
    h.video("Alien (1979)/Alien (1979).mkv");
    h.video("Heat (1995)/Heat (1995).mkv");
    h.write("Heat (1995)/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;
    h.video("Heat (1995)/Heat (1995) - Remux.mkv");
    h.write("Heat (1995)/Heat (1995) - Remux.nfo", &tmdb_movie(1));
    h.scan().await;
    assert_eq!(
        h.movie_of("Heat (1995)/Heat (1995).mkv")
            .pinned_ref
            .as_deref(),
        Some("tmdb:949"),
        "the conflicting NFO was kept"
    );

    let locked = h.root.join("Heat (1995)");
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

    assert!(
        h.applied("Heat (1995)/Heat (1995) - Remux.nfo")
            .await
            .is_some(),
        "an NFO the walk could not see is not an NFO the walk found gone"
    );
    h.scan().await;
    assert_eq!(
        h.movie_of("Heat (1995)/Heat (1995).mkv")
            .pinned_ref
            .as_deref(),
        Some("tmdb:949"),
        "the kept NFO is not re-applied as new"
    );
}

/// A removal the watcher reports while the library root is gone -- a volume
/// unmounted -- is not an NFO or a subtitle deleted: their records stay.
#[tokio::test]
async fn a_removal_while_the_root_is_gone_forgets_no_nfo_or_subtitle() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.write("Matrix/matrix.en.srt", "1");
    h.scan().await;
    assert!(h.applied("Matrix/movie.nfo").await.is_some());
    assert_eq!(h.subtitles_of("Matrix/matrix.mkv").await.len(), 1);

    let parked = h.root.with_extension("unmounted");
    std::fs::rename(&h.root, &parked).unwrap();
    h.event("Matrix/movie.nfo", FsEventKind::Removed).await;
    h.event("Matrix/matrix.en.srt", FsEventKind::Removed).await;
    std::fs::rename(&parked, &h.root).unwrap();

    assert!(h.applied("Matrix/movie.nfo").await.is_some());
    assert_eq!(h.subtitles_of("Matrix/matrix.mkv").await.len(), 1);
}

/// Deleting a kept, conflicting NFO forgets its record; one created at that
/// path again is an NFO added after indexing, and re-pins the title (FR-219).
#[tokio::test]
async fn a_kept_nfo_deleted_and_recreated_repins_its_title() {
    let h = Harness::new().await;
    h.video("Heat (1995)/Heat (1995).mkv");
    h.write("Heat (1995)/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;
    h.video("Heat (1995)/Heat (1995) - Remux.mkv");
    h.write("Heat (1995)/Heat (1995) - Remux.nfo", &tmdb_movie(1));
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

    std::fs::remove_file(h.root.join("Heat (1995)/Heat (1995) - Remux.nfo")).unwrap();
    h.event("Heat (1995)/Heat (1995) - Remux.nfo", FsEventKind::Removed)
        .await;
    h.write("Heat (1995)/Heat (1995) - Remux.nfo", &tmdb_movie(1));
    h.event("Heat (1995)/Heat (1995) - Remux.nfo", FsEventKind::Created)
        .await;

    assert_eq!(h.movie_pins(), vec![Some("tmdb:1".to_string())]);
}
