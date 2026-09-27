//! Subcutaneous tests for `/v1/files/{file_id}/progress`,
//! `/v1/continue-watching` and `/v1/history`.
//!
//! The playback service here is the real `DbPlaybackService` over in-memory
//! repositories, because what these tests assert is how a reported position
//! comes back out of the other two endpoints -- a multi-step state change no
//! stub could stand in for. The repositories are kept beside the state so a
//! test can seed the file a report is about; everything the playback routes
//! never touch comes from `test_support`.

use std::path::PathBuf;
use std::sync::Arc;

use beam_auth::utils::session_store::SessionData;
use beam_domain::models::movie::Movie;
use beam_domain::models::{MediaFile, MediaFileContent, MovieEntry};
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::playback_progress::in_memory::InMemoryPlaybackProgressRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::services::TestClock;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;

use crate::routes::playback::{
    HistoryResponse, ReportProgressRequest, get_continue_watching, get_history,
    report_playback_progress,
};
use crate::routes::test_support::make_app_state;
use crate::services::playback::{ContinueWatchingItem, DbPlaybackService, PlaybackProgressDto};
use crate::state::{AppServices, AppState};

/// A state whose playback service is real, plus the repositories behind it.
struct Fixture {
    state: AppState,
    file_repo: Arc<InMemoryFileRepository>,
    movie_repo: Arc<InMemoryMovieRepository>,
    /// What the progress rows' `updated_at` is stamped from, so a test orders
    /// reports by advancing it rather than by racing the wall clock.
    progress_clock: Arc<TestClock>,
}

fn fixture() -> Fixture {
    let base = make_app_state();

    let file_repo = Arc::new(InMemoryFileRepository::default());
    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let progress_clock = Arc::new(TestClock::new());
    let playback: Arc<dyn crate::services::playback::PlaybackService> =
        Arc::new(DbPlaybackService::new(
            Arc::new(InMemoryPlaybackProgressRepository::new(
                progress_clock.clone(),
                file_repo.clone(),
            )),
            file_repo.clone(),
            movie_repo.clone(),
            Arc::new(InMemoryShowRepository::default()),
        ));

    let services = AppServices {
        hash: base.services.hash.clone(),
        library: base.services.library.clone(),
        metadata: base.services.metadata.clone(),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        playback,
        genre_repo: base.services.genre_repo.clone(),
        library_repo: base.services.library_repo.clone(),
        file_repo: file_repo.clone(),
        enrichment_repo: base.services.enrichment_repo.clone(),
        movie_repo: movie_repo.clone(),
        show_repo: base.services.show_repo.clone(),
        artwork: base.services.artwork.clone(),
        session_store: base.services.session_store.clone(),
        oidc_client: base.services.oidc_client.clone(),
        pending_auth_store: base.services.pending_auth_store.clone(),
        oidc_config: base.services.oidc_config.clone(),
        watch_status: Arc::new(beam_index::services::watch_status::WatchStatus::new()),
    };

    Fixture {
        state: AppState::new(base.config.clone(), services, base.probe.clone(), None),
        file_repo,
        movie_repo,
        progress_clock,
    }
}

fn client(fixture: &Fixture) -> TestClient<AppState> {
    let service = Router::new()
        .nest(
            "/v1",
            Router::new().mount(kynos::routes![
                report_playback_progress,
                get_continue_watching,
                get_history
            ]),
        )
        .build(fixture.state.clone())
        .expect("the playback router describes itself");

    TestClient::new(service)
}

/// Issues a session directly, bypassing the OIDC login flow.
///
/// The user id is what every playback row is keyed by, so one token is used
/// for a whole test: a second session would be a second user with an empty
/// history.
async fn seed_session(fixture: &Fixture) -> String {
    fixture
        .state
        .services
        .session_store
        .create(
            &SessionData {
                user_id: uuid::Uuid::new_v4().to_string(),
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

fn make_media_file(content: MediaFileContent) -> MediaFile {
    MediaFile {
        id: uuid::Uuid::new_v4(),
        library_id: uuid::Uuid::new_v4(),
        path: PathBuf::from("/media/test.mp4"),
        hash: 0,
        size_bytes: 1024,
        mtime: None,
        mime_type: Some("video/mp4".to_owned()),
        duration: Some(std::time::Duration::from_secs(7200)),
        container_format: Some("mp4".to_owned()),
        content: Some(content),
        status: beam_domain::models::FileStatus::Known,
        scanned_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        missing_since: None,
    }
}

fn make_movie() -> Movie {
    Movie {
        id: uuid::Uuid::new_v4(),
        title: "Test Movie".to_owned(),
        title_localized: None,
        description: None,
        year: None,
        release_date: None,
        runtime: None,
        poster_url: None,
        backdrop_url: None,
        tmdb_id: None,
        imdb_id: None,
        tvdb_id: None,
        anilist_id: None,
        rating_tmdb: None,
        rating_imdb: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

/// Seeds a file whose content resolves to nothing, which is all a progress
/// report itself needs.
fn seed_bare_file(fixture: &Fixture) -> uuid::Uuid {
    let file = make_media_file(MediaFileContent::Movie {
        movie_entry_id: uuid::Uuid::new_v4(),
    });
    let file_id = file.id;
    fixture
        .file_repo
        .files
        .lock()
        .unwrap()
        .insert(file_id, file);
    file_id
}

/// Seeds a resolvable movie + entry + file, and returns both ids.
fn seed_movie_file(fixture: &Fixture) -> (uuid::Uuid, uuid::Uuid) {
    let movie = make_movie();
    let movie_id = movie.id;
    fixture
        .movie_repo
        .movies
        .lock()
        .unwrap()
        .insert(movie_id, movie);

    let entry = MovieEntry {
        id: uuid::Uuid::new_v4(),
        library_id: uuid::Uuid::new_v4(),
        movie_id,
        edition: None,
        is_primary: true,
        created_at: chrono::Utc::now(),
    };
    let entry_id = entry.id;
    fixture
        .movie_repo
        .entries
        .lock()
        .unwrap()
        .insert(entry_id, entry);

    let file = make_media_file(MediaFileContent::Movie {
        movie_entry_id: entry_id,
    });
    let file_id = file.id;
    fixture
        .file_repo
        .files
        .lock()
        .unwrap()
        .insert(file_id, file);

    (movie_id, file_id)
}

/// Reports a position for `file_id` as the holder of `token`.
async fn report(
    client: &TestClient<AppState>,
    token: &str,
    file_id: uuid::Uuid,
    position_secs: f64,
) -> StatusCode {
    client
        .put(&format!("/v1/files/{file_id}/progress"))
        .cookie("beam_session", token)
        .json(&ReportProgressRequest {
            position_secs,
            duration_secs: Some(100.0),
        })
        .send()
        .await
        .status()
}

// ── PUT /v1/files/{file_id}/progress ─────────────────────────────────────────

#[tokio::test]
async fn reporting_progress_requires_a_session() {
    let fixture = fixture();
    let response = client(&fixture)
        .put(&format!("/v1/files/{}/progress", uuid::Uuid::new_v4()))
        .json(&ReportProgressRequest {
            position_secs: 10.0,
            duration_secs: Some(100.0),
        })
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reporting_progress_for_an_unknown_file_is_a_404() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = seed_session(&fixture).await;

    let status = report(&client, &token, uuid::Uuid::new_v4(), 10.0).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_reported_position_comes_back_as_an_incomplete_progress_row() {
    let fixture = fixture();
    let file_id = seed_bare_file(&fixture);
    let client = client(&fixture);
    let token = seed_session(&fixture).await;

    let response = client
        .put(&format!("/v1/files/{file_id}/progress"))
        .cookie("beam_session", &token)
        .json(&ReportProgressRequest {
            position_secs: 42.0,
            duration_secs: Some(100.0),
        })
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: PlaybackProgressDto = response.json();
    assert_eq!(body.position_secs, 42.0);
    assert!(!body.completed);
}

// ── GET /v1/continue-watching ────────────────────────────────────────────────

#[tokio::test]
async fn continue_watching_requires_a_session() {
    let fixture = fixture();
    let response = client(&fixture).get("/v1/continue-watching").send().await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_partially_watched_file_resolves_back_to_its_movie() {
    let fixture = fixture();
    let (movie_id, file_id) = seed_movie_file(&fixture);
    let client = client(&fixture);
    let token = seed_session(&fixture).await;

    assert_eq!(
        report(&client, &token, file_id, 10.0).await,
        StatusCode::OK,
        "the report the rest of this test reads back must have landed"
    );

    let response = client
        .get("/v1/continue-watching")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let items: Vec<ContinueWatchingItem> = response.json();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].media_id, movie_id.to_string());
    assert_eq!(items[0].media_type, "movie");
}

/// A file that goes missing (issue #179) drops out of continue-watching and
/// refuses new reports, but its position is kept: when the file is restored
/// under the same id, the row comes back where the user left it.
#[tokio::test]
async fn a_missing_file_leaves_continue_watching_and_returns_with_its_position_on_restore() {
    use beam_domain::repositories::FileRepository;

    let fixture = fixture();
    let (_, file_id) = seed_movie_file(&fixture);
    let client = client(&fixture);
    let token = seed_session(&fixture).await;
    assert_eq!(report(&client, &token, file_id, 37.0).await, StatusCode::OK);

    let continue_watching = || async {
        let response = client
            .get("/v1/continue-watching")
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        response.json::<Vec<ContinueWatchingItem>>()
    };

    fixture
        .file_repo
        .mark_missing(vec![file_id], chrono::Utc::now())
        .await
        .unwrap();

    assert!(
        continue_watching().await.is_empty(),
        "a missing file cannot be resumed"
    );
    assert_eq!(
        report(&client, &token, file_id, 50.0).await,
        StatusCode::NOT_FOUND,
        "a missing file cannot be played, so it takes no reports"
    );

    fixture.file_repo.restore(file_id).await.unwrap();

    let items = continue_watching().await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].file_id, file_id.to_string());
    assert_eq!(
        items[0].position_secs, 37.0,
        "the position survived the absence"
    );
}

// ── GET /v1/history ──────────────────────────────────────────────────────────

#[tokio::test]
async fn history_requires_a_session() {
    let fixture = fixture();
    let response = client(&fixture).get("/v1/history").send().await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn history_counts_and_returns_the_completed_row_continue_watching_hides() {
    let fixture = fixture();
    let (_, in_progress_file) = seed_movie_file(&fixture);
    let (_, completed_file) = seed_movie_file(&fixture);
    let client = client(&fixture);
    let token = seed_session(&fixture).await;

    assert_eq!(
        report(&client, &token, in_progress_file, 10.0).await,
        StatusCode::OK
    );
    assert_eq!(
        report(&client, &token, completed_file, 99.0).await,
        StatusCode::OK
    );

    let response = client
        .get("/v1/history")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: HistoryResponse = response.json();
    assert_eq!(body.total, 2);
    assert_eq!(body.items.len(), 2);
    assert!(
        body.items.iter().any(|item| item.completed),
        "a completed row belongs in history even though continue-watching drops it"
    );
}

// ── missing files and the list limits (issue #179) ───────────────────────────

/// Seeds `count` movie files, reports progress on each (one clock step apart,
/// each newer than the last) and marks every one missing.
async fn seed_missing_reports(
    fixture: &Fixture,
    client: &TestClient<AppState>,
    token: &str,
    count: usize,
    position_secs: f64,
) {
    use beam_domain::repositories::FileRepository;

    for _ in 0..count {
        fixture
            .progress_clock
            .advance(std::time::Duration::from_secs(60));
        let (_, file_id) = seed_movie_file(fixture);
        assert_eq!(
            report(client, token, file_id, position_secs).await,
            StatusCode::OK
        );
        fixture
            .file_repo
            .mark_missing(vec![file_id], chrono::Utc::now())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn more_missing_rows_than_the_limit_do_not_push_a_present_file_out_of_continue_watching() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = seed_session(&fixture).await;
    let (_, present) = seed_movie_file(&fixture);
    assert_eq!(report(&client, &token, present, 10.0).await, StatusCode::OK);
    // Three missing rows, all newer than the present one, against a limit of
    // two: cut before they are filtered, they would fill the whole list.
    seed_missing_reports(&fixture, &client, &token, 3, 10.0).await;

    let response = client
        .get("/v1/continue-watching?limit=2")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let items: Vec<ContinueWatchingItem> = response.json();
    let files: Vec<String> = items.into_iter().map(|item| item.file_id).collect();
    assert_eq!(files, vec![present.to_string()]);
}

#[tokio::test]
async fn history_items_and_total_leave_out_missing_files() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = seed_session(&fixture).await;
    let (_, present) = seed_movie_file(&fixture);
    assert_eq!(report(&client, &token, present, 99.0).await, StatusCode::OK);
    seed_missing_reports(&fixture, &client, &token, 3, 10.0).await;

    let response = client
        .get("/v1/history?limit=2&offset=0")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: HistoryResponse = response.json();
    let files: Vec<String> = body.items.iter().map(|item| item.file_id.clone()).collect();
    assert_eq!(
        files,
        vec![present.to_string()],
        "the first page is the present file, not the newer missing ones"
    );
    assert_eq!(body.total, 1, "missing files are not counted either");
}
