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
use beam_index::probe::metadata::MetadataError as ProbeError;
use beam_index::services::IndexService;
use beam_index::services::LocalIndexService;
use beam_index::services::media_info::MockMediaInfoService;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::broadcast;

use crate::models::{
    AdminLogEntryDto, AdminStatusResponse, AdminUserListResponse, CreateLibraryRequest, Library,
    ScanJob, ScanState, ScanTrigger, UpdateAdminUserRequest,
};
use crate::routes::admin::{
    create_library, delete_library, get_admin_events, get_admin_log_count, get_admin_logs,
    get_admin_status, get_library, get_library_files, get_library_scan, list_admin_users,
    list_libraries, refresh_media_metadata, scan_library, stream_admin_events, update_admin_user,
};
use crate::services::admin_log::{AdminLogService, LocalAdminLogService};
use crate::services::hash::HashService;
use crate::services::library::{InMemoryPathValidator, LibraryService, LocalLibraryService};
use crate::services::metadata::{
    MediaConnection, MediaFilter, MetadataError, MetadataService, PageInfo,
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
    async fn get_media_metadata(
        &self,
        _media_id: uuid::Uuid,
    ) -> Result<Option<crate::models::MediaMetadata>, MetadataError> {
        Ok(None)
    }
    async fn search_media(
        &self,
        _request: crate::services::metadata::BrowseRequest,
    ) -> Result<MediaConnection, MetadataError> {
        Ok(MediaConnection {
            items: vec![],
            page_info: PageInfo {
                has_next_page: false,
                has_previous_page: false,
                start_cursor: None,
                end_cursor: None,
            },
        })
    }
    async fn refresh_metadata(&self, filter: MediaFilter) -> Result<(), MetadataError> {
        // Part of the trait's contract rather than a shortcut, the same way
        // the media and stream stubs honour it: a malformed id is `InvalidId`,
        // which the route owes a 400. While this stub ignored its argument
        // entirely, the route test below could assert 204 for `some-id` and
        // pass -- codifying the answer production had just stopped giving.
        if let MediaFilter::ByMediaId(media_id) = &filter {
            uuid::Uuid::parse_str(media_id).map_err(|_| MetadataError::InvalidId)?;
        }
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
    /// The real indexer the library service starts its scans on.
    index: Arc<LocalIndexService>,
    /// Every file the indexer hashes takes one permit (see
    /// [`GatedHashService`]).
    hash_gate: Arc<tokio::sync::Semaphore>,
    /// The indexer's clock: it moves only when a test advances it.
    clock: Arc<beam_domain::services::TestClock>,
    /// Where the validator resolves every library root: a real, empty
    /// directory for a fixture from [`make_scan_test_state`], and a path
    /// that does not exist otherwise.
    library_root: PathBuf,
    /// The filesystem watcher the library service tells about created and
    /// deleted libraries.
    watcher: Arc<beam_index::services::watcher::InMemoryFsWatcher>,
    _scratch: Option<TempDir>,
}

impl TestFixture {
    fn library_root(&self) -> &std::path::Path {
        &self.library_root
    }

    /// Let every hash through from now on.
    fn open_gate(&self) {
        self.hash_gate
            .add_permits(tokio::sync::Semaphore::MAX_PERMITS - self.hash_gate.available_permits());
    }
}

/// Whether a scan's hashes wait for the test.
#[derive(Clone, Copy)]
enum Gate {
    Open,
    /// Every hash waits until [`TestFixture::open_gate`], so a test can act
    /// while a scan is running.
    Closed,
}

/// The indexer's hasher, held at a gate the test controls. The hash itself
/// is a stand-in: nothing here compares file contents.
#[derive(Debug)]
struct GatedHashService {
    gate: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl HashService for GatedHashService {
    fn hash_sync(&self, path: &std::path::Path) -> std::io::Result<u64> {
        Ok(path.as_os_str().len() as u64)
    }

    async fn hash_async(&self, path: PathBuf) -> std::io::Result<u64> {
        self.gate
            .acquire()
            .await
            .expect("the gate is never closed")
            .forget();
        Ok(path.as_os_str().len() as u64)
    }
}

fn make_test_state() -> TestFixture {
    make_test_state_with_notification(Arc::new(InMemoryNotificationService::new()))
}

fn make_test_state_with_notification(notification: Arc<dyn NotificationService>) -> TestFixture {
    make_test_state_with_data_dir(notification, PathBuf::from("/beam-data"))
}

/// Like [`make_test_state`], with the library service's data directory
/// chosen by the test.
fn make_test_state_with_data_dir(
    notification: Arc<dyn NotificationService>,
    data_dir: PathBuf,
) -> TestFixture {
    build_fixture(
        notification,
        PathBuf::from("/videos/movies"),
        None,
        data_dir,
        Gate::Open,
    )
}

/// A fixture whose libraries resolve to a real, empty directory, so a scan
/// of one runs for real.
fn make_scan_test_state(gate: Gate) -> TestFixture {
    let scratch = TempDir::new().unwrap();
    let root = scratch.path().join("movies");
    std::fs::create_dir(&root).unwrap();
    build_fixture(
        Arc::new(InMemoryNotificationService::new()),
        root,
        Some(scratch),
        PathBuf::from("/beam-data"),
        gate,
    )
}

fn build_fixture(
    notification: Arc<dyn NotificationService>,
    library_root: PathBuf,
    scratch: Option<TempDir>,
    data_dir: PathBuf,
    gate: Gate,
) -> TestFixture {
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

    let hash_gate = Arc::new(tokio::sync::Semaphore::new(match gate {
        Gate::Open => tokio::sync::Semaphore::MAX_PERMITS,
        Gate::Closed => 0,
    }));
    // No file here is a real film: every probe fails, and the file is
    // indexed without a title, which is all these tests need of it.
    let mut prober = MockMediaInfoService::new();
    prober
        .expect_get_video_metadata()
        .returning(|_| Err(ProbeError::UnknownError("not a film".to_string())));
    let clock = Arc::new(beam_domain::services::TestClock::starting_at(
        chrono::Utc::now(),
    ));
    let index = Arc::new(
        LocalIndexService::new(
        library_repo.clone(),
        file_repo.clone(),
        Arc::new(beam_domain::repositories::movie::in_memory::InMemoryMovieRepository::default()),
        Arc::new(beam_domain::repositories::show::in_memory::InMemoryShowRepository::default()),
        Arc::new(
            beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository::default(),
        ),
        Arc::new(GatedHashService {
            gate: hash_gate.clone(),
        }),
        Arc::new(prober),
        notification.clone(),
        admin_log.clone(),
        Arc::new(beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository::default()),
        )
        .with_clock(clock.clone()),
    );

    let watcher = Arc::new(beam_index::services::watcher::InMemoryFsWatcher::new());
    let library: Arc<dyn LibraryService> = Arc::new(LocalLibraryService::new(
        library_repo.clone(),
        file_repo.clone(),
        PathBuf::from("/videos"),
        data_dir,
        notification.clone(),
        index.clone() as Arc<dyn IndexService>,
        Arc::new(InMemoryPathValidator::success(library_root.clone())),
        Arc::new(beam_index::runtime::LibraryWatches::new(Some(
            watcher.clone(),
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
        device_auth_store: Arc::new(
            beam_auth::utils::device_auth_store::in_memory::InMemoryDeviceAuthStore::default(),
        ),
        oidc_config: OidcRuntimeConfig {
            web_url: "http://localhost:5173".to_string(),
            cookie_secure: false,
            admin_claim: None,
            admin_value: None,
            session_idle_days: 14,
            session_max_days: 60,
        },
        watch_status: Arc::new(beam_index::services::watch_status::WatchStatus::new()),
        telemetry: crate::routes::test_support::idle_library_report(),
        playback_telemetry: crate::routes::test_support::idle_playback_telemetry(),
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
        index,
        hash_gate,
        clock,
        library_root,
        watcher,
        _scratch: scratch,
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
                    get_library_scan,
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

/// Three operations resolve a library by id and share `LibraryRefError`, so a
/// malformed id has to be the same 400 on each. Table-driven because the
/// interesting thing is that the answers agree, not any one of them.
#[tokio::test]
async fn a_malformed_library_id_is_a_400_on_every_route_that_takes_one() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    for (case, request) in [
        ("get one library", client.get("/v1/libraries/not-a-uuid")),
        (
            "list its files",
            client.get("/v1/libraries/not-a-uuid/files"),
        ),
        ("delete it", client.delete("/v1/admin/libraries/not-a-uuid")),
    ] {
        let response = request.cookie("beam_session", &token).send().await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{case}");
        response.assert_problem_type(
            "https://beam.justinchung.net/reference/errors/#invalid-library-id",
        );
    }
}

/// NFR-108: listing a library's files is open to any signed-in user, so the
/// listing must never carry the server's filesystem layout -- each file is
/// shown relative to its library's root.
#[tokio::test]
async fn a_regular_user_listing_library_files_never_sees_a_filesystem_path() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let admin_token = seed_user_session(&fixture, true).await;
    let user_token = seed_user_session(&fixture, false).await;

    // `InMemoryPathValidator` resolves every root to `/videos/movies`.
    let library: Library = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &admin_token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await
        .json();
    let library_id = uuid::Uuid::parse_str(&library.id).unwrap();

    for path in ["/videos/movies/A (2016)/A.mkv", "/videos/movies/b.mp4"] {
        fixture
            .file_repo
            .create(CreateMediaFile {
                library_id,
                path: PathBuf::from(path),
                hash: 1,
                size_bytes: 10,
                mtime: None,
                mime_type: None,
                duration: None,
                container_format: None,
                content: None,
                status: FileStatus::Unknown,
                classifier_version: 0,
            })
            .await
            .unwrap();
    }

    let response = client
        .get(&format!("/v1/libraries/{library_id}/files"))
        .cookie("beam_session", &user_token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>();
    assert!(
        !body.to_string().contains("/videos"),
        "the library root must not appear anywhere in the listing: {body}"
    );
    let mut paths: Vec<&str> = body
        .as_array()
        .expect("the listing is an array")
        .iter()
        .map(|file| file["path"].as_str().expect("every file has a path"))
        .collect();
    paths.sort_unstable();
    assert_eq!(paths, ["A (2016)/A.mkv", "b.mp4"]);
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

    // Named, so a client can tell "sign in again as an admin" apart from the
    // same-origin 403s this operation can also answer with.
    response
        .assert_status(StatusCode::FORBIDDEN)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#admin-required");
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

// ─── Library scans (issue #181) ──────────────────────────────────────────────

/// Create a library through the API -- the fixture's validator resolves it to
/// the fixture's library root -- and return it with its id parsed.
async fn create_movies_library(
    client: &TestClient<AppState>,
    token: &str,
) -> (Library, uuid::Uuid) {
    let created: Library = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await
        .json();
    let id = uuid::Uuid::parse_str(&created.id).expect("a library id is a UUID");
    (created, id)
}

/// Wait -- on the job itself, never on a clock -- until `library_id`'s latest
/// scan satisfies `done`, bounded so a hang fails rather than stalls.
async fn wait_for_scan(
    fixture: &TestFixture,
    library_id: uuid::Uuid,
    done: impl FnMut(&Option<beam_index::services::ScanJob>) -> bool,
) -> beam_index::services::ScanJob {
    let mut jobs = fixture.index.subscribe_scan(library_id);
    tokio::time::timeout(std::time::Duration::from_secs(10), jobs.wait_for(done))
        .await
        .expect("the scan reaches the awaited state")
        .expect("the indexer is alive")
        .clone()
        .expect("a scan job")
}

fn finished(job: &Option<beam_index::services::ScanJob>) -> bool {
    job.as_ref().is_some_and(|job| !job.state.is_active())
}

fn running(job: &Option<beam_index::services::ScanJob>) -> bool {
    job.as_ref()
        .is_some_and(|job| job.state == beam_index::services::ScanState::Running)
}

/// The request answers before the scan runs: 202 with the job, queued. The
/// scan then runs to the end in the background, indexing the library, and
/// the job reads as succeeded.
#[tokio::test]
async fn scanning_a_library_answers_202_with_a_queued_job_that_then_succeeds() {
    let fixture = make_scan_test_state(Gate::Open);
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let (created, library_id) = create_movies_library(&client, &token).await;
    let video = fixture.library_root().join("Heat (1995).mkv");
    std::fs::write(&video, b"not really a film").unwrap();

    let response = client
        .post(&format!("/v1/admin/libraries/{}/scan", created.id))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let job: ScanJob = response.json();
    assert_eq!(job.library_id, library_id);
    assert_eq!(job.trigger, ScanTrigger::Manual);
    assert_eq!(job.state, ScanState::Queued);
    assert_eq!(job.started_at, None);

    let done = wait_for_scan(&fixture, library_id, finished).await;
    assert_eq!(done.id, job.id, "the job the request answered with ran");
    assert_eq!(done.state, beam_index::services::ScanState::Succeeded);
    assert_eq!(done.progress.added, 1);
    assert!(
        fixture
            .file_repo
            .find_by_path(&video.to_string_lossy())
            .await
            .unwrap()
            .is_some(),
        "the scan indexed the file"
    );

    let read = client
        .get(&format!("/v1/admin/libraries/{}/scan", created.id))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(read.status(), StatusCode::OK);
    let read: ScanJob = read.json();
    assert_eq!(read.id, job.id);
    assert_eq!(read.state, ScanState::Succeeded);
    assert_eq!(read.progress.added_count, 1);
    assert_eq!(read.progress.total_count, Some(1));
    assert!(read.finished_at.is_some());
}

/// One scan per library at a time: asking again while one runs is a 409 with
/// its own problem type, and starts nothing. Once it has finished, a new scan
/// is accepted.
#[tokio::test]
async fn a_second_scan_while_one_runs_is_409_and_starts_nothing() {
    let fixture = make_scan_test_state(Gate::Closed);
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let (created, library_id) = create_movies_library(&client, &token).await;
    std::fs::write(fixture.library_root().join("Heat (1995).mkv"), b"film").unwrap();
    let scan = format!("/v1/admin/libraries/{}/scan", created.id);

    let first: ScanJob = client
        .post(&scan)
        .cookie("beam_session", &token)
        .send()
        .await
        .json();
    // Held on the file's hash.
    wait_for_scan(&fixture, library_id, running).await;

    let second = client
        .post(&scan)
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    second.assert_problem_type(
        "https://beam.justinchung.net/reference/errors/#library-scan-in-progress",
    );
    assert_eq!(
        fixture.index.scan_job(library_id).map(|job| job.id),
        Some(first.id),
        "the refused request registered no job"
    );

    fixture.open_gate();
    let done = wait_for_scan(&fixture, library_id, finished).await;
    assert_eq!(done.id, first.id);

    let third = client
        .post(&scan)
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(
        third.status(),
        StatusCode::ACCEPTED,
        "a finished scan no longer holds the library"
    );
    wait_for_scan(&fixture, library_id, finished).await;
}

/// A library deleted while it is being scanned: the delete cancels the scan
/// and waits, with the library still in place, until the scan has stopped
/// after the file it was on and failed as cancelled -- never as an internal
/// error from writing for a library that is gone. Then 204, and the library's
/// latest job is forgotten with it.
#[tokio::test]
async fn deleting_a_library_mid_scan_waits_for_the_cancelled_scan() {
    let fixture = make_scan_test_state(Gate::Closed);
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let (created, library_id) = create_movies_library(&client, &token).await;
    for name in ["Heat (1995).mkv", "Ronin (1998).mkv"] {
        std::fs::write(fixture.library_root().join(name), b"film").unwrap();
    }

    let accepted = client
        .post(&format!("/v1/admin/libraries/{}/scan", created.id))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    wait_for_scan(&fixture, library_id, running).await;
    let jobs = fixture.index.subscribe_scan(library_id);

    let delete = client
        .delete(&format!("/v1/admin/libraries/{}", created.id))
        .cookie("beam_session", &token)
        .send();
    let release = async {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        // The delete has cancelled the scan and is waiting on its timeout.
        while fixture.clock.waiter_count() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the delete never waited"
            );
            tokio::task::yield_now().await;
        }
        assert!(
            fixture
                .state
                .services
                .library_repo
                .find_by_id(library_id)
                .await
                .unwrap()
                .is_some(),
            "the library is kept while its scan runs"
        );
        fixture.open_gate();
    };
    let (deleted, ()) = tokio::join!(delete, release);

    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let done = jobs.borrow().clone().expect("the scan's last job");
    assert_eq!(done.state, beam_index::services::ScanState::Failed);
    assert_eq!(done.failure.as_deref(), Some("cancelled"));
    assert!(
        done.progress.processed < 2,
        "the scan stopped before the second file"
    );
    assert_eq!(
        fixture.index.scan_job(library_id),
        None,
        "the deleted library's job is forgotten"
    );
}

/// A rescan of a library whose root has gone since registration is a 400 whose
/// `detail` is the indexer's own message, passed through unchanged. That
/// message is path-free by construction in `beam-index` (asserted there), so
/// the route has no reason to replace it -- and must not (NFR-108). No job is
/// registered for it.
#[tokio::test]
async fn scanning_a_library_whose_root_has_gone_is_400_without_a_path() {
    const REASON: &str = "Library root path does not exist or is not a directory";
    let fixture = make_scan_test_state(Gate::Open);
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let (created, library_id) = create_movies_library(&client, &token).await;
    std::fs::remove_dir(fixture.library_root()).unwrap();

    let response = client
        .post(&format!("/v1/admin/libraries/{}/scan", created.id))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    response.assert_problem_type(
        "https://beam.justinchung.net/reference/errors/#library-path-not-found",
    );
    let detail = response.json::<Value>()["detail"]
        .as_str()
        .expect("problem detail is a string")
        .to_string();
    // The subject is the pass-through, so assert the indexer's reason survives
    // rather than comparing against `IndexError`'s `Display`.
    assert!(
        detail.ends_with(REASON),
        "the route must pass the indexer's reason through unchanged: {detail:?}"
    );
    assert!(
        !detail.contains('/'),
        "no filesystem path may reach the client: {detail}"
    );
    assert_eq!(
        fixture.index.scan_job(library_id),
        None,
        "no job was registered"
    );
}

/// Scan jobs live in memory: a library not scanned since the server started
/// has none, which is its own 404 -- distinct from a library that does not
/// exist.
#[tokio::test]
async fn reading_the_scan_of_a_library_never_scanned_is_404_scan_not_found() {
    let fixture = make_scan_test_state(Gate::Open);
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let (created, _) = create_movies_library(&client, &token).await;

    let response = client
        .get(&format!("/v1/admin/libraries/{}/scan", created.id))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    response.assert_problem_type("https://beam.justinchung.net/reference/errors/#scan-not-found");
}

/// The scan routes capture a `Uuid` (wire conventions), so a malformed id is
/// the `Path` extractor's own `about:blank` 400; an
/// unknown one is the library's 404.
#[tokio::test]
async fn a_scan_of_a_malformed_or_unknown_library_is_400_or_404() {
    let fixture = make_scan_test_state(Gate::Open);
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let unknown = uuid::Uuid::new_v4();

    for (method, path, status, problem) in [
        (
            "POST",
            "/v1/admin/libraries/not-a-uuid/scan".to_string(),
            StatusCode::BAD_REQUEST,
            "about:blank",
        ),
        (
            "GET",
            "/v1/admin/libraries/not-a-uuid/scan".to_string(),
            StatusCode::BAD_REQUEST,
            "about:blank",
        ),
        (
            "POST",
            format!("/v1/admin/libraries/{unknown}/scan"),
            StatusCode::NOT_FOUND,
            "https://beam.justinchung.net/reference/errors/#library-not-found",
        ),
        (
            "GET",
            format!("/v1/admin/libraries/{unknown}/scan"),
            StatusCode::NOT_FOUND,
            "https://beam.justinchung.net/reference/errors/#library-not-found",
        ),
    ] {
        let request = match method {
            "POST" => client.post(&path),
            _ => client.get(&path),
        };
        let response = request.cookie("beam_session", &token).send().await;
        assert_eq!(response.status(), status, "{method} {path}");
        response.assert_problem_type(problem);
    }
    assert_eq!(fixture.index.scan_job(unknown), None);
}

#[tokio::test]
async fn scans_are_closed_to_a_regular_user() {
    let fixture = make_scan_test_state(Gate::Open);
    let client = build_client(&fixture);
    let admin = seed_user_session(&fixture, true).await;
    let (created, library_id) = create_movies_library(&client, &admin).await;
    let token = seed_user_session(&fixture, false).await;
    let scan = format!("/v1/admin/libraries/{}/scan", created.id);

    assert_eq!(
        client
            .post(&scan)
            .cookie("beam_session", &token)
            .send()
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(&scan)
            .cookie("beam_session", &token)
            .send()
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fixture.index.scan_job(library_id),
        None,
        "a refused request starts no scan"
    );
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

/// A library is watched from the moment it is created, and no longer from
/// the moment it is deleted -- not from the next maintenance cycle (issue
/// #180).
#[tokio::test]
async fn a_created_library_is_watched_at_once_and_a_deleted_one_unwatched() {
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
    let id = uuid::Uuid::parse_str(&created.id).unwrap();
    // Registered on a task of its own, after the request has answered.
    fixture.watcher.until_watched(id).await;
    assert_eq!(fixture.watcher.watched_libraries(), vec![id]);

    let deleted = client
        .delete(&format!("/v1/admin/libraries/{}", created.id))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(fixture.watcher.watched_libraries().is_empty());
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
            AdminEvent::info(
                EventCategory::ScanProgress,
                "Scanning 'Movies': 3 of 8 files",
                Some("lib-1".to_string()),
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
    assert_eq!(events.len(), 3, "one record per broadcast event");

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
        .post("/v1/admin/media/0199a1f0-0000-7000-8000-000000000001/refresh")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
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
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-media-id");
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
            status: FileStatus::Unknown,
            classifier_version: 0,
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

// ── Library root conflicts (issue #186) ─────────────────────────────────────

#[tokio::test]
async fn registering_a_root_that_overlaps_an_existing_library_is_409_and_changes_nothing() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let create = || {
        client
            .post("/v1/admin/libraries")
            .cookie("beam_session", &token)
            .json(&CreateLibraryRequest {
                name: "Movies".to_string(),
                root_path: "movies".to_string(),
            })
            .send()
    };
    assert_eq!(create().await.status(), StatusCode::OK);

    // The fixture's validator resolves every request to the same root.
    let response = create().await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let problem: serde_json::Value = response.json();
    assert_eq!(
        problem["type"],
        "https://beam.justinchung.net/reference/errors/#library-path-overlaps-library"
    );
    let detail = problem["detail"].as_str().unwrap_or_default();
    assert!(
        !detail.contains('/'),
        "the detail names no path (NFR-108): {detail:?}"
    );
    let listed = client
        .get("/v1/libraries")
        .cookie("beam_session", &token)
        .send()
        .await
        .json::<Vec<Library>>();
    assert_eq!(listed.len(), 1, "the rejected library was not stored");
}

#[tokio::test]
async fn registering_a_root_that_holds_the_data_directory_is_400_and_changes_nothing() {
    // The fixture's validator resolves every request to /videos/movies.
    let fixture = make_test_state_with_data_dir(
        Arc::new(InMemoryNotificationService::new()),
        PathBuf::from("/videos/movies/.beam"),
    );
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

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem: serde_json::Value = response.json();
    assert_eq!(
        problem["type"],
        "https://beam.justinchung.net/reference/errors/#library-path-overlaps-data-dir"
    );
    let detail = problem["detail"].as_str().unwrap_or_default();
    assert!(
        !detail.contains('/'),
        "the detail names no path (NFR-108): {detail:?}"
    );
    let listed = client
        .get("/v1/libraries")
        .cookie("beam_session", &token)
        .send()
        .await
        .json::<Vec<Library>>();
    assert!(listed.is_empty(), "the rejected library was not stored");
}

// ── Watcher status (issue #186) ─────────────────────────────────────────────

#[tokio::test]
async fn the_status_endpoint_reports_how_each_library_is_watched() {
    use crate::models::{LibraryPollReason, LibraryWatchMode};
    use beam_index::services::watch_status::{PollReason, WatchMode};

    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let library: Library = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await
        .json();
    let library_id = uuid::Uuid::parse_str(&library.id).unwrap();

    let get_watcher = || async {
        client
            .get("/v1/admin/status")
            .cookie("beam_session", &token)
            .send()
            .await
            .json::<AdminStatusResponse>()
            .watcher
    };

    // Nothing has registered a watch yet: the library is unwatched.
    let watcher = get_watcher().await;
    assert!(!watcher.enabled);
    assert_eq!(watcher.libraries.len(), 1);
    assert_eq!(watcher.libraries[0].library_id, library_id);
    assert_eq!(watcher.libraries[0].mode, LibraryWatchMode::Unwatched);
    assert_eq!(watcher.libraries[0].poll_reason, None);

    // What the runtime writes when the library hits the watch limit.
    let status = &fixture.state.services.watch_status;
    status.set_enabled(true);
    status.set_max_user_watches(Some(8192));
    status.mark_limit_reached();
    status.set_mode(
        library_id,
        WatchMode::Polling(PollReason::WatchLimitReached),
    );

    let watcher = get_watcher().await;
    assert!(watcher.enabled);
    assert!(watcher.watch_limit_reached);
    assert_eq!(watcher.watch_limit_count, Some(8192));
    assert_eq!(watcher.libraries[0].mode, LibraryWatchMode::Polling);
    assert_eq!(
        watcher.libraries[0].poll_reason,
        Some(LibraryPollReason::WatchLimitReached)
    );
}
