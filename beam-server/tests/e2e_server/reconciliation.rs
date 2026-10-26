//! The library changes under a running server, and both the API and the
//! database follow it: files added and deleted (soft-deleted for a grace
//! period, #179), renamed, moved and swapped at one size and mtime while
//! keeping their ids and progress (#180, #228), an emptied root and a root
//! that disappears and returns (#160), and links never followed (#238).

use std::time::{Duration, SystemTime};

use beam_test_support::e2e::StackOptions;
use beam_test_support::e2e::client::api;
use uuid::Uuid;

use crate::common::{
    Admin, Video, assert_refused, continue_watching, file_row, file_rows, report, watch_rows,
};

/// The id of the present file at `path`, as the API lists it.
async fn id_at(admin: &Admin, library: Uuid, path: &str) -> Uuid {
    admin.file(library, path).await.id
}

/// The paths the API lists for `library`.
async fn listed(admin: &Admin, library: Uuid) -> Vec<String> {
    admin
        .files(library)
        .await
        .into_iter()
        .map(|file| file.path)
        .collect()
}

/// Scan `library` and return the counts that say what reconciliation did.
async fn rescan(admin: &Admin, library: Uuid) -> Counts {
    Counts::of(&admin.scan_ok(library).await)
}

/// The reconciliation counts of one scan.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counts {
    added: i64,
    changed: i64,
    unchanged: i64,
    marked_missing: i64,
    restored: i64,
    relinked: i64,
    purged: i64,
    failed: i64,
}

impl Counts {
    fn of(progress: &api::types::ScanProgress) -> Self {
        let api::types::ScanProgress {
            added_count,
            changed_count,
            unchanged_count,
            marked_missing_count,
            restored_count,
            relinked_count,
            purged_count,
            failed_count,
            total_count: _,
            processed_count: _,
            deferred_count: _,
        } = *progress;
        Self {
            added: added_count,
            changed: changed_count,
            unchanged: unchanged_count,
            marked_missing: marked_missing_count,
            restored: restored_count,
            relinked: relinked_count,
            purged: purged_count,
            failed: failed_count,
        }
    }
}

/// A deleted file is soft-deleted: hidden from the API, its row and its
/// viewer's progress kept, and the same row -- id, progress and all --
/// present again when the file comes back.
#[tokio::test]
async fn a_deleted_file_is_hidden_kept_and_restored_under_its_id() {
    let mut admin = Admin::start().await;
    let library_tree = &mut admin.stack.library;
    library_tree.add_video("films/Arrival (2016).mp4", &Video::new(440));
    library_tree.add_video("films/Heat (1995).mp4", &Video::new(550).seconds(10));
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;
    let heat = id_at(&admin, library, "Heat (1995).mp4").await;
    report(&admin.api, heat, 4.0, Some(10.0)).await;

    // Out of the library, not deleted: the same bytes and mtime come back.
    admin
        .stack
        .library
        .rename("films/Heat (1995).mp4", "parked/Heat (1995).mp4");
    assert_eq!(
        rescan(&admin, library).await,
        Counts {
            unchanged: 1,
            marked_missing: 1,
            ..Counts::default()
        }
    );
    assert_eq!(listed(&admin, library).await, ["Arrival (2016).mp4"]);
    assert!(
        admin
            .titles()
            .await
            .iter()
            .all(|title| title.name != "Heat"),
        "a title with no present file is not browsed"
    );
    let row = file_row(&admin.stack, heat)
        .await
        .expect("the row is kept for the grace period");
    assert!(row.missing_since.is_some(), "and stamped missing");
    assert_eq!(watch_rows(&admin.stack).await.len(), 1, "progress is kept");

    admin
        .stack
        .library
        .rename("parked/Heat (1995).mp4", "films/Heat (1995).mp4");
    assert_eq!(
        rescan(&admin, library).await,
        Counts {
            // A restored file is found as it was, so it is also unchanged.
            unchanged: 2,
            restored: 1,
            ..Counts::default()
        }
    );
    assert_eq!(
        id_at(&admin, library, "Heat (1995).mp4").await,
        heat,
        "restored under its old id"
    );
    let row = file_row(&admin.stack, heat).await.expect("the row");
    assert_eq!(row.missing_since, None);
    let shelf = continue_watching(&admin.api).await;
    assert_eq!(shelf.len(), 1);
    assert_eq!(
        (shelf[0].file_id, shelf[0].position_secs),
        (heat, 4.0),
        "the viewer resumes where they were"
    );

    admin.finish().await;
}

/// With no grace period, a file gone from disk is purged by the first scan
/// that sees it gone, and its title and the viewer's state with it; a new
/// file in its place is a new row.
#[tokio::test]
async fn with_no_grace_a_deleted_file_is_purged_and_a_new_one_is_new() {
    let mut admin =
        Admin::start_with(StackOptions::new().env("BEAM_MISSING_FILE_GRACE_DAYS", "0")).await;
    admin
        .stack
        .library
        .add_video("films/Arrival (2016).mp4", &Video::new(440));
    admin
        .stack
        .library
        .add_video("films/Heat (1995).mp4", &Video::new(550));
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;
    let heat = id_at(&admin, library, "Heat (1995).mp4").await;
    report(&admin.api, heat, 2.0, Some(6.0)).await;

    admin.stack.library.remove_file("films/Heat (1995).mp4");
    let counts = rescan(&admin, library).await;
    assert_eq!(
        (counts.marked_missing, counts.purged),
        (1, 1),
        "stamped and purged in one scan: {counts:?}"
    );
    assert!(
        file_row(&admin.stack, heat).await.is_none(),
        "the row is gone"
    );
    assert_eq!(file_rows(&admin.stack, library).await.len(), 1);
    assert!(
        admin
            .titles()
            .await
            .iter()
            .all(|title| title.name != "Heat"),
        "the title went with its last file"
    );
    assert!(
        watch_rows(&admin.stack).await.is_empty(),
        "and the viewer's state with the title"
    );

    admin
        .stack
        .library
        .add_video("films/Heat (1995).mp4", &Video::new(551));
    assert_eq!(rescan(&admin, library).await.added, 1);
    assert_ne!(id_at(&admin, library, "Heat (1995).mp4").await, heat);

    admin.finish().await;
}

/// A file renamed, a file moved to another folder, and two files of one
/// size and one modification time that swap names: each row follows its
/// content, keeping its id and the viewer's progress, and nothing is
/// indexed anew.
#[tokio::test]
async fn renamed_moved_and_swapped_files_keep_their_ids_and_progress() {
    let mut admin = Admin::start().await;
    let tree = &mut admin.stack.library;
    tree.add_video("films/Arrival (2016).mp4", &Video::new(440));
    tree.add_video("films/Heat (1995).mp4", &Video::new(550));
    // Same encode, a tag of the same length apart: the same size.
    let twin = |tag: &str| Video::new(660).tag("comment", tag);
    tree.add_video("films/Alien (1979).mp4", &twin("AAAA"));
    tree.add_video("films/Ronin (1998).mp4", &twin("BBBB"));
    let mtime = SystemTime::now() - Duration::from_secs(3600);
    tree.set_modified("films/Alien (1979).mp4", mtime);
    tree.set_modified("films/Ronin (1998).mp4", mtime);
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;

    let before = admin.files(library).await;
    let by_path = |path: &str| {
        before
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("no {path}"))
            .clone()
    };
    let (alien, ronin) = (by_path("Alien (1979).mp4"), by_path("Ronin (1998).mp4"));
    assert_eq!(alien.size_bytes, ronin.size_bytes, "the twins are one size");
    assert_ne!(alien.hash, ronin.hash, "and two contents");
    let arrival = by_path("Arrival (2016).mp4").id;
    let heat = by_path("Heat (1995).mp4").id;
    for (file, position) in [
        (arrival, 1.0),
        (heat, 2.0),
        (alien.id, 3.0),
        (ronin.id, 4.0),
    ] {
        report(&admin.api, file, position, Some(6.0)).await;
    }

    let tree = &mut admin.stack.library;
    tree.rename("films/Arrival (2016).mp4", "films/Arrival.2016.1080p.mp4");
    tree.rename("films/Heat (1995).mp4", "films/Crime/Heat (1995).mp4");
    tree.swap("films/Alien (1979).mp4", "films/Ronin (1998).mp4");
    assert_eq!(
        tree.modified("films/Alien (1979).mp4"),
        tree.modified("films/Ronin (1998).mp4"),
        "each swapped path still has its old size and mtime"
    );

    let counts = rescan(&admin, library).await;
    assert_eq!(
        (
            counts.relinked,
            counts.added,
            counts.changed,
            counts.marked_missing
        ),
        (4, 0, 0, 0),
        "four files found at new paths, none new, none gone: {counts:?}"
    );
    let after = admin.files(library).await;
    let id_now = |path: &str| {
        after
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("no {path} after the changes"))
    };
    assert_eq!(id_now("Arrival.2016.1080p.mp4").id, arrival, "renamed");
    assert_eq!(id_now("Crime/Heat (1995).mp4").id, heat, "moved");
    let (at_alien, at_ronin) = (id_now("Alien (1979).mp4"), id_now("Ronin (1998).mp4"));
    assert_eq!(
        (at_alien.id, at_alien.hash.as_str()),
        (ronin.id, ronin.hash.as_str()),
        "Ronin's row is at Ronin's bytes"
    );
    assert_eq!(
        (at_ronin.id, at_ronin.hash.as_str()),
        (alien.id, alien.hash.as_str())
    );
    assert_eq!(
        file_rows(&admin.stack, library).await.len(),
        4,
        "no row was added or left behind"
    );

    // Each viewer position is still on its file.
    let mut positions: Vec<(Uuid, f64)> = continue_watching(&admin.api)
        .await
        .into_iter()
        .map(|item| (item.file_id, item.position_secs))
        .collect();
    positions.sort_by(|a, b| a.1.total_cmp(&b.1));
    assert_eq!(
        positions,
        [
            (arrival, 1.0),
            (heat, 2.0),
            (alien.id, 3.0),
            (ronin.id, 4.0)
        ]
    );

    admin.finish().await;
}

/// A root gone altogether, or emptied of every file, is refused rather than
/// taken for a library whose every file was deleted: nothing is marked
/// missing, and when the files come back they are the same rows.
#[tokio::test]
async fn an_emptied_or_vanished_root_marks_nothing_missing() {
    let mut admin = Admin::start().await;
    admin
        .stack
        .library
        .add_video("films/Arrival (2016).mp4", &Video::new(440));
    admin
        .stack
        .library
        .add_video("films/Heat (1995).mp4", &Video::new(550));
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;
    let ids: Vec<Uuid> = admin.files(library).await.iter().map(|f| f.id).collect();

    let present_rows = |rows: Vec<beam_entity::files::Model>| {
        rows.iter()
            .filter(|row| row.missing_since.is_none())
            .count()
    };

    let last_job = admin
        .api
        .get_library_scan(library)
        .await
        .expect("read the last scan")
        .into_inner()
        .id;

    // The volume unmounted: the root directory itself is gone, and the scan
    // is refused before it starts -- no job, nothing touched.
    admin.stack.library.rename("films", "unmounted");
    assert_refused(
        "a scan of a root that is gone",
        400,
        admin.api.scan_library(library).await,
    );
    assert_eq!(
        admin
            .api
            .get_library_scan(library)
            .await
            .expect("read the last scan")
            .into_inner()
            .id,
        last_job,
        "no job was started"
    );
    assert_eq!(present_rows(file_rows(&admin.stack, library).await), 2);
    assert_eq!(listed(&admin, library).await.len(), 2);

    // Mounted again, but empty: a job that refuses to reconcile.
    admin.stack.library.create_dir("films");
    let failed = admin.scan(library).await;
    assert_eq!(
        failed.state,
        api::types::ScanState::Failed,
        "an empty root is refused, not reconciled"
    );
    let failure = failed.failure.expect("a failed scan says why");
    assert!(
        failure.contains("refusing to reconcile"),
        "the failure says what it refused: {failure}"
    );
    assert!(
        !failure.contains(&admin.stack.library.video_dir().display().to_string()),
        "and names no filesystem path: {failure}"
    );
    assert_eq!(Counts::of(&failed.progress), Counts::default());
    assert_eq!(present_rows(file_rows(&admin.stack, library).await), 2);

    // The files return.
    admin
        .stack
        .library
        .rename("unmounted/Arrival (2016).mp4", "films/Arrival (2016).mp4");
    admin
        .stack
        .library
        .rename("unmounted/Heat (1995).mp4", "films/Heat (1995).mp4");
    assert_eq!(
        rescan(&admin, library).await,
        Counts {
            unchanged: 2,
            ..Counts::default()
        }
    );
    let back: Vec<Uuid> = admin.files(library).await.iter().map(|f| f.id).collect();
    assert_eq!(back, ids, "the same rows");

    admin.finish().await;
}

/// A link inside the library -- to a file or to a folder, inside the library
/// or out of it -- is never followed: nothing it leads to is indexed or read.
#[tokio::test]
async fn a_symlinked_file_or_folder_is_never_read() {
    let mut admin = Admin::start().await;
    admin
        .stack
        .library
        .add_video("films/Arrival (2016).mp4", &Video::new(440));
    let outside = admin.stack.library.outside();
    let secret = outside.join("Secret (2001).mp4");
    Video::new(770).render(&secret);
    std::fs::create_dir(outside.join("Hidden")).expect("create a folder outside");
    Video::new(880).render(&outside.join("Hidden/Hidden (2002).mp4"));

    let tree = &mut admin.stack.library;
    tree.symlink(&secret, "films/Secret (2001).mp4");
    tree.symlink(&outside.join("Hidden"), "films/Hidden");
    let inside = tree.path("films/Arrival (2016).mp4");
    tree.symlink(&inside, "films/Arrival again (2016).mp4");

    let library = admin.add_library("Films", "films").await;
    let counts = rescan(&admin, library).await;
    assert_eq!(
        (counts.added, counts.failed),
        (1, 0),
        "only the real file is a file: {counts:?}"
    );
    assert_eq!(listed(&admin, library).await, ["Arrival (2016).mp4"]);
    let names: Vec<String> = admin.titles().await.into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["Arrival"]);

    // A file replaced by a link to itself elsewhere is gone, not followed.
    admin
        .stack
        .library
        .rename("films/Arrival (2016).mp4", "kept/Arrival (2016).mp4");
    let kept = admin.stack.library.path("kept/Arrival (2016).mp4");
    admin
        .stack
        .library
        .symlink(&kept, "films/Arrival (2016).mp4");
    admin
        .stack
        .library
        .add_video("films/Heat (1995).mp4", &Video::new(550));
    let counts = rescan(&admin, library).await;
    assert_eq!(
        (counts.added, counts.marked_missing),
        (1, 1),
        "the linked-over file is missing: {counts:?}"
    );
    assert_eq!(listed(&admin, library).await, ["Heat (1995).mp4"]);

    admin.finish().await;
}
