//! Subcutaneous tests for `/v1/files/{file_id}/trickplay/{sheet}` (issue
//! #265).
//!
//! Real router, real handlers, the real `TrickplayService` over a real
//! `TempDir` data directory; only the index below the repository traits is a
//! double. What these assert is what a player receives: the sheet's bytes,
//! their type and validator, and which problem a miss names.

use std::path::PathBuf;
use std::sync::Arc;

use beam_auth::utils::session_store::SessionData;
use beam_domain::models::trickplay::{TrickplayLayout, TrickplayRecord};
use beam_domain::models::{FileStatus, MediaFile, MediaFileContent};
use beam_domain::repositories::TrickplayRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::trickplay::in_memory::InMemoryTrickplayRepository;
use beam_domain::test_utils::media_file;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use tempfile::TempDir;
use uuid::Uuid;

use crate::routes::test_support::make_app_state;
use crate::routes::trickplay::{get_trickplay_sheet, head_trickplay_sheet};
use crate::services::trickplay::{TrickplayDependencies, TrickplayService};
use crate::state::{AppServices, AppState};

const SHEET: &[u8] = b"\xff\xd8\xff\xe0 a stand-in sheet";

struct Fixture {
    state: AppState,
    files: Arc<InMemoryFileRepository>,
    records: Arc<InMemoryTrickplayRepository>,
    root: PathBuf,
    _dir: TempDir,
}

async fn fixture() -> Fixture {
    let base = make_app_state();
    let files = Arc::new(InMemoryFileRepository::default());
    let records = Arc::new(InMemoryTrickplayRepository::default());
    let dir = TempDir::new().expect("a temp dir");
    let root = dir.path().join("trickplay");
    let trickplay = Arc::new(TrickplayService::new(
        root.clone(),
        TrickplayDependencies {
            libraries: Arc::new(InMemoryLibraryRepository::default()),
            files: files.clone(),
            records: records.clone(),
            generator: Arc::new(beam_index::services::trickplay::MockTrickplayGenerator::new()),
            clock: Arc::new(beam_domain::services::RealClock),
        },
    ));

    let services = AppServices {
        hash: base.services.hash.clone(),
        access: base.services.access.clone(),
        library: base.services.library.clone(),
        metadata: base.services.metadata.clone(),
        subtitles: base.services.subtitles.clone(),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        language_preferences: base.services.language_preferences.clone(),
        playback: base.services.playback.clone(),
        genre_repo: base.services.genre_repo.clone(),
        library_repo: base.services.library_repo.clone(),
        file_repo: files.clone(),
        enrichment_repo: base.services.enrichment_repo.clone(),
        enrichment_control: base.services.enrichment_control.clone(),
        people: base.services.people.clone(),
        lists: base.services.lists.clone(),
        collections: base.services.collections.clone(),
        movie_repo: base.services.movie_repo.clone(),
        show_repo: base.services.show_repo.clone(),
        artwork: base.services.artwork.clone(),
        session_store: base.services.session_store.clone(),
        oidc_client: base.services.oidc_client.clone(),
        pending_auth_store: base.services.pending_auth_store.clone(),
        device_auth_store: base.services.device_auth_store.clone(),
        oidc_config: base.services.oidc_config.clone(),
        watch_status: Arc::new(beam_index::services::watch_status::WatchStatus::new()),
        telemetry: crate::routes::test_support::idle_library_report(),
        playback_telemetry: crate::routes::test_support::idle_playback_telemetry(),
        trickplay,
    };

    Fixture {
        state: AppState::new(base.config.clone(), services, base.probe.clone(), None),
        files,
        records,
        root,
        _dir: dir,
    }
}

impl Fixture {
    fn client(&self) -> TestClient<AppState> {
        let service = Router::new()
            .nest(
                "/v1",
                Router::new().mount(kynos::routes![get_trickplay_sheet, head_trickplay_sheet]),
            )
            .build(self.state.clone())
            .expect("the trickplay router describes itself");
        TestClient::new(service)
    }

    async fn session(&self) -> String {
        self.state
            .services
            .session_store
            .create(
                &SessionData {
                    user_id: Uuid::new_v4().to_string(),
                    device_hash: "test-device".to_owned(),
                    ip: "127.0.0.1".to_owned(),
                    created_at: chrono::Utc::now().timestamp(),
                    last_active: chrono::Utc::now().timestamp(),
                },
                86_400,
                86_400,
            )
            .await
            .expect("the in-memory session store issues a session")
    }

    /// A present video file row hashed `hash`.
    fn video(&self, hash: u64) -> MediaFile {
        let file = MediaFile {
            hash,
            duration: Some(std::time::Duration::from_secs(7_200)),
            content: Some(MediaFileContent::movie(Uuid::new_v4())),
            status: FileStatus::Known,
            ..media_file(Uuid::new_v4(), "/videos/Movie.mkv")
        };
        self.files
            .files
            .lock()
            .unwrap()
            .insert(file.id, file.clone());
        file
    }

    /// Record `sheets` full sheets of `file`, made from content hashed
    /// `made_from`, and write each one where the worker would have.
    async fn sheets(&self, file: &MediaFile, made_from: u64, sheets: u32) {
        let dir = self
            .root
            .join(file.id.to_string())
            .join(format!("{made_from:016x}"));
        std::fs::create_dir_all(&dir).unwrap();
        for sheet in 0..sheets {
            std::fs::write(dir.join(format!("{sheet}.jpg")), SHEET).unwrap();
        }
        self.records
            .upsert(TrickplayRecord {
                file_id: file.id,
                file_hash: made_from,
                layout: Some(TrickplayLayout {
                    interval_ms: 10_000,
                    tile_width: 320,
                    tile_height: 180,
                    columns: 10,
                    rows: 10,
                    tile_count: sheets * 100,
                }),
                generated_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
    }
}

fn url(file_id: Uuid, sheet: u32) -> String {
    format!("/v1/files/{file_id}/trickplay/{sheet}")
}

fn problem_type(body: &[u8]) -> String {
    let problem: serde_json::Value = serde_json::from_slice(body).expect("a problem document");
    problem["type"].as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn a_sheet_is_served_as_a_jpeg_with_a_validator_a_revalidation_matches() {
    let f = fixture().await;
    let file = f.video(0x2a);
    f.sheets(&file, 0x2a, 2).await;
    let token = f.session().await;

    let response = f
        .client()
        .get(&url(file.id, 1))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.header("content-type"), Some("image/jpeg"));
    assert_eq!(response.bytes().as_ref(), SHEET);
    let etag = response.header("etag").expect("a validator").to_owned();
    assert_eq!(etag, "\"000000000000002a-1-g1\"");

    let again = f
        .client()
        .get(&url(file.id, 1))
        .cookie("beam_session", &token)
        .header("if-none-match", &etag)
        .send()
        .await;
    assert_eq!(again.status(), StatusCode::NOT_MODIFIED);

    let head = f
        .client()
        .head(&url(file.id, 0))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.header("content-type"), Some("image/jpeg"));
    assert!(head.bytes().is_empty());
}

#[tokio::test]
async fn a_sheet_the_file_does_not_have_is_trickplay_not_found() {
    let f = fixture().await;
    let token = f.session().await;
    // Past the last sheet.
    let made = f.video(1);
    f.sheets(&made, 1, 1).await;
    // Made from content the file no longer has.
    let changed = f.video(3);
    f.sheets(&changed, 2, 1).await;
    // Never made.
    let unmade = f.video(4);
    // Recorded, but gone from disk.
    let cleared = f.video(5);
    f.sheets(&cleared, 5, 1).await;
    std::fs::remove_dir_all(f.root.join(cleared.id.to_string())).unwrap();

    for (file_id, sheet) in [
        (made.id, 1),
        (changed.id, 0),
        (unmade.id, 0),
        (cleared.id, 0),
    ] {
        let response = f
            .client()
            .get(&url(file_id, sheet))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{file_id}/{sheet}"
        );
        assert_eq!(
            problem_type(response.bytes().as_ref()),
            "https://beam.justinchung.net/reference/errors/#trickplay-not-found",
            "{file_id}/{sheet}"
        );
    }
}

#[tokio::test]
async fn the_sheets_of_an_unknown_file_are_file_not_found() {
    let f = fixture().await;
    let token = f.session().await;

    let response = f
        .client()
        .get(&url(Uuid::new_v4(), 0))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        problem_type(response.bytes().as_ref()),
        "https://beam.justinchung.net/reference/errors/#file-not-found"
    );
}

/// NFR-205: a trickplay record store that cannot read is a 500, never the
/// sheet-not-found that tells a player the file simply has no previews.
#[tokio::test]
async fn a_trickplay_record_store_failure_is_a_500_never_sheet_not_found() {
    use beam_domain::models::file::CreateMediaFile;
    use beam_domain::repositories::FileRepository;
    use beam_domain::repositories::trickplay::MockTrickplayRepository;

    use crate::routes::test_support::{outage, signed_in, state_with};

    let files = Arc::new(InMemoryFileRepository::default());
    let file = files
        .create(CreateMediaFile {
            duration: Some(std::time::Duration::from_secs(7_200)),
            content: Some(MediaFileContent::movie(Uuid::new_v4())),
            status: FileStatus::Known,
            ..beam_domain::test_utils::create_media_file(Uuid::new_v4(), "/videos/Movie.mkv")
        })
        .await
        .expect("a file row");
    let mut records = MockTrickplayRepository::new();
    records.expect_find().returning(|_| Err(outage()));
    let trickplay = Arc::new(TrickplayService::new(
        PathBuf::from("/nonexistent/beam-test/trickplay"),
        TrickplayDependencies {
            libraries: Arc::new(InMemoryLibraryRepository::default()),
            files: files.clone(),
            records: Arc::new(records),
            generator: Arc::new(beam_index::services::trickplay::MockTrickplayGenerator::new()),
            clock: Arc::new(beam_domain::services::RealClock),
        },
    ));
    let state = state_with(|services| {
        services.file_repo = files;
        services.trickplay = trickplay;
    });
    let token = signed_in(&state).await;
    let client = TestClient::new(
        Router::new()
            .nest(
                "/v1",
                Router::new().mount(kynos::routes![get_trickplay_sheet, head_trickplay_sheet]),
            )
            .build(state)
            .expect("the trickplay router describes itself"),
    );

    for response in [
        client
            .get(&url(file.id, 0))
            .cookie("beam_session", &token)
            .send()
            .await,
        client
            .head(&url(file.id, 0))
            .cookie("beam_session", &token)
            .send()
            .await,
    ] {
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}

#[tokio::test]
async fn a_sheet_needs_a_session() {
    let f = fixture().await;
    let file = f.video(1);
    f.sheets(&file, 1, 1).await;

    let response = f.client().get(&url(file.id, 0)).send().await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = f.client().head(&url(file.id, 0)).send().await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
