//! When the library report is sent, and remembering that it was (issue #93,
//! ADR-0019).
//!
//! The first report goes an hour after start -- long enough that a server
//! restarted in a crash loop, or brought up to look around and torn down,
//! never sends one -- and then weekly. A failed delivery is retried after an
//! hour, doubling to a day. When the last delivery happened is kept in a small
//! JSON file under `BEAM_DATA_DIR`, so a restart does not send early.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use beam_domain::providers::telemetry::{TelemetrySendError, TelemetrySink};
use beam_domain::repositories::LibraryShapeRepository;
use beam_domain::services::Clock;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::models::telemetry::LibraryReport;
use crate::services::telemetry::library_report::build_library_report;
use crate::services::telemetry::otlp::{OTLP_JSON_CONTENT_TYPE, encode_otlp_json};

/// How long after start the first report waits.
pub const FIRST_SEND_DELAY: Duration = Duration::from_secs(60 * 60);

/// How often a report is sent once one has been.
pub const SEND_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The first retry after a failed delivery.
pub const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(60 * 60);

/// The longest a retry ever waits.
pub const MAX_RETRY_DELAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Why a report could not be produced or delivered.
#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("reading the library shape failed: {0}")]
    Db(#[from] DbErr),
    #[error("delivering the report failed: {0}")]
    Send(#[from] TelemetrySendError),
}

/// What the report schedule is configured with.
#[derive(Debug, Clone)]
pub struct LibraryReportConfig {
    /// The collector URL, or `None` when the operator has not opted in.
    pub destination: Option<String>,
    /// `scheme://host[:port]` of `destination`, safe to show.
    pub destination_origin: Option<String>,
    /// Where the last delivery time is kept.
    pub state_path: PathBuf,
    /// The Beam release reports name.
    pub server_version: String,
}

/// The report this server would send now, and where it stands.
#[derive(Debug, Clone)]
pub struct LibraryReportPreview {
    pub destination_origin: Option<String>,
    pub last_sent_at: Option<DateTime<Utc>>,
    pub next_send_at: Option<DateTime<Utc>>,
    pub report: LibraryReport,
    pub content_type: &'static str,
    pub payload: Vec<u8>,
}

/// What the state file holds.
#[derive(Debug, Serialize, Deserialize)]
struct SendState {
    last_sent_at: DateTime<Utc>,
}

/// `Duration` to a `chrono::Duration`, saturating: the constants above are
/// all far inside chrono's range.
fn span(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::MAX)
}

/// When the first report after a start at `started_at` is due: an hour after
/// start, or a week after the last delivery, whichever is later.
pub fn first_send_at(
    started_at: DateTime<Utc>,
    last_sent_at: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    let after_start = started_at + span(FIRST_SEND_DELAY);
    match last_sent_at {
        Some(last) => after_start.max(last + span(SEND_INTERVAL)),
        None => after_start,
    }
}

/// The retry delay after one that failed: doubled, capped at a day.
pub fn next_retry_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_RETRY_DELAY)
}

/// Builds, previews and -- when a destination is configured -- periodically
/// sends the anonymous library report.
#[derive(Debug)]
pub struct LibraryReportService {
    config: LibraryReportConfig,
    shape_repo: Arc<dyn LibraryShapeRepository>,
    sink: Arc<dyn TelemetrySink>,
    clock: Arc<dyn Clock>,
    /// When the running schedule will next send; `None` until it starts.
    next_send_at: Mutex<Option<DateTime<Utc>>>,
}

impl LibraryReportService {
    pub fn new(
        config: LibraryReportConfig,
        shape_repo: Arc<dyn LibraryShapeRepository>,
        sink: Arc<dyn TelemetrySink>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            config,
            shape_repo,
            sink,
            clock,
            next_send_at: Mutex::new(None),
        }
    }

    /// Whether the operator has opted in.
    pub fn destination_configured(&self) -> bool {
        self.config.destination.is_some()
    }

    /// The report as of now, and its exact encoding.
    async fn build(&self) -> Result<(LibraryReport, Vec<u8>), TelemetryError> {
        let shape = self.shape_repo.shape().await?;
        let report = build_library_report(
            &shape,
            &self.config.server_version,
            self.clock.now().date_naive(),
        );
        let payload = encode_otlp_json(&report);
        Ok((report, payload))
    }

    /// Exactly what a delivery now would send, sending nothing.
    pub async fn preview(&self) -> Result<LibraryReportPreview, TelemetryError> {
        let (report, payload) = self.build().await?;
        Ok(LibraryReportPreview {
            destination_origin: self.config.destination_origin.clone(),
            last_sent_at: self.last_sent_at().await,
            next_send_at: *self.next_send_at.lock().expect("not poisoned"),
            report,
            content_type: OTLP_JSON_CONTENT_TYPE,
            payload,
        })
    }

    /// When a report last reached the collector. An unreadable or corrupt
    /// state file reads as never: the worst outcome is one report sent early.
    pub async fn last_sent_at(&self) -> Option<DateTime<Utc>> {
        let bytes = tokio::fs::read(&self.config.state_path).await.ok()?;
        serde_json::from_slice::<SendState>(&bytes)
            .ok()
            .map(|state| state.last_sent_at)
    }

    /// Records a delivery at `at`: written beside the target and renamed over
    /// it, so a crash mid-write leaves the old record rather than a torn one.
    async fn record_sent(&self, at: DateTime<Utc>) -> std::io::Result<()> {
        let path = &self.config.state_path;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let body = serde_json::to_vec(&SendState { last_sent_at: at })
            .expect("the state is plain data and always serialises");
        let temp = path.with_extension("json.tmp");
        tokio::fs::write(&temp, body).await?;
        tokio::fs::rename(&temp, path).await
    }

    /// Builds and delivers one report to `destination`, recording the
    /// delivery only once the collector has accepted it.
    async fn send(&self, destination: &str) -> Result<(), TelemetryError> {
        let (_, payload) = self.build().await?;
        self.sink
            .post(destination, OTLP_JSON_CONTENT_TYPE, payload)
            .await?;
        let sent_at = self.clock.now();
        // Delivered either way; a record that failed to write costs one early
        // report after the next restart, which is not worth failing over.
        if let Err(error) = self.record_sent(sent_at).await {
            tracing::warn!(%error, "could not record the library report delivery");
        }
        Ok(())
    }

    fn set_next_send_at(&self, at: DateTime<Utc>) {
        *self.next_send_at.lock().expect("not poisoned") = Some(at);
    }

    /// Runs the schedule until the process exits. Returns at once when no
    /// destination is configured: without one, nothing is ever sent.
    pub async fn run(&self) {
        let Some(destination) = self.config.destination.clone() else {
            return;
        };
        let mut next = first_send_at(self.clock.now(), self.last_sent_at().await);
        let mut retry = INITIAL_RETRY_DELAY;
        loop {
            self.set_next_send_at(next);
            let wait = (next - self.clock.now()).to_std().unwrap_or(Duration::ZERO);
            self.clock.sleep(wait).await;
            match self.send(&destination).await {
                Ok(()) => {
                    tracing::info!("library report delivered");
                    next = self.clock.now() + span(SEND_INTERVAL);
                    retry = INITIAL_RETRY_DELAY;
                }
                Err(error) => {
                    tracing::warn!(%error, retry_in_secs = retry.as_secs(), "library report not delivered");
                    next = self.clock.now() + span(retry);
                    retry = next_retry_delay(retry);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod scheduler_tests;
