use std::sync::Arc;
use std::time::{Duration, Instant};

use beam_domain::services::{Clock, TestClock};
use chrono::{DateTime, TimeDelta, Utc};
use uuid::Uuid;

use super::*;

fn queued(library_id: Uuid, clock: &TestClock) -> ScanJob {
    ScanJob {
        id: Uuid::new_v4(),
        library_id,
        trigger: ScanTrigger::Manual,
        state: ScanState::Queued,
        queued_at: clock.now(),
        started_at: None,
        finished_at: None,
        progress: ScanProgress::default(),
        failure: None,
    }
}

fn instant(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid instant")
}

// ─── settle_state ───────────────────────────────────────────────────────────

#[test]
fn settle_state_leaves_a_recent_write_for_the_rest_of_the_window() {
    let window = Duration::from_secs(30);
    let now = instant(100);
    // (mtime, expected)
    let cases: [(Option<DateTime<Utc>>, Settle); 6] = [
        (None, Settle::Settled),
        // Dated after now: no evidence of a copy in progress.
        (Some(instant(160)), Settle::Settled),
        (
            Some(instant(100)),
            Settle::Unsettled {
                retry_after: Duration::from_secs(30),
            },
        ),
        (
            Some(instant(90)),
            Settle::Unsettled {
                retry_after: Duration::from_secs(20),
            },
        ),
        // Exactly one window old: settled.
        (Some(instant(70)), Settle::Settled),
        (Some(instant(0)), Settle::Settled),
    ];
    for (mtime, expected) in cases {
        assert_eq!(
            settle_state(now, mtime, window),
            expected,
            "mtime {mtime:?}"
        );
    }
}

#[test]
fn settle_state_with_a_zero_window_settles_even_a_file_written_now() {
    let now = instant(100);
    assert_eq!(
        settle_state(now, Some(now), Duration::ZERO),
        Settle::Settled
    );
}

#[test]
fn settle_state_retries_just_after_the_window_closes() {
    // A sub-second remainder is kept, not rounded away to "settled".
    let now = instant(100);
    let mtime = now - TimeDelta::milliseconds(29_500);
    assert_eq!(
        settle_state(now, Some(mtime), Duration::from_secs(30)),
        Settle::Unsettled {
            retry_after: Duration::from_millis(500),
        }
    );
}

// ─── ProgressThrottle ───────────────────────────────────────────────────────

#[test]
fn the_throttle_lets_the_first_event_through_then_one_per_interval() {
    let start = Instant::now();
    let mut throttle = ProgressThrottle::new(Duration::from_secs(1));

    assert!(throttle.ready(start), "the first event always goes out");
    assert!(!throttle.ready(start), "nothing more at the same instant");
    assert!(!throttle.ready(start + Duration::from_millis(999)));
    assert!(throttle.ready(start + Duration::from_millis(1_000)));
    // The next interval runs from the event that went out, not the first.
    assert!(!throttle.ready(start + Duration::from_millis(1_600)));
    assert!(throttle.ready(start + Duration::from_millis(2_000)));
}

// ─── ScanCoordinator ────────────────────────────────────────────────────────

#[test]
fn a_second_job_is_refused_while_one_is_queued_or_running() {
    let clock = Arc::new(TestClock::new());
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();

    let ticket = coordinator
        .register(queued(library, &clock), clock.clone())
        .expect("the first job registers");
    assert_eq!(
        coordinator
            .register(queued(library, &clock), clock.clone())
            .err(),
        Some(ScanRefused::InProgress),
        "a queued job holds the library"
    );
    ticket.start();
    assert_eq!(
        coordinator
            .register(queued(library, &clock), clock.clone())
            .err(),
        Some(ScanRefused::InProgress),
        "so does a running one"
    );
    assert!(
        coordinator
            .register(queued(Uuid::new_v4(), &clock), clock.clone())
            .is_ok(),
        "another library is not held"
    );

    ticket.succeed(ScanProgress::default());
    let next = coordinator
        .register(queued(library, &clock), clock.clone())
        .expect("a finished job no longer holds the library");
    assert_eq!(
        coordinator.job(library).map(|job| job.id),
        Some(next.job_id()),
        "the latest job replaces the finished one"
    );
}

#[test]
fn a_job_records_its_life_on_the_injected_clock() {
    let clock = Arc::new(TestClock::starting_at(instant(0)));
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();
    let ticket = coordinator
        .register(queued(library, &clock), clock.clone())
        .unwrap();

    clock.advance(Duration::from_secs(5));
    ticket.start();
    let progress = ScanProgress {
        total: Some(3),
        processed: 3,
        added: 2,
        unchanged: 1,
        ..ScanProgress::default()
    };
    clock.advance(Duration::from_secs(7));
    ticket.succeed(progress);

    let job = coordinator.job(library).expect("the job is kept");
    assert_eq!(job.state, ScanState::Succeeded);
    assert_eq!(job.queued_at, instant(0));
    assert_eq!(job.started_at, Some(instant(5)));
    assert_eq!(job.finished_at, Some(instant(12)));
    assert_eq!(job.progress, progress);
    assert_eq!(job.failure, None);
}

#[test]
fn dropping_an_unfinished_ticket_fails_its_job_as_interrupted() {
    let clock = Arc::new(TestClock::new());
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();
    let ticket = coordinator
        .register(queued(library, &clock), clock.clone())
        .unwrap();
    ticket.start();

    drop(ticket);

    let job = coordinator.job(library).unwrap();
    assert_eq!(job.state, ScanState::Failed);
    assert_eq!(job.failure.as_deref(), Some(INTERRUPTED));
    assert!(job.finished_at.is_some());
}

#[test]
fn dropping_a_finished_ticket_leaves_its_job_as_it_ended() {
    let clock = Arc::new(TestClock::new());
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();
    let ticket = coordinator
        .register(queued(library, &clock), clock.clone())
        .unwrap();
    ticket.start();
    ticket.fail("Library not found");

    drop(ticket);

    let job = coordinator.job(library).unwrap();
    assert_eq!(job.failure.as_deref(), Some("Library not found"));
}

#[test]
fn cancelling_reaches_only_the_active_job() {
    let clock = Arc::new(TestClock::new());
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();

    assert!(
        !coordinator.cancel(library, clock.now()),
        "nothing to cancel before a job"
    );
    let first = coordinator
        .register(queued(library, &clock), clock.clone())
        .unwrap();
    assert!(coordinator.cancel(library, clock.now()));
    assert!(first.is_cancelled());

    first.fail(CANCELLED);
    assert!(
        !coordinator.cancel(library, clock.now()),
        "a finished job is not cancelled"
    );
    let second = coordinator
        .register(queued(library, &clock), clock.clone())
        .unwrap();
    assert!(
        !second.is_cancelled(),
        "the last job's cancellation does not carry over"
    );
}

#[tokio::test]
async fn a_reconcile_does_not_get_the_library_while_a_job_is_registered() {
    let clock = Arc::new(TestClock::new());
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();

    let ticket = coordinator
        .register(queued(library, &clock), clock.clone())
        .unwrap();
    assert!(
        coordinator.try_acquire_for_reconcile(library).is_none(),
        "a queued job holds the library even before it takes the lock"
    );
    let guard = coordinator.acquire_for_scan(library).await;
    ticket.start();
    assert!(coordinator.try_acquire_for_reconcile(library).is_none());
    assert!(
        coordinator
            .try_acquire_for_reconcile(Uuid::new_v4())
            .is_some(),
        "another library is free"
    );

    ticket.succeed(ScanProgress::default());
    drop(guard);
    assert!(coordinator.try_acquire_for_reconcile(library).is_some());
}

#[tokio::test]
async fn a_reconcile_does_not_get_a_library_another_reconcile_holds() {
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();

    let held = coordinator
        .try_acquire_for_reconcile(library)
        .expect("free");
    assert!(coordinator.try_acquire_for_reconcile(library).is_none());
    drop(held);
    assert!(coordinator.try_acquire_for_reconcile(library).is_some());
}

#[tokio::test]
async fn the_exclusive_catalog_shuts_out_scans_and_reconciles() {
    let coordinator = Arc::new(ScanCoordinator::new());
    let library = Uuid::new_v4();

    let exclusive = coordinator.exclusive_catalog().await;
    assert!(
        coordinator.try_acquire_for_reconcile(library).is_none(),
        "a reconcile never classifies while the identity passes run"
    );
    let waiting = {
        let coordinator = coordinator.clone();
        tokio::spawn(async move {
            let _guard = coordinator.acquire_for_scan(library).await;
        })
    };
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished(), "a scan waits for the passes");

    drop(exclusive);
    waiting
        .await
        .expect("the scan gets the catalog once they finish");
    assert!(
        coordinator.try_exclusive_catalog().is_some(),
        "and lets it go"
    );
}

#[tokio::test]
async fn the_passes_do_not_get_the_catalog_while_a_scan_holds_it() {
    let coordinator = ScanCoordinator::new();
    let guard = coordinator.acquire_for_scan(Uuid::new_v4()).await;
    assert!(coordinator.try_exclusive_catalog().is_none());
    drop(guard);
    assert!(coordinator.try_exclusive_catalog().is_some());
}

#[tokio::test]
async fn a_subscriber_sees_the_job_finish() {
    let clock = Arc::new(TestClock::new());
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();
    let mut jobs = coordinator.subscribe(library);
    assert_eq!(*jobs.borrow(), None, "no job yet");

    let ticket = coordinator
        .register(queued(library, &clock), clock.clone())
        .unwrap();
    let job_id = ticket.job_id();
    tokio::spawn(async move {
        ticket.start();
        ticket.succeed(ScanProgress::default());
    });

    let finished = tokio::time::timeout(
        Duration::from_secs(10),
        jobs.wait_for(|job| job.as_ref().is_some_and(|job| !job.state.is_active())),
    )
    .await
    .expect("the job finishes")
    .expect("the coordinator is alive")
    .clone()
    .expect("a job");
    assert_eq!(finished.id, job_id);
    assert_eq!(finished.state, ScanState::Succeeded);
}

/// A job still queued when its library is deleted fails as cancelled at
/// once, rather than when its task finally gets the library -- which may be
/// behind another library's scan and the identity passes (PR #224 r2). Its
/// task cannot then revive it.
#[test]
fn cancelling_a_queued_job_fails_it_at_once_and_for_good() {
    let clock = Arc::new(TestClock::starting_at(instant(0)));
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();
    let ticket = coordinator
        .register(queued(library, &clock), clock.clone())
        .unwrap();

    clock.advance(Duration::from_secs(3));
    assert!(coordinator.cancel(library, clock.now()));

    let job = coordinator.job(library).unwrap();
    assert_eq!(job.state, ScanState::Failed);
    assert_eq!(job.failure.as_deref(), Some(CANCELLED));
    assert_eq!(job.finished_at, Some(instant(3)));
    assert!(ticket.is_cancelled());

    clock.advance(Duration::from_secs(4));
    ticket.start();
    ticket.succeed(ScanProgress {
        added: 9,
        ..ScanProgress::default()
    });
    drop(ticket);
    assert_eq!(
        coordinator.job(library),
        Some(job),
        "the task that gets the library later changes nothing"
    );
}

/// A running job is only asked to stop: it finishes the file it is on and
/// fails as cancelled itself.
#[test]
fn cancelling_a_running_job_leaves_it_running_until_it_stops() {
    let clock = Arc::new(TestClock::new());
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();
    let ticket = coordinator
        .register(queued(library, &clock), clock.clone())
        .unwrap();
    ticket.start();

    assert!(coordinator.cancel(library, clock.now()));

    assert_eq!(coordinator.job(library).unwrap().state, ScanState::Running);
    ticket.fail(CANCELLED);
    assert_eq!(coordinator.job(library).unwrap().state, ScanState::Failed);
}

/// Between stopping a library's scan and deleting its rows, nothing may
/// start on it (PR #224 r2): a retired library is refused a scan job and a
/// reconcile, even once its slot has been forgotten.
#[tokio::test]
async fn a_retired_library_is_never_scanned_or_reconciled_again() {
    let clock = Arc::new(TestClock::new());
    let coordinator = ScanCoordinator::new();
    let library = Uuid::new_v4();
    let other = Uuid::new_v4();

    coordinator.retire(library);

    assert_eq!(
        coordinator
            .register(queued(library, &clock), clock.clone())
            .err(),
        Some(ScanRefused::Retired)
    );
    assert!(coordinator.try_acquire_for_reconcile(library).is_none());
    coordinator.forget(library);
    assert_eq!(
        coordinator
            .register(queued(library, &clock), clock.clone())
            .err(),
        Some(ScanRefused::Retired),
        "forgetting the slot does not bring the library back"
    );
    assert!(coordinator.try_acquire_for_reconcile(library).is_none());

    assert!(
        coordinator
            .register(queued(other, &clock), clock.clone())
            .is_ok(),
        "only the retired library is refused"
    );
}
