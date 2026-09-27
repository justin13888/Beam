use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use beam_domain::models::library_shape::LibraryShape;
use beam_domain::providers::telemetry::{RecordingTelemetrySink, TelemetrySendError};
use beam_domain::repositories::library_shape::MockLibraryShapeRepository;
use beam_domain::repositories::library_shape::in_memory::InMemoryLibraryShapeRepository;
use beam_domain::services::TestClock;
use chrono::{DateTime, TimeZone, Utc};
use tempfile::TempDir;

use super::*;

const URL: &str = "https://collector.example/v1/metrics";

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 27, 10, 0, 0).unwrap()
}

struct Harness {
    service: Arc<LibraryReportService>,
    sink: Arc<RecordingTelemetrySink>,
    clock: Arc<TestClock>,
    state_path: PathBuf,
    _dir: TempDir,
}

fn harness(destination: Option<&str>, sink: RecordingTelemetrySink) -> Harness {
    harness_over(
        destination,
        sink,
        Arc::new(InMemoryLibraryShapeRepository::default()),
    )
}

/// A harness whose report reads `shape_repo`.
fn harness_over(
    destination: Option<&str>,
    sink: RecordingTelemetrySink,
    shape_repo: Arc<dyn LibraryShapeRepository>,
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("telemetry").join("library-report.json");
    let sink = Arc::new(sink);
    let clock = Arc::new(TestClock::starting_at(start()));
    let service = Arc::new(LibraryReportService::new(
        LibraryReportConfig {
            destination: destination.map(str::to_string),
            destination_origin: destination.map(|_| "https://collector.example".to_string()),
            state_path: state_path.clone(),
            server_version: "1.2.3".to_string(),
        },
        shape_repo,
        sink.clone(),
        clock.clone(),
    ));
    Harness {
        service,
        sink,
        clock,
        state_path,
        _dir: dir,
    }
}

fn spawn(harness: &Harness) {
    let service = harness.service.clone();
    tokio::spawn(async move { service.run().await });
}

/// Polls `condition`, yielding to the spawned schedule between checks.
async fn until(label: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("timed out waiting for: {label}");
}

async fn parked(harness: &Harness) {
    until("the schedule to sleep", || {
        harness.clock.waiter_count() == 1
    })
    .await;
}

fn write_state(path: &std::path::Path, last_sent_at: DateTime<Utc>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({ "last_sent_at": last_sent_at })).unwrap(),
    )
    .unwrap();
}

#[test]
fn the_first_report_waits_an_hour_or_a_week_since_the_last_whichever_is_later() {
    let hour = chrono::Duration::hours(1);
    let day = chrono::Duration::days(1);
    for (last_sent_at, expected) in [
        (None, start() + hour),
        (Some(start() - day * 2), start() + day * 5),
        (Some(start() - day * 30), start() + hour),
        // Exactly a week ago: the hour's grace still applies.
        (Some(start() - day * 7), start() + hour),
        // A clock that went backwards past the last send.
        (Some(start() + day), start() + day * 8),
    ] {
        assert_eq!(
            first_send_at(start(), last_sent_at),
            expected,
            "{last_sent_at:?}"
        );
    }
}

#[test]
fn a_retry_doubles_up_to_a_day() {
    let mut delay = INITIAL_RETRY_DELAY;
    let mut seen = vec![delay.as_secs() / 3600];
    for _ in 0..6 {
        delay = next_retry_delay(delay);
        seen.push(delay.as_secs() / 3600);
    }
    assert_eq!(seen, vec![1, 2, 4, 8, 16, 24, 24]);
}

#[tokio::test]
async fn nothing_is_sent_before_an_hour_then_one_report_a_week() {
    let h = harness(Some(URL), RecordingTelemetrySink::new());
    spawn(&h);
    parked(&h).await;

    h.clock.advance(FIRST_SEND_DELAY - Duration::from_secs(1));
    parked(&h).await;
    assert_eq!(h.sink.sent_count(), 0, "one second short of the hour");

    h.clock.advance(Duration::from_secs(1));
    until("the first report", || h.sink.sent_count() == 1).await;
    parked(&h).await;
    let sent = &h.sink.sent()[0];
    assert_eq!(sent.url, URL);
    assert_eq!(sent.content_type, "application/json");
    assert_eq!(
        h.service.last_sent_at().await,
        Some(start() + chrono::Duration::hours(1)),
        "a delivery is recorded"
    );
    assert_eq!(
        h.service.preview().await.unwrap().next_send_at,
        Some(start() + chrono::Duration::hours(1) + chrono::Duration::days(7))
    );

    h.clock.advance(SEND_INTERVAL - Duration::from_secs(1));
    parked(&h).await;
    assert_eq!(h.sink.sent_count(), 1, "one second short of a week");

    h.clock.advance(Duration::from_secs(1));
    until("the second report", || h.sink.sent_count() == 2).await;
}

/// A restart two days after a delivery waits out the rest of the week rather
/// than sending an hour in.
#[tokio::test]
async fn a_restart_keeps_the_weekly_cadence() {
    let h = harness(Some(URL), RecordingTelemetrySink::new());
    write_state(&h.state_path, start() - chrono::Duration::days(2));
    spawn(&h);
    parked(&h).await;

    assert_eq!(
        h.service.preview().await.unwrap().next_send_at,
        Some(start() + chrono::Duration::days(5))
    );
    h.clock.advance(FIRST_SEND_DELAY);
    parked(&h).await;
    assert_eq!(h.sink.sent_count(), 0);
}

#[tokio::test]
async fn a_failed_delivery_is_retried_after_an_hour_and_not_recorded() {
    let h = harness(
        Some(URL),
        RecordingTelemetrySink::new().failing_next(TelemetrySendError::Rejected { status: 503 }),
    );
    spawn(&h);
    parked(&h).await;

    h.clock.advance(FIRST_SEND_DELAY);
    until("the failing attempt", || h.sink.sent_count() == 1).await;
    parked(&h).await;
    assert!(
        !h.state_path.exists(),
        "a delivery the collector refused is not recorded"
    );
    assert_eq!(
        h.service.preview().await.unwrap().next_send_at,
        Some(start() + chrono::Duration::hours(2))
    );

    h.clock.advance(INITIAL_RETRY_DELAY);
    until("the retry", || h.sink.sent_count() == 2).await;
    parked(&h).await;
    assert_eq!(
        h.service.last_sent_at().await,
        Some(start() + chrono::Duration::hours(2))
    );
}

/// NFR-205: a report that cannot be built is a failed delivery like any
/// other -- nothing sent, nothing recorded, retried on the same backoff.
#[tokio::test]
async fn a_store_failure_is_a_failed_delivery_and_is_retried() {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut store = MockLibraryShapeRepository::new();
    let counted = reads.clone();
    store.expect_shape().returning(move || {
        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(DbErr::Custom("connection reset".to_string()))
        } else {
            Ok(LibraryShape::default())
        }
    });
    let h = harness_over(Some(URL), RecordingTelemetrySink::new(), Arc::new(store));
    spawn(&h);
    parked(&h).await;

    h.clock.advance(FIRST_SEND_DELAY);
    until("the failing read", || reads.load(Ordering::SeqCst) == 1).await;
    parked(&h).await;
    assert_eq!(h.sink.sent_count(), 0, "nothing to send");
    assert!(
        !h.state_path.exists(),
        "a report that was never built is not recorded as sent"
    );

    h.clock
        .advance(INITIAL_RETRY_DELAY - Duration::from_secs(1));
    parked(&h).await;
    assert_eq!(h.sink.sent_count(), 0, "one second short of the retry");

    h.clock.advance(Duration::from_secs(1));
    until("the retry", || h.sink.sent_count() == 1).await;
    parked(&h).await;
    assert_eq!(
        h.service.last_sent_at().await,
        Some(start() + chrono::Duration::hours(2))
    );
}

#[tokio::test]
async fn without_a_destination_nothing_is_ever_sent() {
    let h = harness(None, RecordingTelemetrySink::new());

    // Returns rather than parking: there is no schedule to run.
    h.service.run().await;

    assert_eq!(h.clock.waiter_count(), 0);
    assert_eq!(h.sink.sent_count(), 0);
    let preview = h.service.preview().await.unwrap();
    assert_eq!(preview.next_send_at, None);
    assert!(!h.service.destination_configured());
}

/// The preview is the delivery: at one instant, the bytes shown are the
/// bytes sent.
#[tokio::test]
async fn the_preview_is_byte_for_byte_what_is_sent() {
    let h = harness(Some(URL), RecordingTelemetrySink::new());
    spawn(&h);
    parked(&h).await;
    h.clock.advance(FIRST_SEND_DELAY);
    until("the first report", || h.sink.sent_count() == 1).await;

    let preview = h.service.preview().await.unwrap();

    assert_eq!(preview.payload, h.sink.sent()[0].body);
    assert_eq!(preview.content_type, h.sink.sent()[0].content_type);
}

#[tokio::test]
async fn a_corrupt_state_file_reads_as_never_sent() {
    let h = harness(Some(URL), RecordingTelemetrySink::new());
    std::fs::create_dir_all(h.state_path.parent().unwrap()).unwrap();
    std::fs::write(&h.state_path, b"not json").unwrap();

    assert_eq!(h.service.last_sent_at().await, None);
}
