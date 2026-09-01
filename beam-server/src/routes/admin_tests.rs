//! Subcutaneous tests for the `/v1/libraries` and `/v1/admin/*` REST routes.
//!
//! Driven through Kynos's in-process `TestClient` over a real router: real
//! handlers, real `AdminAuth`/`SessionAuth` extractors, and in-memory
//! implementations for everything below the trait line -- no Redis, no
//! PostgreSQL. Library CRUD/scan runs against the real `LocalLibraryService`
//! (backed by in-memory repos), not a stub, so this also exercises that
//! service's actual logic through the REST surface.
//!
//! `/v1/admin/events/stream` needs one thing the others do not: a notification
//! service whose feed *ends*. See [`FiniteEventFeed`].

use std::path::PathBuf;
use std::sync::Arc;

use beam_auth::utils::oidc_config::OidcRuntimeConfig;
use beam_auth::utils::{
    models::{CreateUser, User},
    oidc::NotConfiguredOidcClient,
    pending_auth_store::in_memory::InMemoryPendingAuthStore,
    repository::{UserRepository, in_memory::InMemoryUserRepository},
    session_store::{SessionData, SessionStore, in_memory::InMemorySessionStore},
};
use beam_domain::models::enrichment::EnrichmentTargetId;
use beam_domain::models::file::{CreateMediaFile, FileStatus};
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::{EnrichmentStateRepository, FileRepository};
use beam_index::services::index::MockIndexService;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::models::{
    AdminLogEntryDto, AdminStatusResponse, AdminUserListResponse, CreateLibraryRequest, Library,
    ScanLibraryResponse, UpdateAdminUserRequest,
};
use crate::routes::admin::{
    create_library, delete_library, get_admin_events, get_admin_log_count, get_admin_logs,
    get_admin_status, get_library, get_library_files, list_admin_users, list_libraries,
    refresh_media_metadata, scan_library, stream_admin_events, update_admin_user,
};
use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
use crate::services::hash::HashService;
use crate::services::library::{InMemoryPathValidator, LibraryService, LocalLibraryService};
use crate::services::metadata::{
    MediaConnection, MediaFilter, MediaSearchFilters, MediaSortField, MetadataError,
    MetadataService, PageInfo, SortOrder,
};
use crate::services::notification::{
    AdminEvent, EventCategory, InMemoryNotificationService, NotificationService,
};
use crate::services::playback::{
    ContinueWatchingItem, PlaybackError, PlaybackProgressDto, PlaybackReadError, PlaybackService,
};
use crate::state::{AppServices, AppState};

#[derive(Debug)]
struct StubPlaybackService;

#[async_trait::async_trait]
impl PlaybackService for StubPlaybackService {
    async fn report_progress(
        &self,
        _user_id: uuid::Uuid,
        _file_id: uuid::Uuid,
        _position_secs: f64,
        _duration_secs: Option<f64>,
    ) -> Result<PlaybackProgressDto, PlaybackError> {
        unimplemented!("not called in admin route tests")
    }

    async fn get_continue_watching(
        &self,
        _user_id: uuid::Uuid,
        _limit: u32,
    ) -> Result<Vec<ContinueWatchingItem>, PlaybackReadError> {
        unimplemented!("not called in admin route tests")
    }

    async fn get_history(
        &self,
        _user_id: uuid::Uuid,
        _limit: u64,
        _offset: u64,
    ) -> Result<(Vec<crate::services::playback::HistoryItem>, u64), PlaybackReadError> {
        unimplemented!("not called in admin route tests")
    }
}

#[derive(Debug)]
struct StubHashService;

#[async_trait::async_trait]
impl HashService for StubHashService {
    fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
        unimplemented!("not called in admin route tests")
    }
    async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
        unimplemented!("not called in admin route tests")
    }
}

#[derive(Debug, Default)]
struct StubMetadataService;

#[async_trait::async_trait]
impl MetadataService for StubMetadataService {
    async fn get_media_metadata(&self, _media_id: &str) -> Option<crate::models::MediaMetadata> {
        None
    }
    async fn search_media(
        &self,
        _first: Option<u32>,
        _after: Option<String>,
        _last: Option<u32>,
        _before: Option<String>,
        _sort_by: MediaSortField,
        _sort_order: SortOrder,
        _filters: MediaSearchFilters,
    ) -> MediaConnection {
        MediaConnection {
            edges: vec![],
            page_info: PageInfo {
                has_next_page: false,
                has_previous_page: false,
                start_cursor: None,
                end_cursor: None,
            },
        }
    }
    async fn refresh_metadata(&self, _filter: MediaFilter) -> Result<(), MetadataError> {
        Ok(())
    }
    async fn get_media_sources(
        &self,
        _media_id: &str,
    ) -> Result<Vec<crate::models::MediaSource>, MetadataError> {
        unimplemented!("not called in admin route tests")
    }
}

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

type InMemoryFileRepo = beam_domain::repositories::file::in_memory::InMemoryFileRepository;
type InMemoryEnrichmentRepo =
    beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;

struct TestFixture {
    state: AppState,
    session_store: Arc<InMemorySessionStore>,
    user_repo: Arc<InMemoryUserRepository>,
    file_repo: Arc<InMemoryFileRepo>,
    enrichment_repo: Arc<InMemoryEnrichmentRepo>,
    notification: Arc<dyn NotificationService>,
}

fn make_test_state() -> TestFixture {
    make_test_state_with_notification(Arc::new(InMemoryNotificationService::new()))
}

fn make_test_state_with_notification(notification: Arc<dyn NotificationService>) -> TestFixture {
    let session_store = Arc::new(InMemorySessionStore::default());
    let user_repo = Arc::new(InMemoryUserRepository::default());

    let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(Arc::new(
        InMemoryAdminLogRepository::default(),
    )));

    // Shared between the library service and `AppServices::{library_repo,
    // file_repo}` so the status endpoint's counts reflect libraries/files
    // created through the library service.
    let library_repo = Arc::new(InMemoryLibraryRepository::default());
    let file_repo = Arc::new(InMemoryFileRepo::default());
    let enrichment_repo = Arc::new(InMemoryEnrichmentRepo::default());

    let library: Arc<dyn LibraryService> = Arc::new(LocalLibraryService::new(
        library_repo.clone(),
        file_repo.clone(),
        PathBuf::from("/videos"),
        notification.clone(),
        Arc::new({
            let mut mock_index = MockIndexService::new();
            mock_index.expect_scan_library().returning(|_| Ok(0));
            mock_index
        }),
        Arc::new(InMemoryPathValidator::success(PathBuf::from(
            "/videos/movies",
        ))),
    ));

    let services = AppServices {
        hash: Arc::new(StubHashService),
        library,
        metadata: Arc::new(StubMetadataService),
        notification: notification.clone(),
        admin_log,
        user_repo: user_repo.clone(),
        playback: Arc::new(StubPlaybackService),
        genre_repo: Arc::new(
            beam_domain::repositories::genre::in_memory::InMemoryGenreRepository::default(),
        ),
        library_repo,
        file_repo: file_repo.clone(),
        enrichment_repo: enrichment_repo.clone(),
        movie_repo: Arc::new(
            beam_domain::repositories::movie::in_memory::InMemoryMovieRepository::default(),
        ),
        show_repo: Arc::new(
            beam_domain::repositories::show::in_memory::InMemoryShowRepository::default(),
        ),
        artwork: crate::routes::test_support::cold_artwork_cache(),
        session_store: session_store.clone(),
        oidc_client: Arc::new(NotConfiguredOidcClient::new("not used in these tests")),
        pending_auth_store: Arc::new(InMemoryPendingAuthStore::default()),
        oidc_config: OidcRuntimeConfig {
            web_url: "http://localhost:5173".to_string(),
            cookie_secure: false,
            admin_claim: None,
            admin_value: None,
            session_idle_days: 14,
            session_max_days: 60,
        },
    };

    let config = crate::config::ServerConfig {
        video_dir: PathBuf::from("/tmp"),
        data_dir: PathBuf::from("/tmp"),
        database_url: "postgres://unused:unused@localhost/unused".to_string(),
        watch_enabled: false,
        anilist_enabled: false,
        cookie_secure: Some(false),
        ..Default::default()
    };

    let state = AppState::new(
        config,
        services,
        Arc::new(crate::services::health::InMemoryDependencyProbe::healthy()),
        None,
    );

    TestFixture {
        state,
        session_store,
        user_repo,
        file_repo,
        enrichment_repo,
        notification,
    }
}

/// Seeds a user + session directly (bypassing the OIDC login flow, which
/// isn't under test here) and returns the `beam_session` cookie value.
async fn seed_user_session(fixture: &TestFixture, is_admin: bool) -> String {
    let (oidc_subject, email, display_name) = if is_admin {
        ("admin-subj", "admin@example.com", "Admin User")
    } else {
        ("regular-subj", "regular@example.com", "Regular User")
    };

    let user = fixture
        .user_repo
        .create(CreateUser {
            oidc_issuer: "https://test.example".to_string(),
            oidc_subject: oidc_subject.to_string(),
            email: Some(email.to_string()),
            display_name: display_name.to_string(),
            avatar_url: None,
            is_admin,
        })
        .await
        .expect("seed user should succeed");

    fixture
        .session_store
        .create(
            &SessionData {
                user_id: user.id.to_string(),
                device_hash: "test-device".to_string(),
                ip: "127.0.0.1".to_string(),
                created_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
            },
            86400,
            86400,
        )
        .await
        .expect("seed session should succeed")
}

/// The library and admin operations, mounted the way `rest_routes` mounts
/// them. Two `mount` calls rather than one: `routes!` builds a tuple, and one
/// list of every operation runs the arity out.
fn build_client(fixture: &TestFixture) -> TestClient<AppState> {
    let service = Router::new()
        .nest(
            "/v1",
            Router::new()
                .mount(kynos::routes![
                    list_libraries,
                    get_library,
                    get_library_files,
                    create_library,
                    scan_library,
                    refresh_media_metadata,
                    delete_library,
                ])
                .mount(kynos::routes![
                    get_admin_logs,
                    get_admin_log_count,
                    get_admin_events,
                    stream_admin_events,
                    list_admin_users,
                    update_admin_user,
                    get_admin_status,
                ]),
        )
        .build(fixture.state.clone())
        .expect("the admin router describes itself");

    TestClient::new(service)
}

// ─── Library reads ──────────────────────────────────────────────────────────

#[tokio::test]
async fn listing_libraries_requires_a_session() {
    let fixture = make_test_state();
    let client = build_client(&fixture);

    assert_eq!(
        client.get("/v1/libraries").send().await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn an_authenticated_caller_sees_an_empty_library_list() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;

    let response = client
        .get("/v1/libraries")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.json::<Vec<Library>>().is_empty());
}

// ─── Library mutations: admin-gated ─────────────────────────────────────────

#[tokio::test]
async fn creating_a_library_as_a_regular_user_is_403() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;

    let response = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_admin_creates_a_library_and_it_is_then_listed() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let response = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let created: Library = response.json();
    assert_eq!(created.name, "Movies");

    let listed = client
        .get("/v1/libraries")
        .cookie("beam_session", &token)
        .send()
        .await
        .json::<Vec<Library>>();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);
}

#[tokio::test]
async fn scanning_a_library_as_an_admin_returns_the_added_count() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let created: Library = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await
        .json();

    let response = client
        .post(&format!("/v1/admin/libraries/{}/scan", created.id))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    // The mocked indexer scans trivially, so this verifies wiring/auth/response
    // shape rather than scan semantics (covered where the indexer is).
    assert_eq!(response.json::<ScanLibraryResponse>().added, 0);
}

#[tokio::test]
async fn deleting_a_library_returns_204_then_404_on_repeat() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let created: Library = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await
        .json();

    let path = format!("/v1/admin/libraries/{}", created.id);

    assert_eq!(
        client
            .delete(&path)
            .cookie("beam_session", &token)
            .send()
            .await
            .status(),
        StatusCode::NO_CONTENT
    );

    assert_eq!(
        client
            .delete(&path)
            .cookie("beam_session", &token)
            .send()
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
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
        .log(
            beam_domain::models::AdminLogLevel::Info,
            beam_domain::models::AdminLogCategory::System,
            "server started".to_string(),
            None,
        )
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
    let logs: Vec<AdminLogEntryDto> = response.json();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].message, "server started");
}

#[tokio::test]
async fn the_admin_log_count_without_a_session_is_401() {
    let fixture = make_test_state();
    let client = build_client(&fixture);

    assert_eq!(
        client.get("/v1/admin/logs/count").send().await.status(),
        StatusCode::UNAUTHORIZED
    );
}

// ─── Admin events: snapshot and live stream ─────────────────────────────────

/// The snapshot endpoint returns the *newest* events, bounded by `limit`.
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
        .get("/v1/admin/events?limit=2")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let events: Vec<Value> = response.json();
    let messages: Vec<&str> = events
        .iter()
        .map(|event| event["message"].as_str().expect("a message string"))
        .collect();
    assert_eq!(messages, ["middle", "newest"]);
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
                Some("lib-1".to_string()),
                Some("Movies".to_string()),
            ),
            AdminEvent::warning(EventCategory::System, "disk nearly full", None, None),
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
    assert_eq!(events.len(), 2, "one record per broadcast event");

    let first: Value = events[0].json();
    assert_eq!(first["message"], "scan started");
    assert_eq!(first["level"], "info");
    assert_eq!(first["category"], "library_scan");
    assert_eq!(first["library_name"], "Movies");

    let second: Value = events[1].json();
    assert_eq!(second["message"], "disk nearly full");
    assert_eq!(second["level"], "warning");
    assert_eq!(second["category"], "system");
}

// ─── Refresh metadata ────────────────────────────────────────────────────────

#[tokio::test]
async fn refreshing_media_metadata_as_a_regular_user_is_403() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;

    let response = client
        .post("/v1/admin/media/some-id/refresh")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn refreshing_media_metadata_as_an_admin_is_204() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let response = client
        .post("/v1/admin/media/some-id/refresh")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

// ─── Admin users & system status (issue #85) ─────────────────────────────────

/// Seeds an additional non-admin user (no session) and returns it.
async fn seed_plain_user(fixture: &TestFixture, subject: &str, name: &str) -> User {
    fixture
        .user_repo
        .create(CreateUser {
            oidc_issuer: "https://test.example".to_string(),
            oidc_subject: subject.to_string(),
            email: None,
            display_name: name.to_string(),
            avatar_url: None,
            is_admin: false,
        })
        .await
        .expect("seed user should succeed")
}

/// Creates a live session for `user_id` directly in the fixture's store.
async fn seed_session_for(fixture: &TestFixture, user_id: &str) {
    fixture
        .session_store
        .create(
            &SessionData {
                user_id: user_id.to_string(),
                device_hash: "target-device".to_string(),
                ip: "127.0.0.1".to_string(),
                created_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
            },
            86400,
            86400,
        )
        .await
        .expect("seed session should succeed");
}

#[tokio::test]
async fn the_admin_user_and_status_endpoints_are_closed_to_a_regular_user() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;
    let target = seed_plain_user(&fixture, "target-subj", "Target").await;

    assert_eq!(
        client
            .get("/v1/admin/users")
            .cookie("beam_session", &token)
            .send()
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    assert_eq!(
        client
            .patch(&format!("/v1/admin/users/{}", target.id))
            .cookie("beam_session", &token)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send()
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    assert_eq!(
        client
            .get("/v1/admin/status")
            .cookie("beam_session", &token)
            .send()
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn listing_admin_users_without_a_session_is_401() {
    let fixture = make_test_state();
    let client = build_client(&fixture);

    assert_eq!(
        client.get("/v1/admin/users").send().await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_admin_user_list_paginates_and_reports_the_total() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await; // user 1 (admin)
    seed_plain_user(&fixture, "s2", "Alice").await;
    seed_plain_user(&fixture, "s3", "Bob").await;
    seed_plain_user(&fixture, "s4", "Carol").await;

    // Default limit returns everyone, all enabled, exactly one admin.
    let response = client
        .get("/v1/admin/users")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: AdminUserListResponse = response.json();
    assert_eq!(body.total, 4);
    assert_eq!(body.items.len(), 4);
    assert!(body.items.iter().all(|u| !u.disabled));
    assert_eq!(body.items.iter().filter(|u| u.is_admin).count(), 1);

    // Two pages of two cover all four users exactly once, and `total` stays the
    // full count on every page.
    let page1: AdminUserListResponse = client
        .get("/v1/admin/users?limit=2&offset=0")
        .cookie("beam_session", &token)
        .send()
        .await
        .json();
    let page2: AdminUserListResponse = client
        .get("/v1/admin/users?limit=2&offset=2")
        .cookie("beam_session", &token)
        .send()
        .await
        .json();
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page1.total, 4);
    assert_eq!(page2.total, 4);
    let mut ids: Vec<String> = page1
        .items
        .iter()
        .chain(page2.items.iter())
        .map(|u| u.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "pages must cover every user exactly once");

    // limit is clamped to at least 1.
    let clamped: AdminUserListResponse = client
        .get("/v1/admin/users?limit=0")
        .cookie("beam_session", &token)
        .send()
        .await
        .json();
    assert_eq!(clamped.items.len(), 1);
}

#[tokio::test]
async fn disabling_a_user_revokes_their_sessions_and_re_enabling_flips_the_flag() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let admin_token = seed_user_session(&fixture, true).await;

    let target = seed_plain_user(&fixture, "target-subj", "Target").await;
    seed_session_for(&fixture, &target.id.to_string()).await;
    seed_session_for(&fixture, &target.id.to_string()).await;
    assert_eq!(
        fixture
            .session_store
            .list_for_user(&target.id.to_string())
            .await
            .unwrap()
            .len(),
        2
    );

    let path = format!("/v1/admin/users/{}", target.id);

    assert_eq!(
        client
            .patch(&path)
            .cookie("beam_session", &admin_token)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send()
            .await
            .status(),
        StatusCode::NO_CONTENT
    );

    let stored = fixture
        .user_repo
        .find_by_id(target.id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.disabled);
    assert_eq!(
        fixture
            .session_store
            .list_for_user(&target.id.to_string())
            .await
            .unwrap()
            .len(),
        0,
        "disabling must revoke every session of the target"
    );

    // Re-enable: the flag flips back and no session reappears.
    assert_eq!(
        client
            .patch(&path)
            .cookie("beam_session", &admin_token)
            .json(&UpdateAdminUserRequest { disabled: false })
            .send()
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let stored = fixture
        .user_repo
        .find_by_id(target.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!stored.disabled);
    assert_eq!(
        fixture
            .session_store
            .list_for_user(&target.id.to_string())
            .await
            .unwrap()
            .len(),
        0,
        "re-enabling must not mint sessions"
    );
}

#[tokio::test]
async fn an_admin_cannot_disable_their_own_account() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let admin = fixture
        .user_repo
        .find_by_oidc_identity("https://test.example", "admin-subj")
        .await
        .unwrap()
        .expect("admin was seeded");

    let response = client
        .patch(&format!("/v1/admin/users/{}", admin.id))
        .cookie("beam_session", &token)
        .json(&UpdateAdminUserRequest { disabled: true })
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let stored = fixture
        .user_repo
        .find_by_id(admin.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!stored.disabled, "self-disable must not change the account");
}

#[tokio::test]
async fn patching_an_unknown_user_is_404_and_an_unparseable_id_is_400() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    assert_eq!(
        client
            .patch(&format!("/v1/admin/users/{}", uuid::Uuid::new_v4()))
            .cookie("beam_session", &token)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send()
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        client
            .patch("/v1/admin/users/not-a-uuid")
            .cookie("beam_session", &token)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send()
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn the_status_endpoint_reports_counts_queue_state_and_recent_scans() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await; // user 1 (admin)
    seed_plain_user(&fixture, "s2", "Alice").await; // user 2

    // One library created through the API, one file indexed into it.
    let create_response = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let library: Library = create_response.json();

    fixture
        .file_repo
        .create(CreateMediaFile {
            library_id: uuid::Uuid::parse_str(&library.id).unwrap(),
            path: PathBuf::from("/videos/movies/a.mkv"),
            hash: 1,
            size_bytes: 10,
            mtime: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
        })
        .await
        .unwrap();

    // Enrichment queue: two rows, one of which then fails terminally.
    fixture
        .enrichment_repo
        .ensure_pending(EnrichmentTargetId::Movie(uuid::Uuid::new_v4()))
        .await
        .unwrap();
    fixture
        .enrichment_repo
        .ensure_pending(EnrichmentTargetId::Movie(uuid::Uuid::new_v4()))
        .await
        .unwrap();
    let due = fixture
        .enrichment_repo
        .fetch_due(chrono::Utc::now(), 10)
        .await
        .unwrap();
    fixture
        .enrichment_repo
        .mark_failed(due[0].id, "provider exploded", chrono::Utc::now())
        .await
        .unwrap();

    // Two scan log entries plus one unrelated system entry that must be
    // filtered out of `recent_scans`.
    for message in ["scan one", "scan two"] {
        fixture
            .state
            .services
            .admin_log
            .log(
                beam_domain::models::AdminLogLevel::Info,
                beam_domain::models::AdminLogCategory::LibraryScan,
                message.to_string(),
                None,
            )
            .await
            .unwrap();
    }
    fixture
        .state
        .services
        .admin_log
        .log(
            beam_domain::models::AdminLogLevel::Info,
            beam_domain::models::AdminLogCategory::System,
            "server started".to_string(),
            None,
        )
        .await
        .unwrap();

    let response = client
        .get("/v1/admin/status")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: AdminStatusResponse = response.json();

    // uptime_secs deserialized as u64 (its presence is shape-verified);
    // version is the crate's own.
    assert_eq!(body.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(body.counts.users, 2);
    assert_eq!(body.counts.libraries, 1);
    assert_eq!(body.counts.files, 1);
    assert_eq!(body.enrichment.pending, 1);
    assert_eq!(body.enrichment.failed, 1);
    assert_eq!(body.enrichment.enriched, 0);
    assert_eq!(body.enrichment.unmatched, 0);

    let messages: Vec<&str> = body
        .recent_scans
        .iter()
        .map(|scan| scan.message.as_str())
        .collect();
    assert_eq!(body.recent_scans.len(), 2);
    assert!(messages.contains(&"scan one"));
    assert!(messages.contains(&"scan two"));
    assert!(
        !messages.contains(&"server started"),
        "non-scan categories must be filtered out"
    );
}
