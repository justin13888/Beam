//! Subcutaneous tests for `/v1/files/{file_id}/stream` and
//! `/v1/files/{file_id}/download`.
//!
//! Both endpoints are driven through Kynos's in-process `TestClient` over a
//! real router: real handlers, real `Served`/`ByteSource` range engine, real
//! files in a `TempDir`, and fakes only below the trait line. No Postgres, no
//! Docker, no listener.
//!
//! There is deliberately no test of a range *parser* here. Beam no longer has
//! one -- `stream.rs` serves ranges through Kynos's `Served<S, M>` over
//! [`FileByteSource`](crate::routes::stream::FileByteSource) -- and a suite
//! that re-derived RFC 9110's parsing rules would be testing a dependency
//! rather than Beam. What Beam owns is the byte source: that the octets a 206
//! encloses are the octets of the file it named. So the range cases below are
//! end-to-end, and `assert_part` checks the status, the `Content-Range` and the
//! body length together -- either half alone passes while the response is
//! wrong.

use std::path::PathBuf;
use std::sync::Arc;

use beam_auth::utils::oidc_config::OidcRuntimeConfig;
use beam_auth::utils::{
    models::CreateUser,
    oidc::NotConfiguredOidcClient,
    pending_auth_store::in_memory::InMemoryPendingAuthStore,
    repository::{UserRepository, in_memory::InMemoryUserRepository},
    session_store::{SessionData, SessionStore, in_memory::InMemorySessionStore},
};
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use tempfile::TempDir;

use crate::models::{FileContentType, FileIndexStatus, LibraryFile};
use crate::routes::stream::{download_file, head_download_file, head_stream_file, stream_file};
use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
use crate::services::hash::HashService;
use crate::services::library::{LibraryError, LibraryService};
use crate::services::metadata::{
    MediaConnection, MediaFilter, MediaSearchFilters, MediaSortField, MetadataError,
    MetadataService, PageInfo, SortOrder,
};
use crate::services::notification::InMemoryNotificationService;
use crate::services::playback::{
    ContinueWatchingItem, PlaybackError, PlaybackProgressDto, PlaybackService,
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
        unimplemented!("not called in stream route tests")
    }

    async fn get_continue_watching(
        &self,
        _user_id: uuid::Uuid,
        _limit: u32,
    ) -> Result<Vec<ContinueWatchingItem>, PlaybackError> {
        unimplemented!("not called in stream route tests")
    }

    async fn get_history(
        &self,
        _user_id: uuid::Uuid,
        _limit: u64,
        _offset: u64,
    ) -> Result<(Vec<crate::services::playback::HistoryItem>, u64), PlaybackError> {
        unimplemented!("not called in stream route tests")
    }
}

// ─── Constants ────────────────────────────────────────────────────────────

const TEST_FILE_ID: &str = "11111111-1111-1111-1111-111111111111";

// ─── Stub service implementations ─────────────────────────────────────────

#[derive(Debug)]
struct StubHashService;

#[async_trait::async_trait]
impl HashService for StubHashService {
    fn hash_sync(&self, _path: &std::path::Path) -> std::io::Result<u64> {
        unimplemented!("not called in stream route tests")
    }
    async fn hash_async(&self, _path: PathBuf) -> std::io::Result<u64> {
        unimplemented!("not called in stream route tests")
    }
}

#[derive(Debug)]
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
        unimplemented!("not called in stream route tests")
    }
}

/// Stub library service backed by a fixed list of files.
///
/// Only `get_file_by_id` is exercised by the stream routes; all other methods
/// are left `unimplemented!`.
#[derive(Debug, Clone)]
struct StubLibraryService {
    files: Vec<LibraryFile>,
}

impl StubLibraryService {
    fn new(files: Vec<LibraryFile>) -> Self {
        Self { files }
    }
}

#[async_trait::async_trait]
impl LibraryService for StubLibraryService {
    async fn get_libraries(
        &self,
        _user_id: String,
    ) -> Result<Vec<crate::models::Library>, LibraryError> {
        unimplemented!("not called in stream route tests")
    }
    async fn get_library_by_id(
        &self,
        _library_id: String,
    ) -> Result<Option<crate::models::Library>, LibraryError> {
        unimplemented!("not called in stream route tests")
    }
    async fn get_library_files(
        &self,
        _library_id: String,
    ) -> Result<Vec<LibraryFile>, LibraryError> {
        unimplemented!("not called in stream route tests")
    }
    async fn create_library(
        &self,
        _name: String,
        _root_path: String,
    ) -> Result<crate::models::Library, LibraryError> {
        unimplemented!("not called in stream route tests")
    }
    async fn scan_library(&self, _library_id: String) -> Result<u32, LibraryError> {
        unimplemented!("not called in stream route tests")
    }
    async fn delete_library(&self, _library_id: String) -> Result<bool, LibraryError> {
        unimplemented!("not called in stream route tests")
    }
    async fn get_file_by_id(&self, file_id: String) -> Result<Option<LibraryFile>, LibraryError> {
        Ok(self.files.iter().find(|f| f.id == file_id).cloned())
    }
}

// ─── Test fixture ─────────────────────────────────────────────────────────

struct TestFixture {
    state: AppState,
    session_store: Arc<InMemorySessionStore>,
    user_repo: Arc<InMemoryUserRepository>,
}

fn make_test_state(files: Vec<LibraryFile>) -> TestFixture {
    let session_store = Arc::new(InMemorySessionStore::default());
    let user_repo = Arc::new(InMemoryUserRepository::default());

    let notification = Arc::new(InMemoryNotificationService::new());
    let admin_log: Arc<dyn AdminLogService> = Arc::new(LocalAdminLogService::new(Arc::new(
        InMemoryAdminLogRepository::default(),
    )));

    let services = AppServices {
        hash: Arc::new(StubHashService),
        library: Arc::new(StubLibraryService::new(files)),
        metadata: Arc::new(StubMetadataService),
        notification,
        admin_log,
        user_repo: user_repo.clone(),
        playback: Arc::new(StubPlaybackService),
        genre_repo: Arc::new(
            beam_domain::repositories::genre::in_memory::InMemoryGenreRepository::default(),
        ),
        library_repo: Arc::new(
            beam_domain::repositories::library::in_memory::InMemoryLibraryRepository::default(),
        ),
        file_repo: Arc::new(
            beam_domain::repositories::file::in_memory::InMemoryFileRepository::default(),
        ),
        enrichment_repo: Arc::new(
            beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository::default(),
        ),
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
    }
}

/// Seeds a user + session directly (bypassing the OIDC login flow, which
/// isn't under test here) and returns the `beam_session` cookie value.
async fn seed_session_token(fixture: &TestFixture) -> String {
    let user = fixture
        .user_repo
        .create(CreateUser {
            oidc_issuer: "https://test.example".to_string(),
            oidc_subject: "subj-1".to_string(),
            email: Some("test@example.com".to_string()),
            display_name: "Test User".to_string(),
            avatar_url: None,
            is_admin: false,
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

/// The four delivery operations alone, so nothing else can answer.
fn build_client(fixture: &TestFixture) -> TestClient<AppState> {
    let service = Router::new()
        .nest(
            "/v1",
            Router::new().mount(kynos::routes![
                stream_file,
                head_stream_file,
                download_file,
                head_download_file,
            ]),
        )
        .build(fixture.state.clone())
        .expect("the delivery router describes itself");

    TestClient::new(service)
}

/// Constructs a minimal `LibraryFile` fixture for a given `(id, path)` pair.
fn make_library_file(id: &str, path: &str) -> LibraryFile {
    LibraryFile {
        id: id.to_string(),
        library_id: "00000000-0000-0000-0000-000000000001".to_string(),
        path: path.to_string(),
        size_bytes: 1024,
        hash: "0".to_string(),
        mime_type: Some("video/mp4".to_string()),
        duration_secs: Some(60.0),
        container_format: Some("mp4".to_string()),
        status: FileIndexStatus::Known,
        content_type: FileContentType::Movie,
        scanned_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

/// A fixture over one temp file of `contents`, ready to serve.
///
/// The directory is held rather than named: it is what keeps the file the
/// handler reads on disk for the life of the test.
struct ServedFile {
    _dir: TempDir,
    client: TestClient<AppState>,
    token: String,
}

async fn serve(name: &str, contents: &[u8]) -> ServedFile {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();

    let fixture = make_test_state(vec![make_library_file(
        TEST_FILE_ID,
        path.to_str().unwrap(),
    )]);
    let client = build_client(&fixture);
    let token = seed_session_token(&fixture).await;

    ServedFile {
        _dir: dir,
        client,
        token,
    }
}

const STREAM: &str = "/v1/files/11111111-1111-1111-1111-111111111111/stream";
const DOWNLOAD: &str = "/v1/files/11111111-1111-1111-1111-111111111111/download";

// ─── GET /v1/files/{file_id}/stream ────────────────────────────────────────

/// The handler must serve the source file's bytes directly -- no transcoding,
/// no remuxing, no cache copy -- and must reflect the file's actual mime type
/// in the response, not a hardcoded "video/mp4".
#[tokio::test]
async fn stream_file_serves_the_source_bytes_as_indexed() {
    let source_bytes = b"REAL SOURCE FILE BYTES, SERVED AS-IS";

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("video.mkv");
    std::fs::write(&path, source_bytes).unwrap();

    let mut file = make_library_file(TEST_FILE_ID, path.to_str().unwrap());
    file.mime_type = Some("video/x-matroska".to_string());
    let fixture = make_test_state(vec![file]);
    let client = build_client(&fixture);
    let token = seed_session_token(&fixture).await;

    let response = client
        .get(STREAM)
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.header("content-type"),
        Some("video/x-matroska"),
        "Content-Type must reflect the file's actual mime type, not an assumed video/mp4"
    );
    assert!(
        response.header("content-disposition").is_none(),
        "stream endpoint must not set Content-Disposition (renders inline)"
    );
    assert_eq!(
        response.bytes().as_ref(),
        &source_bytes[..],
        "response body must be the untouched source file bytes"
    );
}

/// When the file has no known mime type, fall back to a generic binary
/// content type rather than assuming a container format.
#[tokio::test]
async fn stream_file_falls_back_to_octet_stream_without_a_mime_type() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("video.unknown");
    std::fs::write(&path, b"DATA").unwrap();

    let mut file = make_library_file(TEST_FILE_ID, path.to_str().unwrap());
    file.mime_type = None;
    let fixture = make_test_state(vec![file]);
    let client = build_client(&fixture);
    let token = seed_session_token(&fixture).await;

    let response = client
        .get(STREAM)
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(
        response.header("content-type"),
        Some("application/octet-stream")
    );
}

/// When the file is in the library but the source path does not exist on disk,
/// the handler must return 404.
#[tokio::test]
async fn stream_file_missing_from_disk_is_404() {
    let fixture = make_test_state(vec![make_library_file(
        TEST_FILE_ID,
        "/tmp/__nonexistent_source_video_xyz_beam_test__.mkv",
    )]);
    let client = build_client(&fixture);
    let token = seed_session_token(&fixture).await;

    client
        .get(STREAM)
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// When the file ID is not present in the library service, return 404.
#[tokio::test]
async fn stream_file_unknown_to_the_library_is_404() {
    let fixture = make_test_state(vec![]); // empty library
    let client = build_client(&fixture);
    let token = seed_session_token(&fixture).await;

    client
        .get(STREAM)
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// No session cookie at all must return 401.
#[tokio::test]
async fn stream_file_without_a_session_cookie_is_401() {
    let fixture = make_test_state(vec![]);
    let client = build_client(&fixture);

    assert_eq!(
        client.get(STREAM).send().await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// A cookie value that doesn't match any known session must return 401.
#[tokio::test]
async fn stream_file_with_an_unknown_session_cookie_is_401() {
    let fixture = make_test_state(vec![]);
    let client = build_client(&fixture);

    let response = client
        .get(STREAM)
        .cookie("beam_session", "not-a-real-session")
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// A request without a Range header must return 200 and advertise that ranges
/// are available, which is how a player discovers it can seek.
#[tokio::test]
async fn stream_file_without_a_range_returns_the_whole_representation() {
    let contents = b"FAKE SOURCE DATA FOR RANGE TEST";
    let served = serve("video.mkv", contents).await;

    let response = served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.header("accept-ranges"),
        Some("bytes"),
        "a seekable representation must say so"
    );
    assert_eq!(response.bytes().as_ref(), &contents[..]);
}

/// A `Range: bytes=0-99` against a 200-byte source must enclose exactly those
/// hundred octets.
#[tokio::test]
async fn stream_file_serves_the_requested_span() {
    let served = serve("video.mkv", &[7u8; 200]).await;

    served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .header("Range", "bytes=0-99")
        .send()
        .await
        .assert_part(0, 99, 200)
        .assert_header("content-length", "100");
}

/// A suffix range serves the last N octets of the file. The old hand-rolled
/// parser had a property test for this; the guarantee that matters is that the
/// octets sent are the *last* ones, which only an end-to-end read can show.
#[tokio::test]
async fn stream_file_serves_a_suffix_range_from_the_end() {
    let mut contents = vec![0u8; 200];
    contents[150..].fill(9);
    let served = serve("video.mkv", &contents).await;

    let response = served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .header("Range", "bytes=-50")
        .send()
        .await;

    response.assert_part(150, 199, 200);
    assert!(
        response.bytes().iter().all(|byte| *byte == 9),
        "a suffix range must carry the octets at the end of the file"
    );
}

/// An end bound past the file is clamped rather than rejected -- that is what
/// a browser asking for "the next megabyte" sends.
#[tokio::test]
async fn stream_file_clamps_an_end_bound_past_the_file() {
    let served = serve("video.mkv", &[0u8; 200]).await;

    served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .header("Range", "bytes=100-99999")
        .send()
        .await
        .assert_part(100, 199, 200);
}

/// A start bound past the file is not clamped: RFC 9110 requires a 416, and
/// clamping would silently serve the wrong bytes to a resuming client.
#[tokio::test]
async fn stream_file_rejects_a_start_bound_past_the_file() {
    let served = serve("video.mkv", &[0u8; 200]).await;

    served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .header("Range", "bytes=500-")
        .send()
        .await
        .assert_status(StatusCode::RANGE_NOT_SATISFIABLE)
        .assert_header("content-range", "bytes */200")
        // The body is a problem document, and it must say so. `MediaDelivery`
        // used to stamp the file's own content type onto every response that
        // was not a 304, so this JSON went out labelled `video/mkv` -- a body
        // announced as video that a strict client is entitled to reject.
        .assert_header("content-type", "application/problem+json");
}

// ─── Conditional requests ──────────────────────────────────────────────────

/// A client that already holds the representation is told so rather than being
/// sent the bytes again. None of this existed before the Kynos migration, so a
/// seek re-sent bytes the client already had.
#[tokio::test]
async fn a_matching_if_none_match_is_answered_304_without_a_body() {
    let served = serve("video.mkv", &[0u8; 200]).await;

    let first = served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .send()
        .await;
    let etag = first
        .header("etag")
        .expect("a delivery carries a validator")
        .to_owned();

    let second = served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .header("If-None-Match", &etag)
        .send()
        .await;

    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    assert!(second.bytes().is_empty(), "a 304 carries no representation");
    assert_eq!(
        second.header("content-type"),
        None,
        "a 304 carries no representation, so no content type either"
    );
}

/// The weaker validator, honoured for the client that only kept a date.
#[tokio::test]
async fn a_current_if_modified_since_is_answered_304() {
    let served = serve("video.mkv", &[0u8; 200]).await;

    let first = served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .send()
        .await;
    let modified = first
        .header("last-modified")
        .expect("a delivery states when it last changed")
        .to_owned();

    let second = served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .header("If-Modified-Since", &modified)
        .send()
        .await;

    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    assert!(second.bytes().is_empty());
}

/// A resume whose validator no longer matches gets the whole representation
/// rather than a part -- which is the guarantee `If-Range` exists to provide,
/// and the reason the ETag is no longer `"{file_size}"`: that collided for any
/// two files of the same size, so a resume could splice bytes from another.
#[tokio::test]
async fn a_stale_if_range_is_answered_with_the_whole_representation() {
    let served = serve("video.mkv", &[0u8; 200]).await;

    let response = served
        .client
        .get(STREAM)
        .cookie("beam_session", &served.token)
        .header("If-Range", "\"a-validator-from-another-file\"")
        .header("Range", "bytes=100-199")
        .send()
        .await;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a stale If-Range must not produce a 206 the client would splice"
    );
    assert_eq!(response.bytes().len(), 200);
    assert!(response.header("content-range").is_none());
}

// ─── HEAD ──────────────────────────────────────────────────────────────────

/// `HEAD` is a real operation rather than a `GET` with the body dropped, and it
/// is what a player uses to size a stream before starting it.
#[tokio::test]
async fn head_stream_reports_the_length_without_the_body() {
    let served = serve("video.mkv", &[0u8; 200]).await;

    let response = served
        .client
        .head(STREAM)
        .cookie("beam_session", &served.token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.header("content-length"), Some("200"));
    assert_eq!(response.header("accept-ranges"), Some("bytes"));
    assert!(response.bytes().is_empty(), "a HEAD sends no content");
}

/// The same for a download, so a client can see the filename and the size
/// before committing to the transfer.
#[tokio::test]
async fn head_download_reports_the_attachment_without_the_body() {
    let served = serve("My Movie (2024).mkv", &[0u8; 200]).await;

    let response = served
        .client
        .head(DOWNLOAD)
        .cookie("beam_session", &served.token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.header("content-length"), Some("200"));
    assert_eq!(
        response.header("content-disposition"),
        Some("attachment; filename=\"My Movie (2024).mkv\"")
    );
    assert!(response.bytes().is_empty(), "a HEAD sends no content");
}

// ─── GET /v1/files/{file_id}/download ──────────────────────────────────────

/// The download endpoint must serve the same untouched source bytes as stream,
/// but with a `Content-Disposition: attachment` header carrying the file's
/// original on-disk name so browsers save it instead of trying to play it
/// inline.
#[tokio::test]
async fn download_file_sets_an_attachment_disposition_with_the_filename() {
    let source_bytes = b"REAL SOURCE FILE BYTES";
    let served = serve("My Movie (2024).mkv", source_bytes).await;

    let response = served
        .client
        .get(DOWNLOAD)
        .cookie("beam_session", &served.token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.header("content-disposition"),
        Some("attachment; filename=\"My Movie (2024).mkv\"")
    );
    assert_eq!(response.bytes().as_ref(), &source_bytes[..]);
}

/// Download supports Range requests too, so an interrupted download can resume
/// rather than restarting from byte zero.
#[tokio::test]
async fn download_file_supports_range_requests() {
    let mut contents = vec![0u8; 200];
    contents[100..].fill(3);
    let served = serve("video.mkv", &contents).await;

    let response = served
        .client
        .get(DOWNLOAD)
        .cookie("beam_session", &served.token)
        .header("Range", "bytes=100-199")
        .send()
        .await;

    response.assert_part(100, 199, 200);
    assert!(
        response.bytes().iter().all(|byte| *byte == 3),
        "a resumed download must continue from the offset it asked for"
    );
}

/// No session cookie must return 401 for downloads too.
#[tokio::test]
async fn download_file_without_a_session_cookie_is_401() {
    let fixture = make_test_state(vec![]);
    let client = build_client(&fixture);

    assert_eq!(
        client.get(DOWNLOAD).send().await.status(),
        StatusCode::UNAUTHORIZED
    );
}
