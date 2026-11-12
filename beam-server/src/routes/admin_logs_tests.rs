//! Subcutaneous tests for the `/v1/admin/logs`, `/v1/admin/events` and
//! `/v1/admin/media/{id}/refresh` routes, over the fixture in
//! [`super::admin_tests`].
//!
//! `/v1/admin/events/stream` needs one thing the others do not: a notification
//! service whose feed *ends*. See [`FiniteEventFeed`].

use std::sync::Arc;

use beam_domain::models::enrichment::EnrichmentTargetId;
use beam_domain::models::{AdminLog, AdminLogCategory, CreateAdminLog};
use beam_domain::repositories::{AdminLogRepository, EnrichmentStateRepository};
use beam_index::services::notification::{AdminEvent, EventCategory, NotificationService};
use kynos::http::StatusCode;
use serde_json::Value;
use tokio::sync::broadcast;

use super::admin_tests::{
    build_client, make_test_state, make_test_state_with_notification, seed_user_session,
};
use crate::models::AdminLogConnection;
use crate::routes::test_support::outage;

/// A notification service whose live feed is finite.
///
/// `stream_admin_events` reads its receiver until the channel *closes*, which
/// is the right shape for a server that streams until shutdown and the wrong
/// shape for a test: every other notification service keeps its sender alive on
/// the `AppState`, so the response body never ends and the request never
/// returns. This one creates the channel inside `subscribe`, publishes the
/// backlog, and drops the sender before handing the receiver back -- a
/// `broadcast::Receiver` drains what was queued before its sender went away and
/// only then reports `Closed`, so the handler sees exactly these events and
/// then a clean end of stream.
///
/// It is scaffolding, not a subject: what the tests below assert is what the
/// handler encoded onto the wire.
#[derive(Debug)]
struct FiniteEventFeed {
    events: Vec<AdminEvent>,
}

impl NotificationService for FiniteEventFeed {
    fn publish(&self, _event: AdminEvent) {}

    fn subscribe(&self) -> broadcast::Receiver<AdminEvent> {
        let (sender, receiver) = broadcast::channel(self.events.len().max(1));
        for event in &self.events {
            let _ = sender.send(event.clone());
        }
        receiver
    }

    fn recent_events(&self, limit: usize) -> Vec<AdminEvent> {
        self.events.iter().take(limit).cloned().collect()
    }
}

// ─── Admin logs ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn reading_admin_logs_as_a_regular_user_is_403() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;

    let response = client
        .get("/v1/admin/logs")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_admin_reads_the_seeded_log_entries() {
    let fixture = make_test_state();
    fixture
        .state
        .services
        .admin_log
        .create(beam_domain::models::CreateAdminLog {
            level: beam_domain::models::AdminLogLevel::Info,
            category: beam_domain::models::AdminLogCategory::System,
            message: "server started".to_string(),
            details: None,
        })
        .await
        .unwrap();

    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let response = client
        .get("/v1/admin/logs")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let logs: AdminLogConnection = response.json();
    assert_eq!(logs.items.len(), 1);
    assert_eq!(logs.items[0].message, "server started");
    assert_eq!(logs.total, 1, "the count rides on the page");
}

/// The log pages newest first by `first`/`after`: each page's `end_cursor`
/// starts the next, the pages cover every entry once, `total` is the whole
/// count on each, and the flags say where each page sits.
#[tokio::test]
async fn the_admin_log_pages_by_cursor() {
    let fixture = make_test_state();
    for message in ["first", "second", "third"] {
        fixture
            .state
            .services
            .admin_log
            .create(beam_domain::models::CreateAdminLog {
                level: beam_domain::models::AdminLogLevel::Info,
                category: beam_domain::models::AdminLogCategory::System,
                message: message.to_string(),
                details: None,
            })
            .await
            .unwrap();
    }
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let page1: AdminLogConnection = client
        .get("/v1/admin/logs?first=2")
        .cookie("beam_session", &token)
        .send()
        .await
        .json();
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.total, 3);
    assert!(page1.page_info.has_next_page);
    assert!(!page1.page_info.has_previous_page);

    let after = page1
        .page_info
        .end_cursor
        .clone()
        .expect("a full page ends somewhere");
    let page2: AdminLogConnection = client
        .get(&format!("/v1/admin/logs?first=2&after={after}"))
        .cookie("beam_session", &token)
        .send()
        .await
        .json();
    assert_eq!(page2.items.len(), 1);
    assert_eq!(page2.total, 3);
    assert!(!page2.page_info.has_next_page);
    assert!(page2.page_info.has_previous_page);

    let messages: Vec<&str> = page1
        .items
        .iter()
        .chain(&page2.items)
        .map(|entry| entry.message.as_str())
        .collect();
    let mut sorted = messages.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        ["first", "second", "third"],
        "every entry once: {messages:?}"
    );
}

/// A cursor the log did not issue -- the user list's, or none at all -- is
/// `400 #invalid-cursor`, never read as a position.
#[tokio::test]
async fn the_admin_log_refuses_a_cursor_it_did_not_issue() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let users = crate::services::cursor::offset::encode(
        crate::services::cursor::offset::OffsetList::AdminUsers,
        0,
    );

    for after in [users.as_str(), "not-a-cursor"] {
        client
            .get(&format!("/v1/admin/logs?after={after}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-cursor");
    }
}

/// An admin log store that fails every call.
#[derive(Debug)]
struct UnavailableAdminLog;

#[async_trait::async_trait]
impl AdminLogRepository for UnavailableAdminLog {
    async fn create(&self, _entry: CreateAdminLog) -> Result<AdminLog, sea_orm::DbErr> {
        Err(outage())
    }

    async fn list(&self, _limit: u64, _offset: u64) -> Result<Vec<AdminLog>, sea_orm::DbErr> {
        Err(outage())
    }

    async fn list_by_category(
        &self,
        _category: AdminLogCategory,
        _limit: u64,
        _offset: u64,
    ) -> Result<Vec<AdminLog>, sea_orm::DbErr> {
        Err(outage())
    }

    async fn count(&self) -> Result<u64, sea_orm::DbErr> {
        Err(outage())
    }
}

/// NFR-205: an admin log store that cannot be read is a 500 on both routes
/// that read it, never an empty log or an empty scan history, and the
/// store's message stays off the wire.
#[tokio::test]
async fn an_admin_log_store_failure_is_a_500_on_the_log_and_the_status() {
    let mut fixture = make_test_state();
    let mut services = fixture.state.services.clone();
    services.admin_log = Arc::new(UnavailableAdminLog);
    fixture.state = crate::state::AppState::new(
        fixture.state.config.clone(),
        services,
        fixture.state.probe.clone(),
        None,
    );
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    for path in ["/v1/admin/logs", "/v1/admin/status"] {
        let response = client.get(path).cookie("beam_session", &token).send().await;
        response
            .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
        assert!(
            !response.text().contains("store unavailable"),
            "{path}: the store's message never reaches the wire"
        );
    }
}

// ─── Admin events: snapshot and live stream ─────────────────────────────────

/// The snapshot endpoint returns the *newest* events, bounded by `first`,
/// and says older ones were left out.
#[tokio::test]
async fn the_admin_event_snapshot_returns_the_newest_events_within_the_limit() {
    let fixture = make_test_state();
    for message in ["oldest", "middle", "newest"] {
        fixture.notification.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            message,
            None,
            None,
        ));
    }

    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let response = client
        .get("/v1/admin/events?first=2")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json();
    assert_eq!(
        body["page_info"]["has_next_page"], true,
        "an older event exists"
    );
    assert_eq!(body["page_info"]["has_previous_page"], false);
    let messages: Vec<&str> = body["items"]
        .as_array()
        .expect("a connection's items")
        .iter()
        .map(|event| event["message"].as_str().expect("a message string"))
        .collect();
    assert_eq!(messages, ["middle", "newest"]);
}

/// The snapshot carries operational detail -- library names, scan failures
/// -- so a regular user is refused it by name and an anonymous caller is
/// asked to sign in, and neither body leaks an event.
#[tokio::test]
async fn the_admin_event_snapshot_is_admin_only() {
    let fixture = make_test_state();
    fixture.notification.publish(AdminEvent::info(
        EventCategory::LibraryScan,
        "scan of a private library",
        None,
        Some("Private".to_string()),
    ));
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;

    let refused = client
        .get("/v1/admin/events")
        .cookie("beam_session", &token)
        .send()
        .await;
    refused
        .assert_status(StatusCode::FORBIDDEN)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#admin-required");
    assert!(!refused.text().contains("Private"));

    let anonymous = client.get("/v1/admin/events").send().await;
    anonymous.assert_status(StatusCode::UNAUTHORIZED);
    assert!(!anonymous.text().contains("Private"));
}

#[tokio::test]
async fn the_admin_event_stream_is_admin_only() {
    let fixture = make_test_state_with_notification(Arc::new(FiniteEventFeed { events: vec![] }));
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;

    // Authentication resolves before the stream is committed, so this is a
    // normal response rather than an error arriving after a 200 is on the wire.
    assert_eq!(
        client
            .get("/v1/admin/events/stream")
            .cookie("beam_session", &token)
            .send()
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    assert_eq!(
        client.get("/v1/admin/events/stream").send().await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// Each broadcast event reaches the client as one SSE record carrying the DTO,
/// under the fields an intermediary needs to leave the stream alone.
#[tokio::test]
async fn the_admin_event_stream_encodes_each_event_as_json() {
    let fixture = make_test_state_with_notification(Arc::new(FiniteEventFeed {
        events: vec![
            AdminEvent::info(
                EventCategory::LibraryScan,
                "scan started",
                Some(uuid::Uuid::from_u128(1)),
                Some("Movies".to_string()),
            ),
            AdminEvent::warning(EventCategory::System, "disk nearly full", None, None),
            AdminEvent::info(
                EventCategory::ScanProgress,
                "Scanning 'Movies': 3 of 8 files",
                Some(uuid::Uuid::from_u128(1)),
                Some("Movies".to_string()),
            )
            .with_scan(beam_index::services::scan::ScanEvent {
                job_id: uuid::Uuid::nil(),
                phase: beam_index::services::ScanPhase::Progress,
                progress: beam_index::services::ScanProgress {
                    total: Some(8),
                    processed: 3,
                    added: 3,
                    ..Default::default()
                },
            }),
            AdminEvent::warning(
                EventCategory::Enrichment,
                "No enrichment match for \"Heat\"",
                None,
                None,
            )
            .with_enrichment(beam_index::services::notification::EnrichmentEvent {
                target: EnrichmentTargetId::Show(uuid::Uuid::from_u128(7)),
                title: Some("Heat".to_string()),
                status: beam_domain::models::enrichment::EnrichmentStatus::Unmatched,
                matched_ref: None,
                error: Some("no candidate cleared the match threshold".to_string()),
            }),
        ],
    }));
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let response = client
        .get("/v1/admin/events/stream")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.header("content-type"),
        Some("text/event-stream"),
        "an EventSource only consumes this media type"
    );
    // Without these an nginx in front of the server buffers the stream and the
    // dashboard updates in bursts minutes apart.
    assert_eq!(response.header("cache-control"), Some("no-cache"));
    assert_eq!(response.header("x-accel-buffering"), Some("no"));

    let events = response.events();
    assert_eq!(events.len(), 4, "one record per broadcast event");

    // An enrichment outcome arrives with the title it is about (FR-309).
    let fourth: Value = events[3].json();
    assert_eq!(fourth["category"], "enrichment");
    assert_eq!(fourth["level"], "warning");
    assert_eq!(
        fourth["enrichment"]["media_id"],
        uuid::Uuid::from_u128(7).to_string()
    );
    assert_eq!(fourth["enrichment"]["kind"], "show");
    assert_eq!(fourth["enrichment"]["title"], "Heat");
    assert_eq!(fourth["enrichment"]["status"], "unmatched");
    assert_eq!(
        fourth["enrichment"]["error"],
        "no candidate cleared the match threshold"
    );
    assert_eq!(events[2].json::<Value>()["enrichment"], Value::Null);

    let first: Value = events[0].json();
    assert_eq!(first["message"], "scan started");
    assert_eq!(first["level"], "info");
    assert_eq!(first["category"], "library_scan");
    assert_eq!(first["library_name"], "Movies");

    let second: Value = events[1].json();
    assert_eq!(second["message"], "disk nearly full");
    assert_eq!(second["level"], "warning");
    assert_eq!(second["category"], "system");
    assert_eq!(
        second.get("scan"),
        Some(&Value::Null),
        "only a scan-progress event carries a scan; the others send null, like every absent \
         optional field"
    );

    // A scan's progress arrives as structured data a client can render
    // without parsing the message (FR-208).
    let third: Value = events[2].json();
    assert_eq!(third["category"], "scan_progress");
    assert_eq!(third["scan"]["job_id"], uuid::Uuid::nil().to_string());
    assert_eq!(third["scan"]["phase"], "progress");
    assert_eq!(third["scan"]["progress"]["total_count"], 8);
    assert_eq!(third["scan"]["progress"]["processed_count"], 3);
    assert_eq!(third["scan"]["progress"]["added_count"], 3);
}

/// A feed whose subscriber has already fallen behind: the channel holds one
/// event, every event is sent before the receiver reads, and the sender is
/// dropped -- so the first read reports `Lagged`, the next returns the one
/// event the buffer kept, and the one after that `Closed`.
#[derive(Debug)]
struct LaggingEventFeed {
    events: Vec<AdminEvent>,
}

impl NotificationService for LaggingEventFeed {
    fn publish(&self, _event: AdminEvent) {}

    fn subscribe(&self) -> broadcast::Receiver<AdminEvent> {
        let (sender, receiver) = broadcast::channel(1);
        for event in &self.events {
            let _ = sender.send(event.clone());
        }
        receiver
    }

    fn recent_events(&self, _limit: usize) -> Vec<AdminEvent> {
        Vec::new()
    }
}

/// A dashboard that fell behind the broadcast buffer skips what it missed and
/// keeps receiving, rather than having its stream ended under it.
#[tokio::test]
async fn a_lagging_event_stream_skips_and_keeps_delivering() {
    let fixture = make_test_state_with_notification(Arc::new(LaggingEventFeed {
        events: ["missed", "also missed", "delivered"]
            .into_iter()
            .map(|message| AdminEvent::info(EventCategory::System, message, None, None))
            .collect(),
    }));
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let response = client
        .get("/v1/admin/events/stream")
        .cookie("beam_session", &token)
        .send()
        .await;

    response.assert_status(StatusCode::OK);
    let messages: Vec<String> = response
        .events()
        .iter()
        .map(|event| {
            event.json::<Value>()["message"]
                .as_str()
                .expect("a message string")
                .to_owned()
        })
        .collect();
    assert_eq!(messages, ["delivered"]);
}

// ─── Refresh metadata ────────────────────────────────────────────────────────

#[tokio::test]
async fn refreshing_media_metadata_as_a_regular_user_is_403_and_queues_nothing() {
    use beam_domain::models::CreateMovie;
    use beam_domain::models::enrichment::EnrichmentStatus;
    use beam_domain::repositories::MovieRepository;

    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;
    // A real title, enriched, so a refresh that got through would be visible
    // as the row going back to pending.
    let movie = fixture
        .movie_repo
        .find_or_create_by_identity(CreateMovie::new("Heat".to_string(), Some(1995), None))
        .await
        .unwrap();
    let target = EnrichmentTargetId::Movie(movie.id);
    fixture
        .enrichment_repo
        .ensure_pending(target)
        .await
        .unwrap();
    let row = fixture
        .enrichment_repo
        .find_by_target(target)
        .await
        .unwrap()
        .unwrap();
    fixture
        .enrichment_repo
        .mark_enriched(row.id, "tmdb:949", 0.9, chrono::Utc::now())
        .await
        .unwrap();

    client
        .post(&format!("/v1/admin/media/{}/refresh", movie.id))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#admin-required");

    let row = fixture
        .enrichment_repo
        .find_by_target(target)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, EnrichmentStatus::Enriched, "nothing was queued");
    assert!(!row.force_refresh, "nothing was marked for a fresh fetch");
}

#[tokio::test]
async fn refreshing_media_metadata_as_an_admin_queues_the_title_and_is_204() {
    use beam_domain::models::CreateMovie;
    use beam_domain::models::enrichment::EnrichmentStatus;
    use beam_domain::repositories::MovieRepository;

    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let movie = fixture
        .movie_repo
        .find_or_create_by_identity(CreateMovie::new("Heat".to_string(), Some(1995), None))
        .await
        .unwrap();
    let target = EnrichmentTargetId::Movie(movie.id);
    fixture
        .enrichment_repo
        .ensure_pending(target)
        .await
        .unwrap();
    let row = fixture
        .enrichment_repo
        .find_by_target(target)
        .await
        .unwrap()
        .unwrap();
    fixture
        .enrichment_repo
        .mark_enriched(row.id, "tmdb:949", 0.9, chrono::Utc::now())
        .await
        .unwrap();

    let response = client
        .post(&format!("/v1/admin/media/{}/refresh", movie.id))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let row = fixture
        .enrichment_repo
        .find_by_target(target)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, EnrichmentStatus::Pending);
    assert!(row.force_refresh, "re-fetched fresh");
    assert_eq!(
        row.matched_ref.as_deref(),
        Some("tmdb:949"),
        "keeps its match"
    );
}

#[tokio::test]
async fn refreshing_an_unknown_media_id_is_404() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    client
        .post("/v1/admin/media/0199a1f0-0000-7000-8000-000000000001/refresh")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#media-not-found");
}

/// The route half of the malformed-id fix, which had no test at all.
///
/// The 204 case above used to pass `some-id` -- not a UUID -- and assert 204,
/// codifying the answer production had stopped giving. It only passed because
/// this file's stub ignored its argument while the two sibling stubs had been
/// taught to honour it.
#[tokio::test]
async fn refreshing_a_malformed_media_id_is_400() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    client
        .post("/v1/admin/media/not-a-uuid/refresh")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}
