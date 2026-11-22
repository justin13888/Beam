//! `/v1/admin/files/{file_id}/markers` through the served router (issue
//! #332): real handlers over the real `MarkerService`, with in-memory stores
//! below the trait line. Each test asserts the response and what it left in
//! the store.

use std::sync::Arc;
use std::time::Duration;

use beam_auth::utils::models::CreateUser;
use beam_auth::utils::session_store::SessionData;
use beam_domain::models::chapter::{
    Chapter, MarkerKind, PlaybackMarker, StoredMarker, StoredMarkerSource,
};
use beam_domain::models::{FileStatus, MediaFile, MediaFileContent};
use beam_domain::repositories::chapter::in_memory::InMemoryChapterRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::markers::MockMarkerRepository;
use beam_domain::repositories::markers::in_memory::InMemoryMarkerRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::{ChapterRepository, MarkerRepository};
use beam_domain::test_utils::media_file;
use chrono::Utc;
use kynos::http::StatusCode;
use kynos::test::TestClient;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::models::FileMarkerList;
use crate::routes::test_support::{outage, served_client};
use crate::services::markers::{MarkerDependencies, MarkerService};
use crate::state::{AppServices, AppState};

const VALIDATION_FAILED: &str = "https://beam.justinchung.net/reference/errors/#validation-failed";
const FILE_NOT_FOUND: &str = "https://beam.justinchung.net/reference/errors/#file-not-found";

struct Fixture {
    client: TestClient<AppState>,
    state: AppState,
    admin: String,
    markers: Arc<InMemoryMarkerRepository>,
    /// A 22-minute episode file whose chapters name its intro.
    file: MediaFile,
}

fn service(
    files: Arc<InMemoryFileRepository>,
    chapters: Arc<InMemoryChapterRepository>,
    markers: Arc<dyn MarkerRepository>,
) -> Arc<MarkerService> {
    Arc::new(MarkerService::new(MarkerDependencies {
        libraries: Arc::new(InMemoryLibraryRepository::default()),
        files,
        shows: Arc::new(InMemoryShowRepository::default()),
        chapters,
        markers,
        fingerprinter: Arc::new(beam_index::services::FakeAudioFingerprinter::default()),
        clock: Arc::new(beam_domain::services::RealClock),
    }))
}

fn state_with_markers(markers: Arc<MarkerService>) -> AppState {
    let base = crate::routes::test_support::make_app_state();
    let services = AppServices {
        markers,
        ..base.services.clone()
    };
    AppState::new(base.config.clone(), services, base.probe.clone(), None)
}

async fn fixture() -> Fixture {
    let files = Arc::new(InMemoryFileRepository::default());
    let chapters = Arc::new(InMemoryChapterRepository::default());
    let markers = Arc::new(InMemoryMarkerRepository::default());
    let file = MediaFile {
        duration: Some(Duration::from_secs(22 * 60)),
        content: Some(MediaFileContent::episode(Uuid::new_v4())),
        status: FileStatus::Known,
        ..media_file(Uuid::new_v4(), "/shows/Show/S01E01.mkv")
    };
    files.files.lock().unwrap().insert(file.id, file.clone());
    chapters
        .replace_for_file(
            file.id,
            vec![
                Chapter {
                    index: 0,
                    start_ms: 0,
                    end_ms: 90_000,
                    title: Some("Opening".into()),
                },
                Chapter {
                    index: 1,
                    start_ms: 90_000,
                    end_ms: 1_320_000,
                    title: None,
                },
            ],
        )
        .await
        .unwrap();
    let state = state_with_markers(service(files, chapters, markers.clone()));
    let admin = session(&state, true).await;
    Fixture {
        client: served_client(state.clone()),
        state,
        admin,
        markers,
        file,
    }
}

/// Seeds a user and a session and returns the `beam_session` cookie value.
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
            86_400,
            86_400,
        )
        .await
        .unwrap()
}

impl Fixture {
    fn path(&self, kind: Option<&str>) -> String {
        match kind {
            Some(kind) => format!("/v1/admin/files/{}/markers/{kind}", self.file.id),
            None => format!("/v1/admin/files/{}/markers", self.file.id),
        }
    }

    async fn put(&self, kind: &str, body: Value) -> kynos::test::TestResponse {
        self.client
            .put(&self.path(Some(kind)))
            .cookie("beam_session", &self.admin)
            .json(&body)
            .send()
            .await
    }
}

#[tokio::test]
async fn an_administrator_sets_corrects_and_clears_a_files_markers() {
    let f = fixture().await;

    let listed: FileMarkerList = f
        .client
        .get(&f.path(None))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(listed.file_id, f.file.id);
    assert_eq!(
        serde_json::to_value(&listed.items).unwrap(),
        json!([{"kind": "intro", "start_ms": 0, "end_ms": 90_000, "origin": "chapter"}])
    );

    // Set the credits, then correct the intro the chapters got wrong.
    f.put(
        "credits",
        json!({"start_ms": 1_250_000, "end_ms": 1_320_000}),
    )
    .await
    .assert_status(StatusCode::OK);
    let corrected: Value = f
        .put("intro", json!({"start_ms": 30_000, "end_ms": 110_000}))
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(
        corrected["items"],
        json!([
            {"kind": "intro", "start_ms": 30_000, "end_ms": 110_000, "origin": "admin"},
            {"kind": "credits", "start_ms": 1_250_000, "end_ms": 1_320_000, "origin": "admin"},
        ])
    );
    let admin = |kind, start_ms, end_ms| StoredMarker {
        marker: PlaybackMarker {
            kind,
            start_ms,
            end_ms,
        },
        source: StoredMarkerSource::Admin,
    };
    assert_eq!(
        f.markers.find_by_file_id(f.file.id).await.unwrap(),
        vec![
            admin(MarkerKind::Intro, 30_000, 110_000),
            admin(MarkerKind::Credits, 1_250_000, 1_320_000),
        ]
    );

    // Clearing the intro hands it back to the chapters; clearing a kind
    // nobody set changes nothing.
    let cleared: Value = f
        .client
        .delete(&f.path(Some("intro")))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(
        cleared["items"],
        json!([
            {"kind": "intro", "start_ms": 0, "end_ms": 90_000, "origin": "chapter"},
            {"kind": "credits", "start_ms": 1_250_000, "end_ms": 1_320_000, "origin": "admin"},
        ])
    );
    f.client
        .delete(&f.path(Some("recap")))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::OK);
    assert_eq!(
        f.markers.find_by_file_id(f.file.id).await.unwrap(),
        vec![admin(MarkerKind::Credits, 1_250_000, 1_320_000)]
    );

    // NFR-403: each change is recorded, naming the administrator, the file
    // and the kind.
    let admin_id = f
        .state
        .services
        .user_repo
        .find_by_oidc_identity("https://test.example", "subject-true")
        .await
        .unwrap()
        .expect("the admin was seeded")
        .id
        .to_string();
    let file_id = f.file.id.to_string();
    assert_eq!(
        change_records(&f).await,
        vec![
            (
                format!("Set the credits marker of file {file_id}"),
                json!({"file_id": file_id, "kind": "credits", "start_ms": 1_250_000,
                       "end_ms": 1_320_000, "admin_user_id": admin_id}),
            ),
            (
                format!("Set the intro marker of file {file_id}"),
                json!({"file_id": file_id, "kind": "intro", "start_ms": 30_000,
                       "end_ms": 110_000, "admin_user_id": admin_id}),
            ),
            (
                format!("Cleared the intro marker of file {file_id}"),
                json!({"file_id": file_id, "kind": "intro", "admin_user_id": admin_id}),
            ),
            (
                format!("Cleared the recap marker of file {file_id}"),
                json!({"file_id": file_id, "kind": "recap", "admin_user_id": admin_id}),
            ),
        ]
    );
}

/// Every `System` entry in the fixture's admin log, oldest first, as its
/// message and details.
async fn change_records(f: &Fixture) -> Vec<(String, Value)> {
    let mut entries = f
        .state
        .services
        .admin_log
        .get_logs_by_category(beam_domain::models::AdminLogCategory::System, 100, 0)
        .await
        .unwrap();
    entries.reverse();
    entries
        .into_iter()
        .map(|entry| (entry.message, entry.details.unwrap_or(Value::Null)))
        .collect()
}

#[tokio::test]
async fn a_span_that_does_not_end_after_it_starts_or_ends_past_the_file_is_refused() {
    let f = fixture().await;
    for body in [
        json!({"start_ms": 50_000, "end_ms": 50_000}),
        json!({"start_ms": 60_000, "end_ms": 50_000}),
        // The file is 1,320,000 ms long.
        json!({"start_ms": 1_300_000, "end_ms": 1_320_001}),
    ] {
        let problem: Value = f
            .put("intro", body.clone())
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
            .json();
        assert_eq!(problem["type"], VALIDATION_FAILED, "{body}");
        assert_eq!(problem["errors"][0]["pointer"], "/end_ms", "{body}");
    }
    assert!(
        change_records(&f).await.is_empty(),
        "a refused span is no change, and is not recorded"
    );
    f.put("intro", json!({"start_ms": 1_300_000, "end_ms": 1_320_000}))
        .await
        .assert_status(StatusCode::OK);
    assert_eq!(f.markers.find_by_file_id(f.file.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_file_nobody_indexed_is_not_found_and_an_unknown_kind_is_a_bad_request() {
    let f = fixture().await;
    let unknown = Uuid::new_v4();
    for (method, path) in [
        ("GET", format!("/v1/admin/files/{unknown}/markers")),
        ("PUT", format!("/v1/admin/files/{unknown}/markers/intro")),
        ("DELETE", format!("/v1/admin/files/{unknown}/markers/intro")),
    ] {
        let request = match method {
            "GET" => f.client.get(&path),
            "PUT" => f
                .client
                .put(&path)
                .json(&json!({"start_ms": 0, "end_ms": 1_000})),
            _ => f.client.delete(&path),
        };
        let problem: Value = request
            .cookie("beam_session", &f.admin)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND)
            .json();
        assert_eq!(problem["type"], FILE_NOT_FOUND, "{method} {path}");
    }
    assert!(f.markers.find_by_file_id(unknown).await.unwrap().is_empty());
    assert!(change_records(&f).await.is_empty());

    f.put("opening", json!({"start_ms": 0, "end_ms": 1_000}))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn every_marker_route_is_for_administrators_only() {
    let f = fixture().await;
    let regular = session(&f.state, false).await;
    for (method, path) in [
        ("GET", f.path(None)),
        ("PUT", f.path(Some("intro"))),
        ("DELETE", f.path(Some("intro"))),
    ] {
        let request = |cookie: Option<&str>| {
            let builder = match method {
                "GET" => f.client.get(&path),
                "PUT" => f
                    .client
                    .put(&path)
                    .json(&json!({"start_ms": 0, "end_ms": 1_000})),
                _ => f.client.delete(&path),
            };
            match cookie {
                Some(cookie) => builder.cookie("beam_session", cookie),
                None => builder,
            }
        };
        assert_eq!(
            request(None).send().await.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
        assert_eq!(
            request(Some(&regular)).send().await.status(),
            StatusCode::FORBIDDEN,
            "{method} {path}"
        );
    }
    assert!(
        f.markers
            .find_by_file_id(f.file.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(change_records(&f).await.is_empty());
}

/// A store that cannot be read is a 500 that names no cause (NFR-205).
#[tokio::test]
async fn a_store_that_fails_is_an_internal_error() {
    let files = Arc::new(InMemoryFileRepository::default());
    let file = MediaFile {
        duration: Some(Duration::from_secs(60)),
        ..media_file(Uuid::new_v4(), "/shows/S01E01.mkv")
    };
    files.files.lock().unwrap().insert(file.id, file.clone());
    let mut markers = MockMarkerRepository::new();
    markers
        .expect_find_by_file_id()
        .returning(|_| Err(outage()));
    markers.expect_set_admin().returning(|_, _| Err(outage()));
    let state = state_with_markers(service(
        files,
        Arc::new(InMemoryChapterRepository::default()),
        Arc::new(markers),
    ));
    let admin = session(&state, true).await;
    let client = served_client(state);

    let listed: Value = client
        .get(&format!("/v1/admin/files/{}/markers", file.id))
        .cookie("beam_session", &admin)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .json();
    assert!(
        !listed.to_string().contains("store unavailable"),
        "{listed}"
    );
    client
        .put(&format!("/v1/admin/files/{}/markers/intro", file.id))
        .cookie("beam_session", &admin)
        .json(&json!({"start_ms": 0, "end_ms": 1_000}))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
}

/// A kind in a path is spelled as it is in a body: the path's `Display` and
/// `FromStr` agree with the serde spelling for every kind.
#[test]
fn a_kind_is_spelled_alike_in_a_path_and_a_body() {
    use crate::models::PlaybackMarkerKind;

    for kind in PlaybackMarkerKind::ALL {
        let spelled = kind.to_string();
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(spelled));
        assert_eq!(spelled.parse::<PlaybackMarkerKind>().unwrap(), kind);
    }
    assert!("opening".parse::<PlaybackMarkerKind>().is_err());
}
