//! REST-facing DTOs for the admin API. These wrap beam-index's
//! `AdminEvent`/`EventLevel`/`EventCategory` and beam-domain's
//! `AdminLog`/`AdminLogLevel`/`AdminLogCategory` rather than adding
//! `serde`/`salvo::oapi` derives directly to those crates, which would pull a
//! web-framework dependency into the indexer/domain layers.

use beam_domain::models::admin_log::{AdminLog, AdminLogCategory, AdminLogLevel};
use beam_index::services::notification::{AdminEvent, EventCategory, EventLevel};
use beam_index::services::scan as index_scan;
use beam_index::services::watch_status::{PollReason, WatchMode, WatchStatusSnapshot};
use chrono::{DateTime, Utc};
use kynos::Schema;
use kynos::schema::unchecked::Unchecked;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum AdminEventLevelDto {
    Info,
    Warning,
    Error,
}

impl From<EventLevel> for AdminEventLevelDto {
    fn from(level: EventLevel) -> Self {
        match level {
            EventLevel::Info => AdminEventLevelDto::Info,
            EventLevel::Warning => AdminEventLevelDto::Warning,
            EventLevel::Error => AdminEventLevelDto::Error,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum AdminEventCategoryDto {
    LibraryScan,
    /// A scan job's structured progress; the event carries `scan`. Sent on
    /// the live stream only, never kept in the recent-event snapshot.
    ScanProgress,
    System,
}

impl From<EventCategory> for AdminEventCategoryDto {
    fn from(category: EventCategory) -> Self {
        match category {
            EventCategory::LibraryScan => AdminEventCategoryDto::LibraryScan,
            EventCategory::ScanProgress => AdminEventCategoryDto::ScanProgress,
            EventCategory::System => AdminEventCategoryDto::System,
        }
    }
}

#[derive(Clone, Debug, Serialize, Schema)]
pub struct AdminEventDto {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub level: AdminEventLevelDto,
    pub category: AdminEventCategoryDto,
    pub message: String,
    pub library_id: Option<String>,
    pub library_name: Option<String>,
    /// The scan job a `scan_progress` event reports on; absent on every
    /// other category.
    pub scan: Option<ScanEvent>,
}

impl From<AdminEvent> for AdminEventDto {
    fn from(event: AdminEvent) -> Self {
        let AdminEvent {
            id,
            timestamp,
            level,
            category,
            message,
            library_id,
            library_name,
            scan,
        } = event;
        Self {
            id,
            timestamp,
            level: level.into(),
            category: category.into(),
            message,
            library_id,
            library_name,
            scan: scan.map(ScanEvent::from),
        }
    }
}

// ── Scan jobs (issue #181) ──────────────────────────────────────────────────

/// What started a scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum ScanTrigger {
    /// An administrator asked for it.
    Manual,
    /// The scan of every library when the server starts.
    Startup,
    /// The periodic backstop rescan.
    Periodic,
    /// The library has just started being polled rather than watched.
    NewlyPolled,
}

impl From<index_scan::ScanTrigger> for ScanTrigger {
    fn from(trigger: index_scan::ScanTrigger) -> Self {
        match trigger {
            index_scan::ScanTrigger::Manual => ScanTrigger::Manual,
            index_scan::ScanTrigger::Startup => ScanTrigger::Startup,
            index_scan::ScanTrigger::Periodic => ScanTrigger::Periodic,
            index_scan::ScanTrigger::NewlyPolled => ScanTrigger::NewlyPolled,
        }
    }
}

/// Where a scan job is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum ScanState {
    /// Waiting for the library -- another scan, or a watcher event, holds it.
    Queued,
    /// Walking the library.
    Running,
    /// Finished; `progress` is final.
    Succeeded,
    /// Stopped: `failure` says why.
    Failed,
}

impl From<index_scan::ScanState> for ScanState {
    fn from(state: index_scan::ScanState) -> Self {
        match state {
            index_scan::ScanState::Queued => ScanState::Queued,
            index_scan::ScanState::Running => ScanState::Running,
            index_scan::ScanState::Succeeded => ScanState::Succeeded,
            index_scan::ScanState::Failed => ScanState::Failed,
        }
    }
}

/// A scan's per-file counts so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct ScanProgress {
    /// Files the walk found to index; absent until the walk has finished.
    pub total_count: Option<u64>,
    /// Files handled so far, whatever became of them.
    pub processed_count: u64,
    /// Files indexed for the first time.
    pub added_count: u64,
    /// Indexed files whose content changed, or which probed at last.
    pub changed_count: u64,
    /// Indexed files found as they were.
    pub unchanged_count: u64,
    /// Files still being written, left for a later visit.
    pub deferred_count: u64,
    /// Files that could not be processed.
    pub failed_count: u64,
    /// Files no longer on disk, newly marked missing.
    pub marked_missing_count: u64,
    /// Missing files that came back.
    pub restored_count: u64,
    /// Files missing for the whole grace period, removed.
    pub purged_count: u64,
}

impl From<index_scan::ScanProgress> for ScanProgress {
    fn from(progress: index_scan::ScanProgress) -> Self {
        let index_scan::ScanProgress {
            total,
            processed,
            added,
            changed,
            unchanged,
            deferred,
            failed,
            marked_missing,
            restored,
            purged,
        } = progress;
        Self {
            total_count: total,
            processed_count: processed,
            added_count: added,
            changed_count: changed,
            unchanged_count: unchanged,
            deferred_count: deferred,
            failed_count: failed,
            marked_missing_count: marked_missing,
            restored_count: restored,
            purged_count: purged,
        }
    }
}

/// One scan of one library. A library has at most one queued or running
/// scan; the latest is kept until the server restarts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct ScanJob {
    pub id: uuid::Uuid,
    pub library_id: uuid::Uuid,
    pub trigger: ScanTrigger,
    pub state: ScanState,
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub progress: ScanProgress,
    /// Why a failed scan failed. Names no filesystem path.
    pub failure: Option<String>,
}

impl From<index_scan::ScanJob> for ScanJob {
    fn from(job: index_scan::ScanJob) -> Self {
        let index_scan::ScanJob {
            id,
            library_id,
            trigger,
            state,
            queued_at,
            started_at,
            finished_at,
            progress,
            failure,
        } = job;
        Self {
            id,
            library_id,
            trigger: trigger.into(),
            state: state.into(),
            queued_at,
            started_at,
            finished_at,
            progress: progress.into(),
            failure,
        }
    }
}

/// Which moment of a scan an event reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    Started,
    /// Sent at most once a second while the scan walks the library.
    Progress,
    Completed,
    Failed,
}

impl From<index_scan::ScanPhase> for ScanPhase {
    fn from(phase: index_scan::ScanPhase) -> Self {
        match phase {
            index_scan::ScanPhase::Started => ScanPhase::Started,
            index_scan::ScanPhase::Progress => ScanPhase::Progress,
            index_scan::ScanPhase::Completed => ScanPhase::Completed,
            index_scan::ScanPhase::Failed => ScanPhase::Failed,
        }
    }
}

/// The scan job a `scan_progress` event reports on (FR-208).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct ScanEvent {
    pub job_id: uuid::Uuid,
    pub phase: ScanPhase,
    pub progress: ScanProgress,
}

impl From<index_scan::ScanEvent> for ScanEvent {
    fn from(event: index_scan::ScanEvent) -> Self {
        let index_scan::ScanEvent {
            job_id,
            phase,
            progress,
        } = event;
        Self {
            job_id,
            phase: phase.into(),
            progress: progress.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum AdminLogLevelDto {
    Info,
    Warning,
    Error,
}

impl From<AdminLogLevel> for AdminLogLevelDto {
    fn from(level: AdminLogLevel) -> Self {
        match level {
            AdminLogLevel::Info => AdminLogLevelDto::Info,
            AdminLogLevel::Warning => AdminLogLevelDto::Warning,
            AdminLogLevel::Error => AdminLogLevelDto::Error,
        }
    }
}

fn category_to_str(category: &AdminLogCategory) -> &'static str {
    match category {
        AdminLogCategory::LibraryScan => "library_scan",
        AdminLogCategory::System => "system",
        AdminLogCategory::Auth => "auth",
        AdminLogCategory::Enrichment => "enrichment",
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct AdminLogEntryDto {
    pub id: String,
    pub level: AdminLogLevelDto,
    pub category: String,
    pub message: String,
    /// Structured context an operator can read, whose shape depends on the
    /// event that produced it.
    ///
    /// `Unchecked` because it genuinely is unconstrained: a scan failure and an
    /// enrichment failure put different keys here. Kynos refuses a bare
    /// `serde_json::Value` in a described type -- the schema would be `true`,
    /// which is a claim the document cannot check -- and this wrapper says so
    /// out loud instead, annotating the property `x-kynos-unchecked`. It is
    /// `#[serde(transparent)]`, so the bytes on the wire are unchanged.
    pub details: Option<Unchecked<serde_json::Value>>,
    pub created_at: String,
}

impl From<AdminLog> for AdminLogEntryDto {
    fn from(log: AdminLog) -> Self {
        Self {
            id: log.id.to_string(),
            level: log.level.into(),
            category: category_to_str(&log.category).to_string(),
            message: log.message,
            details: log.details.map(Unchecked),
            created_at: log.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct CreateLibraryRequest {
    pub name: String,
    pub root_path: String,
}

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct AdminLogCountResponse {
    pub count: u64,
}

// ── Admin user management (issue #85) ────────────────────────────────────────

/// One user row in the admin users tab. `is_admin` is informational and
/// read-only here: admin is derived from the IdP-asserted claim on every
/// login, so there is deliberately no endpoint to change it.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct AdminUserDto {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
    /// Local moderation switch (see `PATCH /v1/admin/users/{id}`): a disabled
    /// user cannot log in and their sessions are revoked on disable.
    pub disabled: bool,
    pub created_at: DateTime<Utc>,
}

impl From<beam_auth::utils::models::User> for AdminUserDto {
    fn from(user: beam_auth::utils::models::User) -> Self {
        let beam_auth::utils::models::User {
            id,
            oidc_issuer: _,
            oidc_subject: _,
            email,
            display_name,
            avatar_url,
            is_admin,
            disabled,
            created_at,
            updated_at: _,
        } = user;
        Self {
            id: id.to_string(),
            display_name,
            email,
            avatar_url,
            is_admin,
            disabled,
            created_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct AdminUserListResponse {
    pub items: Vec<AdminUserDto>,
    /// Total number of users across all pages.
    pub total: u64,
}

/// Body of `PATCH /v1/admin/users/{id}`. `disabled` is the only mutable
/// field: `is_admin` is IdP-claim-driven (recomputed at every login), so a
/// local toggle would be silently overwritten and is deliberately absent.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct UpdateAdminUserRequest {
    pub disabled: bool,
}

// ── Admin system status (issue #85) ──────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct AdminStatusCounts {
    pub users: u64,
    pub libraries: u64,
    pub files: u64,
}

/// Metadata-enrichment queue overview: row counts per state.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct EnrichmentQueueCounts {
    pub pending: u64,
    pub enriched: u64,
    pub unmatched: u64,
    pub failed: u64,
}

impl From<beam_domain::models::enrichment::EnrichmentStatusCounts> for EnrichmentQueueCounts {
    fn from(counts: beam_domain::models::enrichment::EnrichmentStatusCounts) -> Self {
        let beam_domain::models::enrichment::EnrichmentStatusCounts {
            pending,
            enriched,
            unmatched,
            failed,
        } = counts;
        Self {
            pending,
            enriched,
            unmatched,
            failed,
        }
    }
}

/// One recent library-scan admin log entry, slimmed to what the system
/// status tab renders.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct RecentScanDto {
    pub level: AdminLogLevelDto,
    pub message: String,
    pub timestamp: DateTime<Utc>,
}

impl From<AdminLog> for RecentScanDto {
    fn from(log: AdminLog) -> Self {
        let AdminLog {
            id: _,
            level,
            category: _,
            message,
            details: _,
            created_at,
        } = log;
        Self {
            level: level.into(),
            message,
            timestamp: created_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct AdminStatusResponse {
    /// Whole seconds since the server process built its state.
    pub uptime_secs: u64,
    /// Server crate version (`CARGO_PKG_VERSION`).
    pub version: String,
    pub counts: AdminStatusCounts,
    pub enrichment: EnrichmentQueueCounts,
    /// Most recent `library_scan` admin log entries, newest first.
    pub recent_scans: Vec<RecentScanDto>,
    /// How the filesystem watcher observes each library.
    pub watcher: WatcherStatus,
}

/// How a library's changes reach the index between full rescans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryWatchMode {
    /// Native change events (inotify on Linux, FSEvents on macOS).
    Native,
    /// A walk of the library every `BEAM_WATCH_POLL_INTERVAL_SECS`.
    Polling,
    /// Not watched: the watcher is off, or registering the library failed.
    /// Only the periodic rescan (`BEAM_SCAN_INTERVAL_SECS`) sees its changes.
    Unwatched,
}

/// Why a library is polled rather than natively watched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryPollReason {
    /// The root is on a network filesystem (NFS, SMB/CIFS, FUSE, ...), where
    /// changes made by other hosts raise no native event.
    NetworkFilesystem,
    /// Watching it natively hit the OS watch limit.
    WatchLimitReached,
    /// The native watcher could not be created.
    NativeUnavailable,
}

impl From<PollReason> for LibraryPollReason {
    fn from(reason: PollReason) -> Self {
        match reason {
            PollReason::NetworkFilesystem => Self::NetworkFilesystem,
            PollReason::WatchLimitReached => Self::WatchLimitReached,
            PollReason::NativeUnavailable => Self::NativeUnavailable,
        }
    }
}

/// One library's watch state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct LibraryWatch {
    pub library_id: uuid::Uuid,
    pub mode: LibraryWatchMode,
    /// Set exactly when `mode` is `polling`.
    pub poll_reason: Option<LibraryPollReason>,
}

/// The filesystem watcher's state, for the admin system-status tab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WatcherStatus {
    /// Whether the filesystem watcher runs (`BEAM_WATCH_ENABLED`).
    pub enabled: bool,
    /// Whether a native watch has hit the OS watch limit since the server
    /// started. The libraries affected are polled; raising the limit
    /// (`fs.inotify.max_user_watches` on Linux) and restarting watches them
    /// natively again.
    pub watch_limit_reached: bool,
    /// The Linux per-user inotify watch limit (`fs.inotify.max_user_watches`),
    /// `null` where it cannot be read.
    pub watch_limit_count: Option<u64>,
    /// Every library, in the order the library list returns them.
    pub libraries: Vec<LibraryWatch>,
}

impl WatcherStatus {
    /// Join the watcher's snapshot with the libraries that exist. A library
    /// the watcher has no entry for is `unwatched`; an entry for a library
    /// that no longer exists is dropped.
    pub fn from_snapshot(
        snapshot: WatchStatusSnapshot,
        library_ids: impl IntoIterator<Item = uuid::Uuid>,
    ) -> Self {
        let WatchStatusSnapshot {
            enabled,
            limit_reached,
            max_user_watches,
            libraries,
        } = snapshot;
        let libraries = library_ids
            .into_iter()
            .map(|library_id| {
                let (mode, poll_reason) = match libraries.get(&library_id) {
                    Some(WatchMode::Native) => (LibraryWatchMode::Native, None),
                    Some(WatchMode::Polling(reason)) => {
                        (LibraryWatchMode::Polling, Some((*reason).into()))
                    }
                    None => (LibraryWatchMode::Unwatched, None),
                };
                LibraryWatch {
                    library_id,
                    mode,
                    poll_reason,
                }
            })
            .collect();
        Self {
            enabled,
            watch_limit_reached: limit_reached,
            watch_limit_count: max_user_watches,
            libraries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn admin_event_dto_maps_all_fields() {
        let event = AdminEvent {
            id: "evt-1".to_string(),
            timestamp: Utc::now(),
            level: EventLevel::Warning,
            category: EventCategory::LibraryScan,
            message: "scan finished".to_string(),
            library_id: Some("lib-1".to_string()),
            library_name: Some("Movies".to_string()),
            scan: None,
        };
        let dto = AdminEventDto::from(event.clone());
        assert_eq!(dto.id, "evt-1");
        assert!(matches!(dto.level, AdminEventLevelDto::Warning));
        assert!(matches!(dto.category, AdminEventCategoryDto::LibraryScan));
        assert_eq!(dto.message, "scan finished");
        assert_eq!(dto.library_id, Some("lib-1".to_string()));
    }

    #[test]
    fn admin_log_entry_dto_maps_category_to_snake_case_string() {
        let log = AdminLog {
            id: Uuid::new_v4(),
            level: AdminLogLevel::Error,
            category: AdminLogCategory::Enrichment,
            message: "match failed".to_string(),
            details: None,
            created_at: Utc::now(),
        };
        let dto = AdminLogEntryDto::from(log);
        assert_eq!(dto.category, "enrichment");
        assert!(matches!(dto.level, AdminLogLevelDto::Error));
        assert_eq!(dto.message, "match failed");
    }

    #[test]
    fn watcher_status_joins_the_snapshot_with_the_libraries_that_exist() {
        let native = Uuid::from_u128(1);
        let polled = Uuid::from_u128(2);
        let unwatched = Uuid::from_u128(3);
        let deleted = Uuid::from_u128(4);
        let snapshot = WatchStatusSnapshot {
            enabled: true,
            limit_reached: true,
            max_user_watches: Some(8192),
            libraries: [
                (native, WatchMode::Native),
                (polled, WatchMode::Polling(PollReason::WatchLimitReached)),
                (deleted, WatchMode::Native),
            ]
            .into_iter()
            .collect(),
        };

        let status = WatcherStatus::from_snapshot(snapshot, [unwatched, polled, native]);

        assert!(status.enabled);
        assert!(status.watch_limit_reached);
        assert_eq!(status.watch_limit_count, Some(8192));
        assert_eq!(
            status.libraries,
            vec![
                LibraryWatch {
                    library_id: unwatched,
                    mode: LibraryWatchMode::Unwatched,
                    poll_reason: None,
                },
                LibraryWatch {
                    library_id: polled,
                    mode: LibraryWatchMode::Polling,
                    poll_reason: Some(LibraryPollReason::WatchLimitReached),
                },
                LibraryWatch {
                    library_id: native,
                    mode: LibraryWatchMode::Native,
                    poll_reason: None,
                },
            ],
            "library order is preserved, a missing entry is unwatched, and a deleted library is dropped"
        );
    }
}
