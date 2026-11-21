//! Subcutaneous tests for the `/v1/libraries` and `/v1/admin/*` REST routes.
//!
//! Driven through Kynos's in-process `TestClient` over a real router: real
//! handlers, real `AdminAuth`/`SessionAuth` extractors, and in-memory
//! implementations for everything below the trait line -- no Redis, no
//! PostgreSQL. Library CRUD/scan runs against the real `LocalLibraryService`
//! (backed by in-memory repos), not a stub, so this also exercises that
//! service's actual logic through the REST surface.
//!
//! This file holds the shared fixture and the library tests; the log, event
//! and refresh tests are in `admin_logs_tests.rs`, and the user, status and
//! access tests in `admin_users_tests.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use beam_auth::utils::{
    models::CreateUser,
    repository::{UserRepository, in_memory::InMemoryUserRepository},
    session_store::{SessionData, SessionStore, in_memory::InMemorySessionStore},
};
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::{AdminLogRepository, FileRepository};
use beam_domain::test_utils::create_media_file;
use beam_index::probe::metadata::MetadataError as ProbeError;
use beam_index::services::IndexService;
use beam_index::services::LocalIndexService;
use beam_index::services::media_info::MockMediaInfoService;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use serde_json::Value;
use tempfile::TempDir;

use crate::models::{
    CreateLibraryRequest, Library, LibraryConnection, ScanJob, ScanState, ScanTrigger,
};
use crate::routes::admin::{
    create_library, delete_library, get_admin_events, get_admin_logs, get_admin_status,
    get_admin_user_access, get_library, get_library_files, get_library_scan, list_admin_users,
    list_libraries, refresh_media_metadata, scan_library, set_admin_user_access,
    stream_admin_events, update_admin_user,
};
use crate::services::library::{
    InMemoryPathValidator, LibraryService, LibraryServiceDependencies, LocalLibraryService,
};
use crate::state::{AppServices, AppState};
use beam_index::services::hash::HashService;
use beam_index::services::notification::{InMemoryNotificationService, NotificationService};

type InMemoryFileRepo = beam_domain::repositories::file::in_memory::InMemoryFileRepository;
type InMemoryEnrichmentRepo =
    beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;

pub(super) struct TestFixture {
    pub(super) state: AppState,
    pub(super) session_store: Arc<InMemorySessionStore>,
    pub(super) user_repo: Arc<dyn UserRepository>,
    pub(super) file_repo: Arc<InMemoryFileRepo>,
    pub(super) enrichment_repo: Arc<InMemoryEnrichmentRepo>,
    pub(super) movie_repo:
        Arc<beam_domain::repositories::movie::in_memory::InMemoryMovieRepository>,
    pub(super) notification: Arc<dyn NotificationService>,
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

/// A hash distinct for each file, standing in for its content's.
fn stand_in_hash(file: &std::fs::File) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt as _;
    Ok(file.metadata()?.ino())
}

/// The indexer's hasher, held at a gate the test controls. The hash itself
/// is a stand-in: nothing here compares file contents.
#[derive(Debug)]
struct GatedHashService {
    gate: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl HashService for GatedHashService {
    async fn hash_async(&self, file: std::fs::File) -> std::io::Result<u64> {
        self.gate
            .acquire()
            .await
            .expect("the gate is never closed")
            .forget();
        stand_in_hash(&file)
    }
}

pub(super) fn make_test_state() -> TestFixture {
    make_test_state_with_notification(Arc::new(InMemoryNotificationService::new()))
}

pub(super) fn make_test_state_with_notification(
    notification: Arc<dyn NotificationService>,
) -> TestFixture {
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
        Arc::new(InMemoryUserRepository::default()),
    )
}

/// Like [`make_test_state`], over the user store `users`.
pub(super) fn make_test_state_with_users(users: Arc<dyn UserRepository>) -> TestFixture {
    build_fixture(
        Arc::new(InMemoryNotificationService::new()),
        PathBuf::from("/videos/movies"),
        None,
        PathBuf::from("/beam-data"),
        Gate::Open,
        users,
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
        Arc::new(InMemoryUserRepository::default()),
    )
}

fn build_fixture(
    notification: Arc<dyn NotificationService>,
    library_root: PathBuf,
    scratch: Option<TempDir>,
    data_dir: PathBuf,
    gate: Gate,
    user_repo: Arc<dyn UserRepository>,
) -> TestFixture {
    let session_store = Arc::new(InMemorySessionStore::default());

    let admin_log: Arc<dyn AdminLogRepository> = Arc::new(InMemoryAdminLogRepository::default());

    // Shared between the library service and `AppServices::{library_repo,
    // file_repo}` so the status endpoint's counts reflect libraries/files
    // created through the library service.
    let library_repo = Arc::new(InMemoryLibraryRepository::default());
    let file_repo = Arc::new(InMemoryFileRepo::default());
    let enrichment_repo = Arc::new(InMemoryEnrichmentRepo::default());
    let movie_repo =
        Arc::new(beam_domain::repositories::movie::in_memory::InMemoryMovieRepository::default());
    let show_repo =
        Arc::new(beam_domain::repositories::show::in_memory::InMemoryShowRepository::default());
    let admin_log_for_control = admin_log.clone();

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
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
        )
        .with_clock(clock.clone()),
    );

    let watcher = Arc::new(beam_index::services::watcher::InMemoryFsWatcher::new());
    let library: Arc<dyn LibraryService> =
        Arc::new(LocalLibraryService::new(LibraryServiceDependencies {
            library_repo: library_repo.clone(),
            file_repo: file_repo.clone(),
            video_dir: PathBuf::from("/videos"),
            data_dir,
            notification_service: notification.clone(),
            index_service: index.clone() as Arc<dyn IndexService>,
            path_validator: Arc::new(InMemoryPathValidator::success(library_root.clone())),
            watch_hook: Arc::new(beam_index::runtime::LibraryWatches::new(Some(
                watcher.clone(),
            ))),
        }));

    let services = AppServices {
        library,
        notification: notification.clone(),
        admin_log,
        user_repo: user_repo.clone(),
        library_repo: library_repo.clone(),
        file_repo: file_repo.clone(),
        enrichment_repo: enrichment_repo.clone(),
        enrichment_control: crate::routes::test_support::enrichment_control(
            movie_repo.clone(),
            show_repo.clone(),
            enrichment_repo.clone(),
            library_repo,
            Arc::new(beam_domain::providers::enrichment::NoopEnrichmentProvider),
            admin_log_for_control,
        ),
        people: crate::routes::test_support::idle_people(movie_repo.clone(), show_repo.clone()),
        movie_repo: movie_repo.clone(),
        show_repo,
        session_store: session_store.clone(),
        ..crate::routes::test_support::idle_services()
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
        movie_repo,
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
pub(super) async fn seed_user_session(fixture: &TestFixture, is_admin: bool) -> String {
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
pub(super) fn build_client(fixture: &TestFixture) -> TestClient<AppState> {
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
                    get_admin_events,
                    stream_admin_events,
                    list_admin_users,
                    update_admin_user,
                    get_admin_status,
                ])
                .mount(kynos::routes![get_admin_user_access, set_admin_user_access]),
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
    let listed: LibraryConnection = response.json();
    assert!(listed.items.is_empty());
    assert!(
        !listed.page_info.has_next_page && !listed.page_info.has_previous_page,
        "the whole list is one page"
    );
}

/// The stored scan count is a signed column; a negative one is reported as
/// unknown rather than wrapped into a huge unsigned count, while a real count
/// is reported as it is.
#[tokio::test]
async fn a_negative_stored_scan_count_is_listed_as_unknown() {
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
    let listed_count = || async {
        let listed = client
            .get("/v1/libraries")
            .cookie("beam_session", &token)
            .send()
            .await
            .json::<Value>();
        let listed = listed["items"].as_array().expect("a connection's items");
        assert_eq!(listed.len(), 1);
        listed[0]["last_scan_file_count"].clone()
    };
    let repo = &fixture.state.services.library_repo;

    repo.update_scan_progress(library.id, None, None, Some(-1))
        .await
        .expect("the in-memory repository does not fail");
    assert_eq!(listed_count().await, Value::Null);

    repo.update_scan_progress(library.id, None, None, Some(7))
        .await
        .expect("the in-memory repository does not fail");
    assert_eq!(listed_count().await, serde_json::json!(7));
}

/// Every route naming a library by id types it `Uuid`, so a malformed id is
/// the `Path` extractor's 400 on each. Table-driven because the interesting
/// thing is that the answers agree, not any one of them.
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
    let library_id = library.id;

    for path in ["/videos/movies/A (2016)/A.mkv", "/videos/movies/b.mp4"] {
        fixture
            .file_repo
            .create(create_media_file(library_id, path))
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
    let mut paths: Vec<&str> = body["items"]
        .as_array()
        .expect("the listing is a connection")
        .iter()
        .map(|file| file["path"].as_str().expect("every file has a path"))
        .collect();
    paths.sort_unstable();
    assert_eq!(paths, ["A (2016)/A.mkv", "b.mp4"]);
}

/// Seed a present file at `path` under `library_id`.
async fn seed_file(fixture: &TestFixture, library_id: uuid::Uuid, path: &str) {
    fixture
        .file_repo
        .create(create_media_file(library_id, path))
        .await
        .unwrap();
}

/// A library's files page by path: `first` bounds a page, its `end_cursor`
/// starts the next, the walk visits every file once in path order, and the
/// cursor carries the root-relative path only (NFR-108).
#[tokio::test]
async fn a_library_s_files_page_by_path() {
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
    for name in ["c.mkv", "a.mkv", "b.mkv"] {
        seed_file(&fixture, library.id, &format!("/videos/movies/{name}")).await;
    }

    let mut walked = Vec::new();
    let mut after: Option<String> = None;
    let mut pages = 0;
    loop {
        let uri = match &after {
            Some(cursor) => format!("/v1/libraries/{}/files?first=2&after={cursor}", library.id),
            None => format!("/v1/libraries/{}/files?first=2", library.id),
        };
        let page: Value = client
            .get(&uri)
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::OK)
            .json();
        pages += 1;
        assert_eq!(page["page_info"]["has_previous_page"], after.is_some());
        let items = page["items"].as_array().expect("a connection's items");
        assert!(items.len() <= 2, "a page holds at most `first`");
        walked.extend(
            items
                .iter()
                .map(|file| file["path"].as_str().unwrap().to_owned()),
        );
        let end = page["page_info"]["end_cursor"].as_str().map(str::to_owned);
        if let Some(cursor) = &end {
            let decoded = URL_SAFE_NO_PAD.decode(cursor).expect("base64url");
            let decoded = String::from_utf8(decoded).expect("JSON text");
            assert!(!decoded.contains("/videos"), "no absolute path: {decoded}");
        }
        if page["page_info"]["has_next_page"] != true {
            break;
        }
        after = end;
    }
    assert_eq!(pages, 2);
    assert_eq!(walked, ["a.mkv", "b.mkv", "c.mkv"]);
}

/// A file-list cursor from another library is `400 #invalid-cursor`, not a
/// position in this one; and a page size past 500 is refused at the edge.
#[tokio::test]
async fn a_library_s_file_list_refuses_another_library_s_cursor() {
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
    let foreign = crate::services::cursor::library_files::encode(
        uuid::Uuid::new_v4(),
        "a.mkv",
        uuid::Uuid::nil(),
    );

    client
        .get(&format!(
            "/v1/libraries/{}/files?after={foreign}",
            library.id
        ))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-cursor");
    client
        .get(&format!("/v1/libraries/{}/files?first=501", library.id))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("about:blank");
    client
        .get(&format!("/v1/libraries/{}/files", uuid::Uuid::new_v4()))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#library-not-found");
}

// ─── Library mutations: admin-gated ─────────────────────────────────────────

/// A blank name, a name over 200 characters and a blank root path are each
/// `422 #validation-failed`, naming the field by pointer, and nothing is
/// stored.
#[tokio::test]
async fn creating_a_library_with_a_bad_name_or_root_is_422_and_changes_nothing() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    for (case, name, root_path, pointers) in [
        ("blank name", "  ", "movies", vec!["/name"]),
        ("long name", &*"n".repeat(201), "movies", vec!["/name"]),
        ("blank root", "Movies", "", vec!["/root_path"]),
        ("both", "", " ", vec!["/name", "/root_path"]),
    ] {
        let problem: Value = client
            .post("/v1/admin/libraries")
            .cookie("beam_session", &token)
            .json(&CreateLibraryRequest {
                name: name.to_string(),
                root_path: root_path.to_string(),
            })
            .send()
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#validation-failed")
            .json();
        let named: Vec<&str> = problem["errors"]
            .as_array()
            .unwrap_or_else(|| panic!("{case}: an errors list: {problem}"))
            .iter()
            .map(|error| error["pointer"].as_str().unwrap())
            .collect();
        assert_eq!(named, pointers, "{case}");
    }

    // 200 characters -- multi-byte ones, counted as characters -- is allowed.
    client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &token)
        .json(&CreateLibraryRequest {
            name: "é".repeat(200),
            root_path: "movies".to_string(),
        })
        .send()
        .await
        .assert_status(StatusCode::OK);
    let listed = client
        .get("/v1/libraries")
        .cookie("beam_session", &token)
        .send()
        .await
        .json::<LibraryConnection>()
        .items;
    assert_eq!(listed.len(), 1, "only the valid library was stored");
}

#[tokio::test]
async fn creating_a_library_as_a_regular_user_is_403_and_stores_nothing() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;
    let admin = seed_user_session(&fixture, true).await;

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

    let listed = client
        .get("/v1/libraries")
        .cookie("beam_session", &admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json::<LibraryConnection>()
        .items;
    assert!(listed.is_empty(), "a refused create stores no library");
    assert!(
        fixture.watcher.watched_libraries().is_empty(),
        "a refused create watches nothing"
    );
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
        .json::<LibraryConnection>()
        .items;

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
    let id = created.id;
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
async fn deleting_a_library_is_closed_to_a_regular_user_and_keeps_it() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let admin = seed_user_session(&fixture, true).await;
    let token = seed_user_session(&fixture, false).await;

    let created: Library = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &admin)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    fixture.watcher.until_watched(created.id).await;
    let path = format!("/v1/admin/libraries/{}", created.id);

    client
        .delete(&path)
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#admin-required");
    client
        .delete(&path)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    let kept: Library = client
        .get(&format!("/v1/libraries/{}", created.id))
        .cookie("beam_session", &admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(kept.id, created.id, "the refused delete kept the library");
    assert_eq!(
        fixture.watcher.watched_libraries(),
        vec![created.id],
        "and kept watching it"
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
    let id = created.id;
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
        .json::<LibraryConnection>()
        .items;
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
        .json::<LibraryConnection>()
        .items;
    assert!(listed.is_empty(), "the rejected library was not stored");
}
