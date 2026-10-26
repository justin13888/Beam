//! Scans as a job (#181): `POST .../scan` answers `202` with the job and
//! runs it in the background, the job is polled to its end, and a second scan
//! of a library already being scanned is refused with `409` rather than
//! queued behind it.

use beam_test_support::e2e::client::api;
use uuid::Uuid;

use crate::common::{Admin, VIEWER, Video, assert_refused};

/// How many filler files make a scan last long enough to be asked twice:
/// the second request follows the first's `202` at once, and each filler is
/// read whole, hashed and probed before the scan can finish.
const SLOW_FILES: i64 = 4;
/// Each one's size: a hole, so it costs no disk. Small enough that the
/// suite's own SHA-256 record of the tree stays cheap in a debug build.
const SLOW_FILE_BYTES: u64 = 32 << 20;

/// A scan is a job: accepted with `202`, observed through `GET .../scan`
/// until it finishes, and exclusive per library -- a scan asked for while one
/// runs is a `409`, and the running one is the one that finishes.
#[tokio::test]
async fn a_scan_is_a_polled_job_and_a_concurrent_one_is_refused() {
    let mut admin = Admin::start().await;
    admin
        .stack
        .library
        .add_video("films/Arrival (2016).mp4", &Video::new(440));
    for n in 0..SLOW_FILES {
        admin
            .stack
            .library
            .write_sparse(format!("films/Filler {n} (2001).mkv"), SLOW_FILE_BYTES);
    }
    let library = admin.add_library("Films", "films").await;

    let started = admin.api.scan_library(library).await.expect("start a scan");
    assert_eq!(
        started.status().as_u16(),
        202,
        "a scan is accepted, not run"
    );
    let job = started.into_inner();
    assert_eq!(job.library_id, library);
    assert_eq!(job.trigger, api::types::ScanTrigger::Manual);

    // Asked again at once, while the first is still reading its files.
    assert_refused(
        "a second scan of a library being scanned",
        409,
        admin.api.scan_library(library).await,
    );

    let finished = admin.finished_scan(library, job.id).await;
    assert_eq!(
        finished.state,
        api::types::ScanState::Succeeded,
        "{:?}",
        finished.failure
    );
    let api::types::ScanProgress {
        total_count,
        processed_count,
        added_count,
        failed_count,
        ..
    } = finished.progress;
    assert_eq!(total_count, Some(1 + SLOW_FILES));
    assert_eq!(processed_count, 1 + SLOW_FILES);
    assert_eq!(
        (added_count, failed_count),
        (1 + SLOW_FILES, 0),
        "every file is indexed, the filler as a file no probe could read"
    );
    assert!(finished.started_at.is_some() && finished.finished_at.is_some());

    // Once it has finished, the library can be scanned again.
    let again = admin.scan(library).await;
    assert_ne!(again.id, job.id, "a new scan is a new job");

    admin.finish().await;
}

/// Only an administrator scans, and only a library that exists.
#[tokio::test]
async fn a_scan_needs_an_administrator_and_a_library() {
    let mut admin = Admin::start().await;
    admin.stack.library.create_dir("films");
    let library = admin.add_library("Films", "films").await;

    let viewer = admin.stack.client(&admin.stack.sign_in(VIEWER).await);
    assert_refused(
        "a viewer starting a scan",
        403,
        viewer.scan_library(library).await,
    );
    assert_refused(
        "a scan of no library",
        404,
        admin.api.scan_library(Uuid::new_v4()).await,
    );
    assert_refused(
        "the scan of a library never scanned",
        404,
        admin.api.get_library_scan(library).await,
    );

    admin.finish().await;
}
