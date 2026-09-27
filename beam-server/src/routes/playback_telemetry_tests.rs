//! `POST /v1/telemetry/playback` and `GET /v1/admin/telemetry/playback`
//! (issue #143) through the served router and handlers, over the real
//! service, with in-memory stores and a test clock below the trait line.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use beam_auth::utils::models::CreateUser;
use beam_auth::utils::session_store::SessionData;
use beam_domain::models::file::{CreateMediaFile, FileStatus, MediaFileContent};
use beam_domain::models::stream::{
    AudioStreamMetadata, CreateMediaStream, StreamMetadata, StreamType, VideoStreamMetadata,
};
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::playback_telemetry::MockPlaybackTelemetryRepository;
use beam_domain::repositories::playback_telemetry::in_memory::InMemoryPlaybackTelemetryRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::{
    FileRepository, MediaStreamRepository, PlaybackTelemetryRepository,
};
use beam_domain::services::{Clock, TestClock};
use chrono::{TimeZone, Utc};
use kynos::http::StatusCode;
use kynos::test::TestClient;
use sea_orm::DbErr;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::services::playback_telemetry::{PlaybackTelemetryConfig, PlaybackTelemetryService};
use crate::state::AppState;

const INGEST: &str = "/v1/telemetry/playback";
const REPORT: &str = "/v1/admin/telemetry/playback";

struct Fixture {
    client: TestClient<AppState>,
    state: AppState,
    repo: Arc<InMemoryPlaybackTelemetryRepository>,
    files: Arc<InMemoryFileRepository>,
    streams: Arc<InMemoryMediaStreamRepository>,
    clock: Arc<TestClock>,
}

/// 23:30 UTC, so a test can cross midnight in one step.
fn late_evening() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 27, 23, 30, 0).unwrap()
}

fn fixture(enabled: bool) -> Fixture {
    fixture_over(enabled, None)
}

/// A fixture whose service counts into `repo` when given, and into its own
/// in-memory store otherwise.
fn fixture_over(enabled: bool, repo: Option<Arc<dyn PlaybackTelemetryRepository>>) -> Fixture {
    let clock = Arc::new(TestClock::starting_at(late_evening()));
    let store = Arc::new(InMemoryPlaybackTelemetryRepository::default());
    let files = Arc::new(InMemoryFileRepository::default());
    let streams = Arc::new(InMemoryMediaStreamRepository::default());
    let service = Arc::new(PlaybackTelemetryService::new(
        PlaybackTelemetryConfig {
            enabled,
            retention_days: 365,
        },
        repo.unwrap_or_else(|| store.clone()),
        files.clone(),
        streams.clone(),
        clock.clone(),
    ));
    let state = crate::routes::test_support::make_app_state_with_playback_telemetry(
        |_| {},
        clock.clone(),
        service,
    );
    let router = crate::routes::create_router()
        .build(state.clone())
        .expect("the served router describes itself");
    Fixture {
        client: TestClient::new(router),
        state,
        repo: store,
        files,
        streams,
        clock,
    }
}

/// Seeds a user and a session through the stores' own traits, returning the
/// user's id and the `beam_session` cookie value.
async fn session(state: &AppState, is_admin: bool) -> (Uuid, String) {
    let user = state
        .services
        .user_repo
        .create(CreateUser {
            oidc_issuer: "https://test.example".to_string(),
            oidc_subject: format!("subject-{}", Uuid::new_v4()),
            email: None,
            display_name: "Someone".to_string(),
            avatar_url: None,
            is_admin,
        })
        .await
        .unwrap();
    let token = state
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
        .unwrap();
    (user.id, token)
}

/// A 2160p HEVC file with two audio tracks, the second flagged default.
async fn seed_hevc_file(fixture: &Fixture) -> Uuid {
    let file = fixture
        .files
        .create(CreateMediaFile {
            library_id: Uuid::new_v4(),
            path: PathBuf::from("/m/SECRET-TITLE (2001).mkv"),
            hash: 1,
            size_bytes: 20_000_000_000,
            mtime: None,
            mime_type: Some("video/x-matroska".to_string()),
            duration: Some(Duration::from_secs(7_200)),
            container_format: Some("matroska,webm".to_string()),
            // A known file is always some title's content; which title does
            // not matter here, since telemetry never records it.
            content: Some(MediaFileContent::Movie {
                movie_entry_id: Uuid::new_v4(),
            }),
            status: FileStatus::Known,
            classifier_version: 0,
        })
        .await
        .unwrap();
    let audio = |index: u32, codec: &str, is_default: bool| CreateMediaStream {
        file_id: file.id,
        index,
        stream_type: StreamType::Audio,
        codec: codec.to_string(),
        metadata: StreamMetadata::Audio(AudioStreamMetadata {
            language: None,
            title: None,
            channels: 6,
            sample_rate: 48_000,
            channel_layout: None,
            bit_rate: None,
            is_default,
            is_forced: false,
        }),
    };
    fixture
        .streams
        .insert_streams(vec![
            CreateMediaStream {
                file_id: file.id,
                index: 0,
                stream_type: StreamType::Video,
                codec: "HEVC".to_string(),
                metadata: StreamMetadata::Video(VideoStreamMetadata {
                    width: 3840,
                    height: 2160,
                    frame_rate: None,
                    bit_rate: None,
                    color_space: None,
                    color_range: None,
                    hdr_format: None,
                }),
            },
            audio(1, "truehd", false),
            audio(2, "eac3", true),
        ])
        .await
        .unwrap();
    file.id
}

async fn post(fixture: &Fixture, token: &str, body: &Value) -> kynos::test::TestResponse {
    fixture
        .client
        .post(INGEST)
        .cookie("beam_session", token)
        .json(body)
        .send()
        .await
}

async fn report(fixture: &Fixture, query: &str) -> Value {
    let (_, token) = session(&fixture.state, true).await;
    let response = fixture
        .client
        .get(&format!("{REPORT}{query}"))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>()
}

async fn nothing_counted(fixture: &Fixture) -> bool {
    let day = late_evening().date_naive();
    fixture
        .repo
        .summarize(day - chrono::Days::new(400), day + chrono::Days::new(1))
        .await
        .unwrap()
        == Default::default()
}

#[tokio::test]
async fn reporting_needs_a_session() {
    let fixture = fixture(true);

    let response = fixture
        .client
        .post(INGEST)
        .json(&json!({ "client_kind": "android", "starts": [{ "file_id": Uuid::new_v4() }] }))
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(nothing_counted(&fixture).await);
}

#[tokio::test]
async fn a_cross_origin_report_is_refused() {
    let fixture = fixture(true);
    let file = seed_hevc_file(&fixture).await;
    let (_, token) = session(&fixture.state, false).await;

    fixture
        .client
        .post(INGEST)
        .cookie("beam_session", &token)
        .header("Origin", "https://evil.example")
        .json(&json!({ "client_kind": "android", "starts": [{ "file_id": file }] }))
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .assert_problem_type(
            "https://beam.justinchung.net/reference/errors/#cross-origin-rejected",
        );
    assert!(nothing_counted(&fixture).await);
}

/// Off by default, and a 409 tells the client to stop: nothing is counted.
#[tokio::test]
async fn while_disabled_a_report_is_a_409_and_nothing_is_counted() {
    let fixture = fixture(false);
    let file = seed_hevc_file(&fixture).await;
    let (_, token) = session(&fixture.state, false).await;

    post(
        &fixture,
        &token,
        &json!({ "client_kind": "android", "starts": [{ "file_id": file }] }),
    )
    .await
    .assert_status(StatusCode::CONFLICT)
    .assert_problem_type(
        "https://beam.justinchung.net/reference/errors/#playback-telemetry-disabled",
    );
    assert!(nothing_counted(&fixture).await);
    let body = report(&fixture, "").await;
    assert_eq!(body["enabled"], false);
}

/// The whole path: a failure is counted under the file's dimensions, and the
/// report an admin reads names neither the file nor the user who reported.
#[tokio::test]
async fn a_reported_failure_is_counted_by_codec_and_names_no_one() {
    let fixture = fixture(true);
    let file = seed_hevc_file(&fixture).await;
    let (user, token) = session(&fixture.state, false).await;

    post(
        &fixture,
        &token,
        &json!({
            "client_kind": "web_firefox",
            "start_failures": [{
                "file_id": file,
                "reason": "video_codec",
                "stage": "preflight"
            }],
            "rebuffers": [{ "file_id": file, "duration_ms": 2_500 }]
        }),
    )
    .await
    .assert_status(StatusCode::NO_CONTENT);

    let body = report(&fixture, "").await;
    assert_eq!(body["enabled"], true);
    assert_eq!(body["to"], "2026-09-27");
    assert_eq!(
        body["from"], "2026-08-29",
        "the last thirty days by default"
    );
    assert_eq!(body["totals"]["failed_count"], 1);
    assert_eq!(body["totals"]["started_count"], 0);
    assert_eq!(body["totals"]["rebuffer_count"], 1);
    assert_eq!(body["totals"]["rebuffer_total_ms"], 2_500);
    assert_eq!(
        body["start_failures"],
        json!([{
            "client_kind": "web_firefox",
            "reason": "video_codec",
            "stage": "preflight",
            "container": "matroska,webm",
            "video_codec": "hevc",
            // No index given: the default track, not the first.
            "audio_codec": "eac3",
            "height_class": "uhd",
            "count": 1
        }])
    );
    let rebuffer = &body["rebuffers"][0];
    assert_eq!(rebuffer["bitrate_class"], "from_20_to_50_mbps");
    assert_eq!(rebuffer["event_count"], 1);
    assert_eq!(
        rebuffer["histogram"][1],
        json!({ "bucket": "from_1_to_3_secs", "count": 1 })
    );
    let whole = body.to_string();
    for secret in [
        file.to_string(),
        user.to_string(),
        "SECRET-TITLE".to_string(),
    ] {
        assert!(!whole.contains(&secret), "{secret} leaked into {whole}");
    }
}

/// An audio track index picks the codec a start is counted under.
#[tokio::test]
async fn a_start_is_counted_under_the_audio_track_that_played() {
    let fixture = fixture(true);
    let file = seed_hevc_file(&fixture).await;
    let (_, token) = session(&fixture.state, false).await;

    post(
        &fixture,
        &token,
        &json!({
            "client_kind": "android_tv",
            "starts": [{ "file_id": file, "audio_track_index": 0 }]
        }),
    )
    .await
    .assert_status(StatusCode::NO_CONTENT);

    let body = report(&fixture, "").await;
    assert_eq!(body["starts"][0]["audio_codec"], "truehd");
    assert_eq!(body["starts"][0]["client_kind"], "android_tv");
}

/// An event naming a file the server cannot resolve is dropped, not refused:
/// the batch still succeeds and the rest of it is counted.
#[tokio::test]
async fn an_unknown_or_missing_file_is_dropped_and_the_rest_counted() {
    let fixture = fixture(true);
    let file = seed_hevc_file(&fixture).await;
    let gone = seed_hevc_file(&fixture).await;
    fixture
        .files
        .mark_missing(vec![gone], fixture.clock.now())
        .await
        .unwrap();
    let (_, token) = session(&fixture.state, false).await;

    post(
        &fixture,
        &token,
        &json!({
            "client_kind": "android",
            "starts": [
                { "file_id": Uuid::new_v4() },
                { "file_id": gone },
                { "file_id": file }
            ],
            "source_switches": [
                { "from_file_id": file, "to_file_id": Uuid::new_v4(), "trigger": "manual" }
            ]
        }),
    )
    .await
    .assert_status(StatusCode::NO_CONTENT);

    let body = report(&fixture, "").await;
    assert_eq!(body["totals"]["started_count"], 1);
    assert_eq!(body["totals"]["source_switch_count"], 0);
}

/// Kynos does not enforce the published `maxItems` (see the gap noted on
/// `PlaybackTelemetryBatch`), so this is the service's own bound answering:
/// a 422 naming the list that broke it, and nothing counted.
#[tokio::test]
async fn more_than_fifty_events_is_a_422_at_the_list_that_broke_it() {
    let fixture = fixture(true);
    let file = seed_hevc_file(&fixture).await;
    let (_, token) = session(&fixture.state, false).await;
    let starts: Vec<Value> = (0..51).map(|_| json!({ "file_id": file })).collect();

    let response = post(
        &fixture,
        &token,
        &json!({ "client_kind": "android", "starts": starts }),
    )
    .await;

    response
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#validation-failed");
    let body = response.json::<Value>();
    assert_eq!(body["errors"][0]["pointer"], "/starts");
    assert!(nothing_counted(&fixture).await);
}

#[tokio::test]
async fn a_zero_length_rebuffer_is_a_422_at_its_duration() {
    let fixture = fixture(true);
    let file = seed_hevc_file(&fixture).await;
    let (_, token) = session(&fixture.state, false).await;

    let response = post(
        &fixture,
        &token,
        &json!({
            "client_kind": "android",
            "starts": [{ "file_id": file }],
            "rebuffers": [{ "file_id": file, "duration_ms": 0 }]
        }),
    )
    .await;

    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    let body = response.json::<Value>();
    assert_eq!(
        body["errors"],
        json!([{
            "pointer": "/rebuffers/0/duration_ms",
            "detail": "must be from 1 to 600000, got 0"
        }])
    );
    assert!(
        nothing_counted(&fixture).await,
        "a refused batch counts nothing"
    );
}

/// Events are counted under the UTC day they arrive: one either side of
/// midnight is two days' counts.
#[tokio::test]
async fn reports_either_side_of_midnight_count_on_their_own_days() {
    let fixture = fixture(true);
    let file = seed_hevc_file(&fixture).await;
    let (_, token) = session(&fixture.state, false).await;
    let batch = json!({ "client_kind": "web_safari", "starts": [{ "file_id": file }] });

    post(&fixture, &token, &batch)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fixture.clock.advance(Duration::from_secs(60 * 60));
    post(&fixture, &token, &batch)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let first = report(&fixture, "?from=2026-09-27&to=2026-09-27").await;
    let second = report(&fixture, "?from=2026-09-28&to=2026-09-28").await;
    let both = report(&fixture, "?from=2026-09-27&to=2026-09-28").await;
    assert_eq!(first["totals"]["started_count"], 1);
    assert_eq!(second["totals"]["started_count"], 1);
    assert_eq!(both["totals"]["started_count"], 2);
    assert_eq!(
        both["starts"].as_array().unwrap().len(),
        1,
        "one row per key"
    );
}

#[tokio::test]
async fn the_report_is_admin_only() {
    let fixture = fixture(true);
    let (_, token) = session(&fixture.state, false).await;

    fixture
        .client
        .get(REPORT)
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    fixture
        .client
        .get(REPORT)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_backwards_or_overlong_range_is_a_400() {
    let fixture = fixture(true);
    let (_, token) = session(&fixture.state, true).await;

    for query in [
        "?from=2026-09-28&to=2026-09-27",
        "?from=2025-09-26&to=2026-09-27",
    ] {
        fixture
            .client
            .get(&format!("{REPORT}{query}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type(
                "https://beam.justinchung.net/reference/errors/#invalid-date-range",
            );
    }
    // The longest range allowed, 366 days, is accepted.
    let body = report(&fixture, "?from=2025-09-27&to=2026-09-27").await;
    assert_eq!(body["from"], "2025-09-27");
}

/// NFR-205: the store failing is a 500 problem on both operations, not a
/// panic or a partial answer. The batch reaches the store as one write
/// carrying every event, so the store's all-or-nothing contract covers all
/// of it: a failure counts none of the batch, and the client's retry of the
/// 500 cannot count any event twice.
#[tokio::test]
async fn a_store_failure_is_an_internal_error_problem() {
    let mut store = MockPlaybackTelemetryRepository::new();
    store
        .expect_record_batch()
        .withf(|_, events| events.len() == 3)
        .times(1)
        .returning(|_, _| Err(DbErr::Custom("connection reset".to_string())));
    store
        .expect_summarize()
        .returning(|_, _| Err(DbErr::Custom("connection reset".to_string())));
    let fixture = fixture_over(true, Some(Arc::new(store)));
    let file = seed_hevc_file(&fixture).await;
    let (_, token) = session(&fixture.state, true).await;

    post(
        &fixture,
        &token,
        &json!({
            "client_kind": "android",
            "starts": [{ "file_id": file }, { "file_id": file }],
            "rebuffers": [{ "file_id": file, "duration_ms": 2_500 }]
        }),
    )
    .await
    .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
    .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
    fixture
        .client
        .get(REPORT)
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
}
