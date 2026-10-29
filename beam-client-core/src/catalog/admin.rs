//! The account and operator views: device sessions, the admin dashboard,
//! accounts, the operational log, server events and health.

use super::absolute;
use crate::api::types as wire;
use crate::servers::ServerRecord;

/// A signed-in device, as the profile screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DeviceSession {
    /// Stable identifier, used to revoke it.
    pub id: String,
    /// Opaque per-device hash the server assigned.
    pub device_hash: String,
    /// Address the session was last seen from.
    pub ip: String,
    /// When it was created, as a Unix timestamp.
    pub created_at_unix: i64,
    /// When it was last active, as a Unix timestamp.
    pub last_active_unix: i64,
}

/// Severity of an operational log line or event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LogLevel {
    /// Routine.
    Info,
    /// Notable but not a failure.
    Warning,
    /// A failure.
    Error,
}

/// What part of the server an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum EventCategory {
    /// Progress or completion of a library scan.
    LibraryScan,
    /// Anything else the server reports.
    System,
}

/// Aggregate counts on the admin dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AdminCounts {
    /// Libraries configured.
    pub libraries: u64,
    /// Files indexed.
    pub files: u64,
    /// Users known to the server.
    pub users: u64,
}

/// Progress of the metadata enrichment queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct EnrichmentCounts {
    /// Waiting to be enriched.
    pub pending: u64,
    /// Enriched successfully.
    pub enriched: u64,
    /// Enrichment failed.
    pub failed: u64,
    /// Probed but matched to no title.
    pub unmatched: u64,
}

/// A recent scan line on the admin dashboard.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecentScan {
    /// Severity.
    pub level: LogLevel,
    /// What happened.
    pub message: String,
    /// When, as a Unix timestamp.
    pub timestamp_unix: i64,
}

/// The admin dashboard's snapshot of the server.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminStatus {
    /// Server version.
    pub version: String,
    /// How long the server has been up.
    pub uptime_secs: u64,
    /// Aggregate counts.
    pub counts: AdminCounts,
    /// Enrichment queue progress.
    pub enrichment: EnrichmentCounts,
    /// Most recent scan activity, newest first.
    pub recent_scans: Vec<RecentScan>,
}

/// A user account, as the admin user list shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminUser {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Email address, where the identity provider supplied one.
    pub email: Option<String>,
    /// Absolute avatar URL.
    pub avatar_url: Option<String>,
    /// Whether the account holds administrative rights.
    pub is_admin: bool,
    /// Whether the account is currently blocked from signing in.
    pub disabled: bool,
    /// When the account was created, as a Unix timestamp.
    pub created_at_unix: i64,
}

/// One page of the admin user list, oldest account first.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminUserPage {
    /// The accounts on this page.
    pub items: Vec<AdminUser>,
    /// Total accounts across every page.
    pub total: u64,
    /// Pass as `after` for the next page; `None` on an empty page.
    pub end_cursor: Option<String>,
    /// Whether a page follows this one.
    pub has_next_page: bool,
}

/// One page of the operational log, newest first.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminLogPage {
    /// The log lines on this page.
    pub items: Vec<AdminLogEntry>,
    /// Total log lines across every page.
    pub total: u64,
    /// Pass as `after` for the next page; `None` on an empty page.
    pub end_cursor: Option<String>,
    /// Whether a page follows this one.
    pub has_next_page: bool,
}

/// One operational log line.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminLogEntry {
    /// Stable identifier.
    pub id: String,
    /// Severity.
    pub level: LogLevel,
    /// Subsystem that emitted it.
    pub category: String,
    /// What happened.
    pub message: String,
    /// Structured detail, as the server serialised it.
    pub details: Option<String>,
    /// When, as the server recorded it.
    pub created_at: String,
}

/// One server event, as the admin activity feed shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AdminEvent {
    /// Stable identifier.
    pub id: String,
    /// Severity.
    pub level: LogLevel,
    /// What part of the server it came from.
    pub category: EventCategory,
    /// What happened.
    pub message: String,
    /// The library it concerns, for a scan event.
    pub library_id: Option<String>,
    /// That library's name, so the feed need not resolve it.
    pub library_name: Option<String>,
    /// When, as a Unix timestamp.
    pub timestamp_unix: i64,
}

/// The server's own health report.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ServerHealth {
    /// Overall status string.
    pub status: String,
    /// Server version.
    pub version: String,
    /// How long the server has been up.
    pub uptime_secs: u64,
    /// Database connectivity, as the server reports it.
    pub database: String,
}

impl DeviceSession {
    /// Normalise a generated session summary.
    #[must_use]
    pub fn from_generated(session: wire::SessionSummary) -> Self {
        let wire::SessionSummary {
            created_at,
            device_hash,
            id,
            ip,
            last_active_at,
        } = session;
        Self {
            id: id.to_string(),
            device_hash,
            ip,
            created_at_unix: created_at.0.unix_timestamp(),
            last_active_unix: last_active_at.0.unix_timestamp(),
        }
    }
}

impl LogLevel {
    /// The server speaks one level for its log and its event stream alike.
    fn from_wire(level: wire::LogLevel) -> Self {
        match level {
            wire::LogLevel::Info => Self::Info,
            wire::LogLevel::Warning => Self::Warning,
            wire::LogLevel::Error => Self::Error,
        }
    }
}

/// The admin log category as the text the UniFFI record carries: the server's
/// own spelling, so a native screen shows what the web admin shows.
///
/// Read from the generated enum's own serialisation rather than restated
/// here, so a category the server adds or respells reaches the record as the
/// server spells it, with no table in this crate to fall out of step.
fn log_category_name(category: wire::AdminLogCategory) -> String {
    match serde_json::to_value(category) {
        Ok(serde_json::Value::String(name)) => name,
        // A unit variant serialises as its name; anything else would be a
        // generator change, reported as what it serialised to rather than
        // hidden behind a guess.
        other => format!("{other:?}"),
    }
}

/// An instant as the RFC 3339 text the UniFFI record's `created_at` has
/// always carried.
fn created_at_rfc3339(at: &time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| at.to_string())
}

impl AdminStatus {
    /// Normalise a generated status response.
    #[must_use]
    pub fn from_generated(status: wire::AdminStatus) -> Self {
        let wire::AdminStatus {
            counts,
            enrichment,
            // Which metadata providers are configured (issue #185) is an operator
            // diagnostic the native admin screens do not show yet.
            enrichment_providers: _,
            metadata_providers: _,
            recent_scans,
            uptime_secs,
            version,
            // The watcher block (issue #186) is an operator diagnostic served
            // through the API; the native apps do not surface it.
            watcher: _,
        } = status;
        Self {
            version,
            uptime_secs: u64::try_from(uptime_secs).unwrap_or(0),
            counts: AdminCounts {
                libraries: u64::try_from(counts.libraries).unwrap_or(0),
                files: u64::try_from(counts.files).unwrap_or(0),
                users: u64::try_from(counts.users).unwrap_or(0),
            },
            enrichment: EnrichmentCounts {
                pending: u64::try_from(enrichment.pending).unwrap_or(0),
                enriched: u64::try_from(enrichment.enriched).unwrap_or(0),
                failed: u64::try_from(enrichment.failed).unwrap_or(0),
                unmatched: u64::try_from(enrichment.unmatched).unwrap_or(0),
            },
            recent_scans: recent_scans
                .into_iter()
                .map(|scan| RecentScan {
                    level: LogLevel::from_wire(scan.level),
                    message: scan.message,
                    timestamp_unix: scan.created_at.0.unix_timestamp(),
                })
                .collect(),
        }
    }
}

impl AdminUser {
    /// Normalise a generated admin user row.
    #[must_use]
    pub fn from_generated(user: wire::AdminUser, record: &ServerRecord) -> Self {
        let wire::AdminUser {
            avatar_url,
            created_at,
            disabled,
            display_name,
            email,
            id,
            is_admin,
        } = user;
        Self {
            id: id.to_string(),
            display_name,
            email,
            avatar_url: absolute(record, avatar_url),
            is_admin,
            disabled,
            created_at_unix: created_at.0.unix_timestamp(),
        }
    }
}

impl AdminLogEntry {
    /// Normalise a generated log line.
    #[must_use]
    pub fn from_generated(entry: wire::AdminLogEntry) -> Self {
        let wire::AdminLogEntry {
            category,
            created_at,
            details,
            id,
            level,
            message,
        } = entry;
        Self {
            id: id.to_string(),
            level: LogLevel::from_wire(level),
            category: log_category_name(category),
            message,
            // `details` is an untyped JSON value in the contract, so it is
            // carried across the boundary as text and rendered verbatim
            // rather than being given a shape the server never promised.
            details: details.map(|value| value.to_string()),
            created_at: created_at_rfc3339(&created_at.0),
        }
    }
}

impl AdminUserPage {
    /// Normalise a generated page of accounts, keeping its cursor and total.
    #[must_use]
    pub fn from_generated(connection: wire::AdminUserConnection, record: &ServerRecord) -> Self {
        let wire::AdminUserConnection {
            items,
            page_info,
            total,
        } = connection;
        Self {
            items: items
                .into_iter()
                .map(|user| AdminUser::from_generated(user, record))
                .collect(),
            total: u64::try_from(total).unwrap_or(0),
            end_cursor: page_info.end_cursor,
            has_next_page: page_info.has_next_page,
        }
    }
}

impl AdminLogPage {
    /// Normalise a generated page of the operational log, keeping its cursor
    /// and total.
    #[must_use]
    pub fn from_generated(connection: wire::AdminLogConnection) -> Self {
        let wire::AdminLogConnection {
            items,
            page_info,
            total,
        } = connection;
        Self {
            items: items
                .into_iter()
                .map(AdminLogEntry::from_generated)
                .collect(),
            total: u64::try_from(total).unwrap_or(0),
            end_cursor: page_info.end_cursor,
            has_next_page: page_info.has_next_page,
        }
    }
}

impl AdminEvent {
    /// Normalise a generated server event.
    #[must_use]
    pub fn from_generated(event: wire::AdminEvent) -> Self {
        let wire::AdminEvent {
            category,
            // An enrichment outcome reads, in the feed, as its message; the
            // structured title is for the admin screens that act on it.
            enrichment: _,
            id,
            level,
            library_id,
            library_name,
            message,
            scan: _,
            occurred_at,
        } = event;
        Self {
            id: id.to_string(),
            level: LogLevel::from_wire(level),
            category: match category {
                // A scan's structured progress reads, in the feed, as the
                // scan event it is; the numbers are in the message too.
                wire::AdminEventCategory::LibraryScan | wire::AdminEventCategory::ScanProgress => {
                    EventCategory::LibraryScan
                }
                // An enrichment outcome (issue #185) has no category of its own in
                // the native apps yet; it reads as the server event it is.
                wire::AdminEventCategory::Enrichment | wire::AdminEventCategory::System => {
                    EventCategory::System
                }
            },
            message,
            library_id: library_id.map(|id| id.to_string()),
            library_name,
            timestamp_unix: occurred_at.0.unix_timestamp(),
        }
    }
}

impl ServerHealth {
    /// Normalise a generated health report.
    #[must_use]
    pub fn from_generated(health: wire::HealthStatus) -> Self {
        let wire::HealthStatus {
            checks,
            status,
            checked_at: _,
            uptime_secs,
            version,
        } = health;
        let wire::HealthChecks { database } = checks;
        // The record keeps its text fields: the typed report is read here and
        // spelled as the server spells it, so the native screens are unchanged.
        let wire::DependencyCheck {
            status: database_status,
            detail: database_detail,
        } = database;
        Self {
            status: match status {
                wire::HealthState::Healthy => "healthy",
                wire::HealthState::Degraded => "degraded",
            }
            .to_owned(),
            version,
            uptime_secs: u64::try_from(uptime_secs).unwrap_or(0),
            database: match (database_status, database_detail) {
                (wire::CheckStatus::Ok, _) => "ok".to_owned(),
                (wire::CheckStatus::Error, Some(detail)) => format!("error: {detail}"),
                (wire::CheckStatus::Error, None) => "error".to_owned(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_support::record;

    /// The server sends a session's instants as date-times; the core hands
    /// the apps whole Unix seconds. Two distinct instants, so a swapped pair
    /// cannot pass.
    #[test]
    fn a_device_session_carries_its_instants_as_unix_seconds() {
        let session: wire::SessionSummary = serde_json::from_str(
            r#"{"id":"5e551011-0000-4000-8000-000000000001","device_hash":"d","ip":"127.0.0.1",
                "created_at":"2023-11-14T22:13:20Z","last_active_at":"2023-11-15T00:13:20Z"}"#,
        )
        .expect("the fixture should match the generated type");

        let session = DeviceSession::from_generated(session);

        assert_eq!(session.id, "5e551011-0000-4000-8000-000000000001");
        assert_eq!(session.created_at_unix, 1_700_000_000);
        assert_eq!(session.last_active_unix, 1_700_007_200);
    }

    #[test]
    fn an_admin_log_entry_carries_untyped_details_as_text() {
        // `details` has no schema in the contract, so it is rendered verbatim
        // rather than given a shape the server never promised.
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","level":"warning",
                       "category":"library_scan","message":"skipped",
                       "created_at":"2026-01-01T12:30:00Z",
                       "details":{"path":"/media/x.mkv"}}"#;
        let entry: wire::AdminLogEntry = serde_json::from_str(json).expect("a valid log entry");
        let mapped = AdminLogEntry::from_generated(entry);
        assert_eq!(mapped.id, "00000000-0000-0000-0000-000000000001");
        assert_eq!(mapped.level, LogLevel::Warning);
        assert_eq!(mapped.category, "library_scan");
        assert_eq!(mapped.created_at, "2026-01-01T12:30:00Z");
        assert_eq!(
            mapped.details.as_deref(),
            Some(r#"{"path":"/media/x.mkv"}"#)
        );
    }

    /// Every category the contract declares reaches the record spelled as the
    /// server spells it. The spellings are read from the document the client
    /// is generated from, so neither side is restated here.
    #[test]
    fn every_admin_log_category_keeps_the_servers_spelling() {
        let document: serde_json::Value =
            serde_json::from_str(include_str!("../../api/openapi.json")).expect("the document");
        let spellings = document
            .pointer("/components/schemas/AdminLogCategory/enum")
            .and_then(serde_json::Value::as_array)
            .expect("the contract declares the categories");
        assert!(!spellings.is_empty());
        for spelling in spellings {
            let category: wire::AdminLogCategory =
                serde_json::from_value(spelling.clone()).expect("a declared category");
            assert_eq!(
                serde_json::Value::String(log_category_name(category)),
                *spelling
            );
        }
    }

    fn admin_status(counts: &str, uptime: &str) -> wire::AdminStatus {
        let json = format!(
            r#"{{"uptime_secs":{uptime},"version":"1.2.3","counts":{counts},
                "enrichment":{{"pending":4,"enriched":100,"unmatched":5,"failed":1}},
                "enrichment_providers":[],"metadata_providers":[],
                "recent_scans":[
                    {{"level":"warning","message":"skipped 2","created_at":"2026-01-01T00:00:00Z"}},
                    {{"level":"info","message":"done","created_at":"2026-01-01T00:00:10Z"}}],
                "watcher":{{"enabled":false,"watch_limit_reached":false,"libraries":[]}}}}"#
        );
        serde_json::from_str(&json).expect("a valid admin status")
    }

    #[test]
    fn an_admin_status_carries_its_counts_and_recent_scans_in_order() {
        let status = AdminStatus::from_generated(admin_status(
            r#"{"users":3,"libraries":2,"files":120}"#,
            "3600",
        ));

        assert_eq!(
            status,
            AdminStatus {
                version: "1.2.3".to_owned(),
                uptime_secs: 3600,
                counts: AdminCounts {
                    libraries: 2,
                    files: 120,
                    users: 3,
                },
                enrichment: EnrichmentCounts {
                    pending: 4,
                    enriched: 100,
                    failed: 1,
                    unmatched: 5,
                },
                recent_scans: vec![
                    RecentScan {
                        level: LogLevel::Warning,
                        message: "skipped 2".to_owned(),
                        timestamp_unix: 1_767_225_600,
                    },
                    RecentScan {
                        level: LogLevel::Info,
                        message: "done".to_owned(),
                        timestamp_unix: 1_767_225_610,
                    },
                ],
            }
        );
    }

    #[test]
    fn a_negative_admin_count_reads_as_zero_rather_than_wrapping() {
        // The schema's minimum is not enforced by the generated client, so a
        // negative value decodes; it must not become 18 quintillion users.
        let status = AdminStatus::from_generated(admin_status(
            r#"{"users":-1,"libraries":2,"files":-5}"#,
            "-7",
        ));

        assert_eq!(status.uptime_secs, 0);
        assert_eq!(
            (
                status.counts.users,
                status.counts.libraries,
                status.counts.files
            ),
            (0, 2, 0)
        );
    }

    fn admin_user(avatar: &str) -> wire::AdminUser {
        let json = format!(
            r#"{{"id":"00000000-0000-0000-0000-0000000000b1","display_name":"Bea",
                "email":null,"avatar_url":{avatar},"is_admin":true,"disabled":false,
                "created_at":"2026-01-01T00:00:00Z"}}"#
        );
        serde_json::from_str(&json).expect("a valid admin user")
    }

    #[test]
    fn an_admin_user_s_avatar_is_resolved_against_the_serving_server() {
        let user = AdminUser::from_generated(admin_user(r#""/avatars/b1.png""#), &record());

        assert_eq!(
            user,
            AdminUser {
                id: "00000000-0000-0000-0000-0000000000b1".to_owned(),
                display_name: "Bea".to_owned(),
                email: None,
                avatar_url: Some("https://beam.local:8000/avatars/b1.png".to_owned()),
                is_admin: true,
                disabled: false,
                created_at_unix: 1_767_225_600,
            }
        );
    }

    #[test]
    fn an_admin_user_without_an_avatar_has_none() {
        let user = AdminUser::from_generated(admin_user("null"), &record());
        assert_eq!(user.avatar_url, None);
    }

    fn admin_event(category: &str) -> wire::AdminEvent {
        let json = format!(
            r#"{{"id":"00000000-0000-0000-0000-0000000000e1","occurred_at":"2026-01-01T00:00:00Z",
                "level":"error","category":"{category}","message":"m",
                "library_id":"00000000-0000-0000-0000-00000000000a","library_name":"Films"}}"#
        );
        serde_json::from_str(&json).expect("a valid admin event")
    }

    /// The feed has two categories and the server four: a scan's progress
    /// reads as the scan it belongs to, and an enrichment outcome as a server
    /// event. Every category the contract declares is folded into one of the
    /// two; the declared list is read from the document, so a category the
    /// server adds fails here rather than being silently unhandled.
    #[test]
    fn every_admin_event_category_folds_into_the_feeds_two() {
        let document: serde_json::Value =
            serde_json::from_str(include_str!("../../api/openapi.json")).expect("the document");
        let declared: Vec<&str> = document
            .pointer("/components/schemas/AdminEventCategory/enum")
            .and_then(serde_json::Value::as_array)
            .expect("the contract declares the categories")
            .iter()
            .map(|value| value.as_str().expect("a category name"))
            .collect();
        let folded: Vec<(&str, EventCategory)> = declared
            .iter()
            .map(|name| {
                (
                    *name,
                    AdminEvent::from_generated(admin_event(name)).category,
                )
            })
            .collect();

        assert_eq!(
            folded,
            vec![
                ("library_scan", EventCategory::LibraryScan),
                ("scan_progress", EventCategory::LibraryScan),
                ("enrichment", EventCategory::System),
                ("system", EventCategory::System),
            ]
        );
    }

    #[test]
    fn an_admin_event_carries_its_library_and_instant() {
        let event = AdminEvent::from_generated(admin_event("system"));

        assert_eq!(
            event,
            AdminEvent {
                id: "00000000-0000-0000-0000-0000000000e1".to_owned(),
                level: LogLevel::Error,
                category: EventCategory::System,
                message: "m".to_owned(),
                library_id: Some("00000000-0000-0000-0000-00000000000a".to_owned()),
                library_name: Some("Films".to_owned()),
                timestamp_unix: 1_767_225_600,
            }
        );
    }

    #[test]
    fn a_health_report_narrows_its_uptime() {
        let json = r#"{"status":"healthy","version":"0.1.0","uptime_secs":1234,
                       "checked_at":"2026-01-01T00:00:00Z",
                       "checks":{"database":{"status":"ok","detail":null}}}"#;
        let health: wire::HealthStatus = serde_json::from_str(json).expect("a valid health report");
        let mapped = ServerHealth::from_generated(health);
        assert_eq!(mapped.status, "healthy");
        assert_eq!(mapped.uptime_secs, 1234);
        assert_eq!(mapped.database, "ok");
    }

    #[test]
    fn a_degraded_health_report_keeps_the_failing_checks_reason() {
        let json = r#"{"status":"degraded","version":"0.1.0","uptime_secs":5,
                       "checked_at":"2026-01-01T00:00:00Z",
                       "checks":{"database":{"status":"error","detail":"connection refused"}}}"#;
        let health: wire::HealthStatus = serde_json::from_str(json).expect("a valid health report");
        let mapped = ServerHealth::from_generated(health);
        assert_eq!(mapped.status, "degraded");
        assert_eq!(mapped.database, "error: connection refused");
    }
}
