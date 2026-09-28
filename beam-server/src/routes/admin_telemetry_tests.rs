//! `GET /v1/admin/telemetry/library` (issue #93) through the served router
//! and handler, over the real report service, with a recording sink and an
//! in-memory store below the trait line.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use beam_auth::utils::models::CreateUser;
use beam_auth::utils::session_store::SessionData;
use beam_domain::models::file::{CreateMediaFile, FileStatus, MediaFileContent};
use beam_domain::models::library::CreateLibrary;
use beam_domain::models::movie::{CreateMovie, CreateMovieEntry};
use beam_domain::models::stream::{
    CreateMediaStream, StreamMetadata, StreamType, VideoStreamMetadata,
};
use beam_domain::providers::telemetry::RecordingTelemetrySink;
use beam_domain::repositories::library_shape::MockLibraryShapeRepository;
use beam_domain::repositories::library_shape::in_memory::InMemoryLibraryShapeRepository;
use beam_domain::repositories::{
    FileRepository, LibraryRepository, LibraryShapeRepository, MediaStreamRepository,
    MovieRepository,
};
use beam_domain::services::TestClock;
use beam_domain::utils::telemetry::{CountBucket, GIB};
use chrono::{TimeZone, Utc};
use kynos::http::StatusCode;
use kynos::test::TestClient;
use sea_orm::DbErr;
use serde_json::Value;
use tempfile::TempDir;

use crate::services::telemetry::LibraryReportService;
use crate::services::telemetry::scheduler::FIRST_SEND_DELAY;
use crate::state::AppState;

const COLLECTOR: &str = "https://collector.example:4318/v1/metrics?token=ingest-secret";

struct Fixture {
    client: TestClient<AppState>,
    state: AppState,
    service: Arc<LibraryReportService>,
    sink: Arc<RecordingTelemetrySink>,
    clock: Arc<TestClock>,
    _dir: TempDir,
}

/// One library, one movie with one file and one video stream, every name and
/// path in it something the report must never carry.
async fn seeded_store() -> InMemoryLibraryShapeRepository {
    let store = InMemoryLibraryShapeRepository::default();
    let library = store
        .libraries
        .create(CreateLibrary {
            name: "SECRET-NAME".to_string(),
            root_path: PathBuf::from("/m/secret-path"),
            description: Some("SECRET-DESCRIPTION".to_string()),
        })
        .await
        .unwrap();
    let movie = store
        .movies
        .find_or_create_by_identity(CreateMovie::new("SECRET-NAME", Some(1999), None))
        .await
        .unwrap();
    let entry = store
        .movies
        .find_or_create_entry(CreateMovieEntry {
            library_id: library.id,
            movie_id: movie.id,
            edition: None,
            is_primary: true,
        })
        .await
        .unwrap();
    let file = store
        .files
        .create(CreateMediaFile {
            library_id: library.id,
            path: PathBuf::from("/m/secret-path/SECRET-NAME (1999).mkv"),
            hash: 0xdead_beef,
            size_bytes: 2 * GIB,
            mtime: None,
            mime_type: Some("video/x-matroska".to_string()),
            duration: None,
            container_format: Some("matroska,webm".to_string()),
            content: Some(MediaFileContent::Movie {
                movie_entry_id: entry.id,
            }),
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
        })
        .await
        .unwrap();
    store
        .streams
        .insert_streams(vec![CreateMediaStream {
            file_id: file.id,
            index: 0,
            stream_type: StreamType::Video,
            codec: "H264".to_string(),
            metadata: StreamMetadata::Video(VideoStreamMetadata {
                width: 1920,
                height: 1080,
                frame_rate: None,
                bit_rate: None,
                color_space: None,
                color_range: None,
                hdr_format: None,
            }),
        }])
        .await
        .unwrap();
    store
}

async fn fixture(destination: Option<&str>) -> Fixture {
    fixture_over(destination, Arc::new(seeded_store().await))
}

/// A fixture whose report reads `shape_repo`.
fn fixture_over(destination: Option<&str>, shape_repo: Arc<dyn LibraryShapeRepository>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::starting_at(
        Utc.with_ymd_and_hms(2026, 9, 27, 10, 0, 0).unwrap(),
    ));
    let sink = Arc::new(RecordingTelemetrySink::new());
    let config = crate::config::ServerConfig {
        telemetry_url: destination.map(str::to_string),
        data_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    let service = Arc::new(LibraryReportService::new(
        config.library_report_config(),
        shape_repo,
        sink.clone(),
        clock.clone(),
    ));
    let state = crate::routes::test_support::make_app_state_with_telemetry(
        |c| {
            c.telemetry_url = config.telemetry_url.clone();
        },
        clock.clone(),
        Arc::new(crate::services::health::InMemoryDependencyProbe::healthy()),
        None,
        service.clone(),
    );
    let router = crate::routes::create_router()
        .build(state.clone())
        .expect("the served router describes itself");
    Fixture {
        client: TestClient::new(router),
        state,
        service,
        sink,
        clock,
        _dir: dir,
    }
}

/// Seeds a user and a session through the stores' own traits and returns the
/// `beam_session` cookie value.
async fn session(state: &AppState, is_admin: bool) -> String {
    let user = state
        .services
        .user_repo
        .create(CreateUser {
            oidc_issuer: "https://test.example".to_string(),
            oidc_subject: format!("subject-{is_admin}"),
            email: None,
            display_name: "Someone".to_string(),
            avatar_url: None,
            is_admin,
        })
        .await
        .unwrap();
    state
        .services
        .session_store
        .create(
            &SessionData {
                user_id: user.id.to_string(),
                device_hash: "test-device".to_string(),
                ip: "127.0.0.1".to_string(),
                created_at: Utc::now().timestamp(),
                last_active: Utc::now().timestamp(),
            },
            86400,
            86400,
        )
        .await
        .unwrap()
}

async fn preview(fixture: &Fixture) -> Value {
    let token = session(&fixture.state, true).await;
    let response = fixture
        .client
        .get("/v1/admin/telemetry/library")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>()
}

async fn until(label: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..100_000 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("timed out waiting for: {label}");
}

#[tokio::test]
async fn the_preview_needs_a_session() {
    let fixture = fixture(Some(COLLECTOR)).await;

    let response = fixture
        .client
        .get("/v1/admin/telemetry/library")
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_preview_is_admin_only() {
    let fixture = fixture(Some(COLLECTOR)).await;
    let token = session(&fixture.state, false).await;

    let response = fixture
        .client
        .get("/v1/admin/telemetry/library")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_preview_counts_the_library_and_names_nothing_in_it() {
    let fixture = fixture(Some(COLLECTOR)).await;

    let body = preview(&fixture).await;

    assert_eq!(body["destination_configured"], true);
    assert_eq!(
        body["destination_origin"], "https://collector.example:4318",
        "the origin only: the path and query may hold an ingest token"
    );
    assert_eq!(body["content_type"], "application/json");
    let report = &body["report"];
    // One of each, reported as the range one falls into.
    let one = CountBucket::of(1).as_str();
    let none = CountBucket::of(0).as_str();
    assert_eq!(report["libraries"], one);
    assert_eq!(report["titles"]["movies"], one);
    assert_eq!(report["titles"]["shows"], none);
    assert_eq!(report["files"]["movie"], one);
    assert_eq!(
        report["codecs"]["video"],
        serde_json::json!([{ "name": "h264", "count": one }])
    );
    assert_eq!(report["generated_on"], "2026-09-27");
    let whole = body.to_string();
    for secret in [
        "SECRET-NAME",
        "SECRET-DESCRIPTION",
        "secret-path",
        "ingest-secret",
        "1999",
    ] {
        assert!(!whole.contains(secret), "{secret} leaked into {whole}");
    }
    assert_eq!(fixture.sink.sent_count(), 0, "previewing sends nothing");
}

/// The preview is the delivery: at one instant, the payload shown is the
/// request body the collector receives.
#[tokio::test]
async fn the_previewed_payload_is_the_delivered_body() {
    let fixture = fixture(Some(COLLECTOR)).await;
    let service = fixture.service.clone();
    tokio::spawn(async move { service.run().await });
    until("the schedule to sleep", || {
        fixture.clock.waiter_count() == 1
    })
    .await;
    fixture.clock.advance(FIRST_SEND_DELAY);
    until("the first report", || fixture.sink.sent_count() == 1).await;
    until("the schedule to sleep again", || {
        fixture.clock.waiter_count() == 1
    })
    .await;

    let body = preview(&fixture).await;

    let sent = &fixture.sink.sent()[0];
    assert_eq!(sent.url, COLLECTOR);
    assert_eq!(body["payload"].as_str().unwrap().as_bytes(), sent.body);
    assert_eq!(body["last_sent_at"], "2026-09-27T11:00:00Z");
    assert_eq!(body["next_send_at"], "2026-10-04T11:00:00Z");
}

/// NFR-205: the store failing is a 500 problem, not a panic or a partial
/// report, and nothing is sent.
#[tokio::test]
async fn a_store_failure_is_an_internal_error_problem() {
    let mut store = MockLibraryShapeRepository::new();
    store
        .expect_shape()
        .times(1)
        .returning(|| Err(DbErr::Custom("connection reset".to_string())));
    let fixture = fixture_over(Some(COLLECTOR), Arc::new(store));
    let token = session(&fixture.state, true).await;

    fixture
        .client
        .get("/v1/admin/telemetry/library")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
    assert_eq!(fixture.sink.sent_count(), 0);
}

#[tokio::test]
async fn without_a_destination_the_preview_says_so_and_nothing_is_sent() {
    let fixture = fixture(None).await;
    // What `main` would do: the schedule returns at once with no collector.
    fixture.service.run().await;

    let body = preview(&fixture).await;

    assert_eq!(body["destination_configured"], false);
    assert_eq!(body["destination_origin"], Value::Null);
    assert_eq!(body["next_send_at"], Value::Null);
    assert_eq!(
        body["report"]["libraries"],
        CountBucket::of(1).as_str(),
        "the preview still works"
    );
    fixture
        .clock
        .advance(Duration::from_secs(30 * 24 * 60 * 60));
    assert_eq!(fixture.sink.sent_count(), 0);
}
