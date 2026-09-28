//! Subcutaneous tests for watched state (issue #188): progress reports,
//! title progress, watched marks, continue-watching, history, and the state
//! laid over the detail routes.
//!
//! The playback and metadata services here are the real ones over in-memory
//! stores, seeded through the repositories' own traits, because what these
//! tests assert is how one request's state comes back out of another -- a
//! multi-step change no stub could stand in for.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use beam_auth::utils::session_store::SessionData;
use beam_domain::models::file::{CreateMediaFile, FileStatus, MediaFileContent};
use beam_domain::models::movie::{CreateMovie, CreateMovieEntry};
use beam_domain::models::show::{CreateEpisode, CreateShow};
use beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::{FileRepository, MovieRepository, ShowRepository};
use beam_domain::services::TestClock;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use uuid::Uuid;

use crate::models::playback::{
    ContinueWatchingConnection, ContinueWatchingReason, HistoryConnection, ReportProgressRequest,
    UserTitleState,
};
use crate::models::{EpisodeDetail, MediaConnection, MediaMetadata, MediaTypeFilter, SeasonDetail};
use crate::routes::media::{browse_media, get_episode_detail, get_media_detail, get_season_detail};
use crate::routes::playback::{
    clear_title_progress, dismiss_continue_watching, get_continue_watching, get_history,
    get_title_progress, mark_unwatched, mark_watched, report_playback_progress,
};
use crate::routes::test_support::make_app_state;
use crate::services::metadata::{DbMetadataService, MetadataRepositories};
use crate::services::playback::{CANDIDATE_CEILING, DbPlaybackService, PlaybackRepositories};
use crate::services::sources::SourceCatalog;
use crate::state::{AppServices, AppState};

/// A state whose playback and metadata services are real, over the stores
/// kept here so a test can seed titles and files through them.
struct Fixture {
    state: AppState,
    files: Arc<InMemoryFileRepository>,
    movies: Arc<InMemoryMovieRepository>,
    shows: Arc<InMemoryShowRepository>,
    /// What `last_played_at` is stamped from, so a test orders plays by
    /// advancing it rather than by racing the wall clock.
    clock: Arc<TestClock>,
    library_id: Uuid,
}

fn fixture() -> Fixture {
    let base = make_app_state();
    let files = Arc::new(InMemoryFileRepository::default());
    let movies = Arc::new(InMemoryMovieRepository::with_files(files.clone()));
    let shows = Arc::new(InMemoryShowRepository::with_files(files.clone()));
    let genres = Arc::new(InMemoryGenreRepository::default());
    let clock = Arc::new(TestClock::new());
    let sources = Arc::new(SourceCatalog::new(
        movies.clone(),
        files.clone(),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(
            beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository::default(),
        ),
    ));
    let playback = Arc::new(DbPlaybackService::new(PlaybackRepositories {
        watch_state: Arc::new(
            beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::new(
                clock.clone(),
            ),
        ),
        files: files.clone(),
        movies: movies.clone(),
        shows: shows.clone(),
        sources: sources.clone(),
    }));
    let metadata = Arc::new(DbMetadataService::new(MetadataRepositories {
        movies: movies.clone(),
        shows: shows.clone(),
        sources,
        catalog: Arc::new(InMemoryCatalogRepository::new(
            movies.clone(),
            shows.clone(),
            genres.clone(),
        )),
        genres: genres.clone(),
    }));

    let services = AppServices {
        hash: base.services.hash.clone(),
        library: base.services.library.clone(),
        metadata,
        subtitles: base.services.subtitles.clone(),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        playback,
        genre_repo: genres,
        library_repo: base.services.library_repo.clone(),
        file_repo: files.clone(),
        enrichment_repo: base.services.enrichment_repo.clone(),
        enrichment_control: base.services.enrichment_control.clone(),
        movie_repo: movies.clone(),
        show_repo: shows.clone(),
        artwork: base.services.artwork.clone(),
        session_store: base.services.session_store.clone(),
        oidc_client: base.services.oidc_client.clone(),
        pending_auth_store: base.services.pending_auth_store.clone(),
        device_auth_store: base.services.device_auth_store.clone(),
        oidc_config: base.services.oidc_config.clone(),
        watch_status: Arc::new(beam_index::services::watch_status::WatchStatus::new()),
        telemetry: crate::routes::test_support::idle_library_report(),
        playback_telemetry: crate::routes::test_support::idle_playback_telemetry(),
    };

    Fixture {
        state: AppState::new(base.config.clone(), services, base.probe.clone(), None),
        files,
        movies,
        shows,
        clock,
        library_id: Uuid::new_v4(),
    }
}

fn client(fixture: &Fixture) -> TestClient<AppState> {
    let service = Router::new()
        .nest(
            "/v1",
            Router::new().mount(kynos::routes![
                report_playback_progress,
                get_title_progress,
                clear_title_progress,
                mark_watched,
                mark_unwatched,
                get_continue_watching,
                dismiss_continue_watching,
                get_history,
                browse_media,
                get_media_detail,
                get_episode_detail,
                get_season_detail,
            ]),
        )
        .build(fixture.state.clone())
        .expect("the playback router describes itself");
    TestClient::new(service)
}

/// A session for a new user. Every row is keyed by the user, so one token
/// is one viewer.
async fn session(fixture: &Fixture) -> String {
    fixture
        .state
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

impl Fixture {
    async fn movie(&self, title: &str) -> Uuid {
        let movie = self
            .movies
            .find_or_create_by_identity(CreateMovie::new(title, None, None))
            .await
            .unwrap();
        // Enrichment is what stores artwork; the detail and the rows serve
        // Beam's own path for it.
        if let Some(stored) = self.movies.movies.lock().unwrap().get_mut(&movie.id) {
            stored.poster_url = Some("https://image.example/poster.jpg".to_string());
        }
        movie.id
    }

    async fn file(&self, content: MediaFileContent, size_bytes: u64, duration_secs: u64) -> Uuid {
        let unique = Uuid::new_v4();
        self.files
            .create(CreateMediaFile {
                library_id: self.library_id,
                path: PathBuf::from(format!("/videos/{unique}.mkv")),
                hash: 0,
                size_bytes,
                mtime: None,
                identity: None,
                mime_type: Some("video/x-matroska".to_string()),
                duration: Some(Duration::from_secs(duration_secs)),
                container_format: Some("matroska".to_string()),
                content: Some(content),
                status: FileStatus::Known,
                classifier_version: 0,
                container_tags: None,
            })
            .await
            .unwrap()
            .id
    }

    /// A file of `movie_id`, `size_bytes` large -- the larger is primary --
    /// lasting 100 seconds.
    async fn movie_file(&self, movie_id: Uuid, size_bytes: u64) -> Uuid {
        let entry = self
            .movies
            .find_or_create_entry(CreateMovieEntry {
                library_id: self.library_id,
                movie_id,
                edition: None,
            })
            .await
            .unwrap();
        self.file(
            MediaFileContent::Movie {
                movie_entry_id: entry.id,
            },
            size_bytes,
            100,
        )
        .await
    }

    async fn show(&self, title: &str) -> Uuid {
        let show = self
            .shows
            .find_or_create_by_identity(CreateShow::new(title, None))
            .await
            .unwrap();
        if let Some(stored) = self.shows.shows.lock().unwrap().get_mut(&show.id) {
            stored.poster_url = Some("https://image.example/show.jpg".to_string());
        }
        show.id
    }

    /// Episode `number` of season `season` of `show_id`, with no file.
    async fn bare_episode(&self, show_id: Uuid, season: u32, number: u32) -> Uuid {
        let season = self
            .shows
            .find_or_create_season(show_id, season)
            .await
            .unwrap();
        self.shows
            .find_or_create_episode(CreateEpisode {
                season_id: season.id,
                episode_number: number,
                title: format!("Episode {number}"),
                runtime: None,
                air_date: None,
            })
            .await
            .unwrap()
            .id
    }

    /// Episode `number` of season `season` with one 100-second file; returns
    /// `(episode, file)`.
    async fn episode(&self, show_id: Uuid, season: u32, number: u32) -> (Uuid, Uuid) {
        let episode = self.bare_episode(show_id, season, number).await;
        let file = self
            .file(MediaFileContent::episode(episode), 1024, 100)
            .await;
        (episode, file)
    }

    fn later(&self) {
        self.clock.advance(Duration::from_secs(60));
    }
}

async fn report(
    client: &TestClient<AppState>,
    token: &str,
    file_id: Uuid,
    position_secs: f64,
    duration_secs: Option<f64>,
) -> kynos::test::TestResponse {
    client
        .put(&format!("/v1/files/{file_id}/progress"))
        .cookie("beam_session", token)
        .json(&ReportProgressRequest {
            position_secs,
            duration_secs,
        })
        .send()
        .await
}

/// Report `position_secs` of a 100-second file, which must be accepted.
async fn watch(client: &TestClient<AppState>, token: &str, file_id: Uuid, position_secs: f64) {
    let response = report(client, token, file_id, position_secs, Some(100.0)).await;
    assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
}

async fn progress(client: &TestClient<AppState>, token: &str, id: Uuid) -> UserTitleState {
    let response = client
        .get(&format!("/v1/media/{id}/progress"))
        .cookie("beam_session", token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
    response.json()
}

async fn continue_watching(
    client: &TestClient<AppState>,
    token: &str,
) -> ContinueWatchingConnection {
    let response = client
        .get("/v1/continue-watching")
        .cookie("beam_session", token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
    response.json()
}

async fn history(client: &TestClient<AppState>, token: &str, query: &str) -> HistoryConnection {
    let response = client
        .get(&format!("/v1/history{query}"))
        .cookie("beam_session", token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
    response.json()
}

async fn set_watched(
    client: &TestClient<AppState>,
    token: &str,
    id: Uuid,
    watched: bool,
) -> StatusCode {
    let path = format!("/v1/media/{id}/watched");
    let request = if watched {
        client.put(&path)
    } else {
        client.delete(&path)
    };
    request.cookie("beam_session", token).send().await.status()
}

// ── Reporting ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_route_requires_a_session() {
    let fixture = fixture();
    let client = client(&fixture);
    let id = Uuid::new_v4();
    for response in [
        client.get(&format!("/v1/media/{id}/progress")).send().await,
        client
            .delete(&format!("/v1/media/{id}/progress"))
            .send()
            .await,
        client.put(&format!("/v1/media/{id}/watched")).send().await,
        client
            .delete(&format!("/v1/continue-watching/{id}"))
            .send()
            .await,
        client.get("/v1/continue-watching").send().await,
        client.get("/v1/history").send().await,
        client.get(&format!("/v1/episodes/{id}")).send().await,
    ] {
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

/// The regression the issue was opened on: switching to another source of a
/// title resumes where the viewer was, and the row remembers which source
/// they chose.
#[tokio::test]
async fn another_source_of_a_title_resumes_where_the_first_stopped() {
    let fixture = fixture();
    let movie = fixture.movie("Heat").await;
    let hd = fixture.movie_file(movie, 1_000).await;
    let uhd = fixture.movie_file(movie, 4_000).await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, hd, 40.0).await;

    let state = progress(&client, &token, movie).await;
    assert_eq!(state.position_secs, 40.0);
    assert_eq!(state.last_file_id, Some(hd));
    let shelf = continue_watching(&client, &token).await;
    assert_eq!(shelf.items.len(), 1);
    assert_eq!(
        shelf.items[0].file_id, hd,
        "resume on the source the viewer chose, not the primary ({uhd})"
    );

    watch(&client, &token, uhd, 45.0).await;
    let state = progress(&client, &token, movie).await;
    assert_eq!((state.position_secs, state.last_file_id), (45.0, Some(uhd)));
}

#[tokio::test]
async fn reaching_the_end_plays_the_title_and_a_rewind_never_unplays_it() {
    let fixture = fixture();
    let movie = fixture.movie("Heat").await;
    let file = fixture.movie_file(movie, 1_000).await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, file, 95.0).await;
    let state = progress(&client, &token, movie).await;
    assert!(state.played);
    assert_eq!((state.position_secs, state.play_count), (0.0, 1));
    assert!(
        continue_watching(&client, &token).await.items.is_empty(),
        "a finished movie leaves the shelf"
    );

    watch(&client, &token, file, 10.0).await;
    let state = progress(&client, &token, movie).await;
    assert!(state.played, "a rewind is a rewatch");
    assert_eq!(state.position_secs, 10.0);
    let shelf = continue_watching(&client, &token).await;
    assert_eq!(shelf.items.len(), 1, "a rewatch resumes");
    assert_eq!(shelf.items[0].reason, ContinueWatchingReason::Resume);
}

#[tokio::test]
async fn a_report_no_player_could_send_is_refused_and_changes_nothing() {
    let fixture = fixture();
    let movie = fixture.movie("Heat").await;
    let file = fixture.movie_file(movie, 1_000).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    watch(&client, &token, file, 30.0).await;

    for (position, duration, pointer) in [
        (-5.0, Some(100.0), "/position_secs"),
        (500.0, Some(100.0), "/position_secs"),
        (10.0, Some(0.0), "/duration_secs"),
        // No duration in the report: the probed 100 seconds is the end.
        (500.0, None, "/position_secs"),
    ] {
        let response = report(&client, &token, file, position, duration).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        assert_eq!(
            body["type"],
            "https://beam.justinchung.net/reference/errors/#validation-failed"
        );
        assert_eq!(body["errors"][0]["pointer"], pointer, "{body}");
    }
    assert_eq!(progress(&client, &token, movie).await.position_secs, 30.0);

    let response = report(&client, &token, file, 101.0, Some(100.0)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "within tolerance of the end"
    );
    let state: UserTitleState = response.json();
    assert!(state.played, "clamped to the end, which plays the title");
}

#[tokio::test]
async fn a_report_for_a_file_that_is_no_title_is_a_404() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let orphan = fixture
        .file(
            MediaFileContent::Movie {
                movie_entry_id: Uuid::new_v4(),
            },
            1_000,
            100,
        )
        .await;

    for file in [Uuid::new_v4(), orphan] {
        let response = report(&client, &token, file, 10.0, Some(100.0)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = response.json();
        assert_eq!(
            body["type"],
            "https://beam.justinchung.net/reference/errors/#file-not-found"
        );
    }
}

// ── Title progress ───────────────────────────────────────────────────────────

#[tokio::test]
async fn title_progress_is_zeros_before_a_play_and_only_for_movies_and_episodes() {
    let fixture = fixture();
    let movie = fixture.movie("Heat").await;
    let show = fixture.show("Dark").await;
    let (episode, _) = fixture.episode(show, 1, 1).await;
    let season = fixture.shows.find_seasons_by_show_id(show).await.unwrap()[0].id;
    let client = client(&fixture);
    let token = session(&fixture).await;

    assert_eq!(
        progress(&client, &token, movie).await,
        UserTitleState::default()
    );
    assert_eq!(
        progress(&client, &token, episode).await,
        UserTitleState::default()
    );
    for id in [show, season] {
        let response = client
            .get(&format!("/v1/media/{id}/progress"))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = response.json();
        assert_eq!(
            body["type"],
            "https://beam.justinchung.net/reference/errors/#progress-not-available-for-show"
        );
    }
    let unknown = client
        .get(&format!("/v1/media/{}/progress", Uuid::new_v4()))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let malformed = client
        .get("/v1/media/not-a-uuid/progress")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn clearing_progress_drops_the_title_from_the_shelf_and_keeps_played_in_history() {
    let fixture = fixture();
    let started = fixture.movie("Heat").await;
    let started_file = fixture.movie_file(started, 1_000).await;
    let rewatching = fixture.movie("Ronin").await;
    let rewatching_file = fixture.movie_file(rewatching, 1_000).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    watch(&client, &token, started_file, 30.0).await;
    watch(&client, &token, rewatching_file, 99.0).await;
    watch(&client, &token, rewatching_file, 20.0).await;
    assert_eq!(continue_watching(&client, &token).await.items.len(), 2);

    for id in [started, rewatching] {
        let response = client
            .delete(&format!("/v1/media/{id}/progress"))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    assert!(continue_watching(&client, &token).await.items.is_empty());
    assert_eq!(
        progress(&client, &token, started).await,
        UserTitleState::default()
    );
    let page = history(&client, &token, "").await;
    assert_eq!(page.total, 1, "only the played title stays in history");
    assert_eq!(page.items[0].media_id, rewatching);
    assert!(page.items[0].played);
}

// ── Watched marks ────────────────────────────────────────────────────────────

#[tokio::test]
async fn marking_a_show_watched_plays_every_episode_and_unwatched_forgets_them() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (e1, _) = fixture.episode(show, 1, 1).await;
    let (e2, _) = fixture.episode(show, 1, 2).await;
    let (e3, _) = fixture.episode(show, 2, 1).await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    assert_eq!(
        set_watched(&client, &token, show, true).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        set_watched(&client, &token, show, true).await,
        StatusCode::NO_CONTENT
    );

    let response = client
        .get(&format!("/v1/media/{show}"))
        .cookie("beam_session", &token)
        .send()
        .await;
    let MediaMetadata::Show(detail) = response.json() else {
        panic!("a show's detail");
    };
    let group = detail
        .user_state
        .expect("the detail carries the show's state");
    assert!(group.played);
    assert_eq!((group.watched_episode_count, group.episode_count), (3, 3));
    for season in &detail.seasons {
        assert!(season.user_state.as_ref().expect("each season's").played);
        for episode in &season.episodes {
            assert!(episode.user_state.played);
            assert_eq!(
                episode.user_state.play_count, 1,
                "marking twice is one play"
            );
        }
    }
    assert!(
        continue_watching(&client, &token).await.items.is_empty(),
        "nothing is left to watch"
    );

    // An episode added later is unwatched, and next up.
    fixture.later();
    let (e4, _) = fixture.episode(show, 2, 2).await;
    assert!(!progress(&client, &token, e4).await.played);

    let season_one = fixture.shows.find_seasons_by_show_id(show).await.unwrap()[0].id;
    assert_eq!(
        set_watched(&client, &token, season_one, false).await,
        StatusCode::NO_CONTENT
    );
    for (episode, played) in [(e1, false), (e2, false), (e3, true)] {
        assert_eq!(progress(&client, &token, episode).await.played, played);
    }
    assert_eq!(
        set_watched(&client, &token, Uuid::new_v4(), true).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn one_viewers_state_is_never_anothers() {
    let fixture = fixture();
    let movie = fixture.movie("Heat").await;
    let file = fixture.movie_file(movie, 1_000).await;
    let client = client(&fixture);
    let (alice, bob) = (session(&fixture).await, session(&fixture).await);

    watch(&client, &alice, file, 30.0).await;
    assert_eq!(
        set_watched(&client, &alice, movie, true).await,
        StatusCode::NO_CONTENT
    );

    assert_eq!(
        progress(&client, &bob, movie).await,
        UserTitleState::default()
    );
    assert!(continue_watching(&client, &bob).await.items.is_empty());
    assert_eq!(history(&client, &bob, "").await.total, 0);
}

// ── Continue-watching ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_finished_episode_offers_the_next_with_what_the_row_displays() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (_, f1) = fixture.episode(show, 1, 1).await;
    let (e2, _) = fixture.episode(show, 1, 2).await;
    // A second, larger source of E2: the primary, which a next-up plays.
    let e2_primary = fixture
        .file(MediaFileContent::episode(e2), 9_000, 100)
        .await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, f1, 99.0).await;

    let shelf = continue_watching(&client, &token).await;
    assert_eq!(shelf.items.len(), 1);
    let item = &shelf.items[0];
    assert_eq!(item.reason, ContinueWatchingReason::NextUp);
    assert_eq!(
        (item.media_id, item.media_type),
        (show, MediaTypeFilter::Show)
    );
    assert_eq!(item.episode_id, Some(e2));
    assert_eq!(item.file_id, e2_primary);
    assert_eq!(item.position_secs, 0.0);
    assert_eq!(
        (item.season_number, item.episode_number),
        (Some(1), Some(2))
    );
    assert_eq!(item.episode_title.as_deref(), Some("Episode 2"));
    assert!(item.title.starts_with("Dark"));
    assert_eq!(
        item.poster_url.as_deref(),
        Some(format!("/v1/artwork/show/{show}/poster").as_str())
    );
    assert!(!shelf.page_info.has_next_page);
}

#[tokio::test]
async fn a_show_is_one_row_however_many_episodes_are_started() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let movie = fixture.movie("Heat").await;
    let movie_file = fixture.movie_file(movie, 1_000).await;
    let (_, f1) = fixture.episode(show, 1, 1).await;
    let (e2, f2) = fixture.episode(show, 1, 2).await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, f1, 30.0).await;
    fixture.later();
    watch(&client, &token, movie_file, 30.0).await;
    fixture.later();
    watch(&client, &token, f2, 50.0).await;

    let shelf = continue_watching(&client, &token).await;
    let rows: Vec<(Uuid, Option<Uuid>, f64)> = shelf
        .items
        .iter()
        .map(|i| (i.media_id, i.episode_id, i.position_secs))
        .collect();
    assert_eq!(
        rows,
        vec![(show, Some(e2), 50.0), (movie, None, 30.0)],
        "the show once, at the episode touched last, newest first"
    );
}

#[tokio::test]
async fn next_up_steps_over_an_episode_with_no_file_and_ends_with_the_show() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (_, f1) = fixture.episode(show, 1, 1).await;
    fixture.bare_episode(show, 1, 2).await;
    let (e3, f3) = fixture.episode(show, 1, 3).await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, f1, 99.0).await;
    assert_eq!(
        continue_watching(&client, &token).await.items[0].episode_id,
        Some(e3)
    );

    fixture.later();
    watch(&client, &token, f3, 99.0).await;
    assert!(continue_watching(&client, &token).await.items.is_empty());
}

#[tokio::test]
async fn a_missing_last_file_falls_back_to_the_primary() {
    let fixture = fixture();
    let movie = fixture.movie("Heat").await;
    let small = fixture.movie_file(movie, 1_000).await;
    let large = fixture.movie_file(movie, 4_000).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    watch(&client, &token, small, 30.0).await;

    fixture
        .files
        .mark_missing(vec![small], chrono::Utc::now())
        .await
        .unwrap();

    let shelf = continue_watching(&client, &token).await;
    assert_eq!(shelf.items[0].file_id, large);
    assert_eq!(shelf.items[0].position_secs, 30.0);

    fixture
        .files
        .mark_missing(vec![large], chrono::Utc::now())
        .await
        .unwrap();
    assert!(
        continue_watching(&client, &token).await.items.is_empty(),
        "nothing left to play it from"
    );
}

#[tokio::test]
async fn a_dismissed_title_stays_away_until_it_is_played_again() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (e1, f1) = fixture.episode(show, 1, 1).await;
    let (_, f2) = fixture.episode(show, 1, 2).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    watch(&client, &token, f1, 30.0).await;

    // An episode id dismisses its show.
    let response = client
        .delete(&format!("/v1/continue-watching/{e1}"))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(continue_watching(&client, &token).await.items.is_empty());
    assert_eq!(
        progress(&client, &token, e1).await.position_secs,
        30.0,
        "dismissing keeps the resume point"
    );

    fixture.later();
    watch(&client, &token, f2, 10.0).await;
    assert_eq!(continue_watching(&client, &token).await.items.len(), 1);

    let unknown = client
        .delete(&format!("/v1/continue-watching/{}", Uuid::new_v4()))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_multi_episode_file_watched_to_its_end_plays_its_whole_run() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let e1 = fixture.bare_episode(show, 1, 1).await;
    let e2 = fixture.bare_episode(show, 1, 2).await;
    let e3 = fixture.bare_episode(show, 1, 3).await;
    let (e4, _) = fixture.episode(show, 1, 4).await;
    let run = fixture
        .file(
            MediaFileContent::Episode {
                episode_id: e1,
                last_episode_number: Some(3),
            },
            1_000,
            100,
        )
        .await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, run, 98.0).await;

    for episode in [e1, e2, e3] {
        assert!(progress(&client, &token, episode).await.played, "{episode}");
    }
    assert!(!progress(&client, &token, e4).await.played);
    assert_eq!(
        continue_watching(&client, &token).await.items[0].episode_id,
        Some(e4),
        "next up is after the run"
    );
}

/// Going back to an earlier episode does not lose the one left part-way
/// after it: when next-up lands on it, the row resumes it, on the source it
/// was being played from (FR-507).
#[tokio::test]
async fn next_up_onto_an_episode_under_way_resumes_it_from_its_source() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (_, f1) = fixture.episode(show, 1, 1).await;
    let (e2, f2) = fixture.episode(show, 1, 2).await;
    // A second, larger source of E2: the primary, which the viewer did not
    // choose.
    let e2_primary = fixture
        .file(MediaFileContent::episode(e2), 9_000, 100)
        .await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, f2, 60.0).await;
    fixture.later();
    // Back to E1, watched to its end: the anchor, and E2 is next.
    watch(&client, &token, f1, 99.0).await;

    let shelf = continue_watching(&client, &token).await;
    assert_eq!(shelf.items.len(), 1);
    let item = &shelf.items[0];
    assert_eq!(item.episode_id, Some(e2));
    assert_eq!(item.reason, ContinueWatchingReason::Resume);
    assert_eq!(item.position_secs, 60.0, "the resume point stands");
    assert_eq!(item.duration_secs, Some(100.0));
    assert_eq!(
        item.file_id, f2,
        "the source it was played from, not the primary ({e2_primary})"
    );

    // With no position to resume, the same episode is offered from the start.
    let fresh = session(&fixture).await;
    watch(&client, &fresh, f1, 99.0).await;
    let item = &continue_watching(&client, &fresh).await.items[0];
    assert_eq!(
        (
            item.reason,
            item.episode_id,
            item.position_secs,
            item.file_id
        ),
        (ContinueWatchingReason::NextUp, Some(e2), 0.0, e2_primary)
    );
}

#[tokio::test]
async fn continue_watching_is_bounded_by_first() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    for title in ["A", "B", "C"] {
        let movie = fixture.movie(title).await;
        let file = fixture.movie_file(movie, 1_000).await;
        watch(&client, &token, file, 10.0).await;
        fixture.later();
    }

    let response = client
        .get("/v1/continue-watching?first=2")
        .cookie("beam_session", &token)
        .send()
        .await;
    let shelf: ContinueWatchingConnection = response.json();
    assert_eq!(shelf.items.len(), 2);
    assert!(
        shelf.page_info.has_next_page,
        "a title past a full shelf may offer a row"
    );
    let whole = continue_watching(&client, &token).await;
    assert_eq!(whole.items.len(), 3);
    assert!(!whole.page_info.has_next_page, "every candidate was read");
    for first in ["0", "51"] {
        let response = client
            .get(&format!("/v1/continue-watching?first={first}"))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "first={first}");
    }
}

/// Mark `count` new shows of one episode each watched, all after everything
/// played so far.
async fn caught_up_shows(
    fixture: &Fixture,
    client: &TestClient<AppState>,
    token: &str,
    count: u64,
) {
    fixture.later();
    for n in 0..count {
        let show = fixture
            .show(&format!("Caught up {n} {}", Uuid::new_v4()))
            .await;
        fixture.episode(show, 1, 1).await;
        assert_eq!(
            set_watched(client, token, show, true).await,
            StatusCode::NO_CONTENT
        );
    }
}

/// A bulk mark makes caught-up shows by the hundred, each played more
/// recently than anything in progress. They are read past, a page of
/// candidates after another, not counted against the shelf.
#[tokio::test]
async fn caught_up_shows_newer_than_an_unfinished_title_never_crowd_it_out() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture.movie("Heat").await;
    let file = fixture.movie_file(movie, 1_000).await;
    watch(&client, &token, file, 30.0).await;
    // More than one page of candidates, so the movie is found by reading on.
    caught_up_shows(&fixture, &client, &token, 250).await;

    for first in [1, 2] {
        let response = client
            .get(&format!("/v1/continue-watching?first={first}"))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
        let shelf: ContinueWatchingConnection = response.json();
        let rows: Vec<Uuid> = shelf.items.iter().map(|i| i.media_id).collect();
        assert_eq!(rows, vec![movie], "first={first}");
        assert!(!shelf.page_info.has_next_page, "first={first}");
    }
}

/// However many caught-up shows a viewer has, one load examines at most
/// [`CANDIDATE_CEILING`] titles. A title just inside it is listed; one just
/// past it is not, and the shelf says more may exist.
#[tokio::test]
async fn a_load_examines_candidates_up_to_the_ceiling_and_says_when_more_may_exist() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture.movie("Heat").await;
    let file = fixture.movie_file(movie, 1_000).await;
    watch(&client, &token, file, 30.0).await;
    caught_up_shows(&fixture, &client, &token, CANDIDATE_CEILING - 1).await;

    let inside = continue_watching(&client, &token).await;
    let rows: Vec<Uuid> = inside.items.iter().map(|i| i.media_id).collect();
    assert_eq!(rows, vec![movie], "the last title the ceiling reaches");
    assert!(!inside.page_info.has_next_page);

    caught_up_shows(&fixture, &client, &token, 1).await;
    let past = continue_watching(&client, &token).await;
    assert!(past.items.is_empty(), "the movie is past the ceiling");
    assert!(
        past.page_info.has_next_page,
        "a title past the ceiling may offer a row"
    );
    assert_eq!(
        progress(&client, &token, movie).await.position_secs,
        30.0,
        "past the ceiling, the title keeps its place"
    );
}

// ── History ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn history_pages_by_cursor_newest_first_with_display_fields() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (episode, episode_file) = fixture.episode(show, 1, 1).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    let mut movies = Vec::new();
    for title in ["A", "B", "C"] {
        let movie = fixture.movie(title).await;
        let file = fixture.movie_file(movie, 1_000).await;
        watch(&client, &token, file, 99.0).await;
        fixture.later();
        movies.push(movie);
    }
    watch(&client, &token, episode_file, 40.0).await;

    let first = history(&client, &token, "?first=2").await;
    assert_eq!(first.total, 4);
    assert!(first.page_info.has_next_page);
    assert_eq!(first.items[0].episode_id, Some(episode));
    assert_eq!(first.items[0].media_id, show);
    assert!(!first.items[0].played);
    assert_eq!(first.items[1].media_id, movies[2]);
    assert!(first.items[1].played);
    assert_eq!(first.items[1].title, "C");

    let after = first.page_info.end_cursor.expect("a cursor to page on");
    let second = history(&client, &token, &format!("?first=2&after={after}")).await;
    let ids: Vec<Uuid> = second.items.iter().map(|i| i.media_id).collect();
    assert_eq!(ids, vec![movies[1], movies[0]]);
    assert!(!second.page_info.has_next_page);
    assert!(second.page_info.has_previous_page);

    let bad = client
        .get("/v1/history?after=not-a-cursor")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
}

// ── Detail payloads ──────────────────────────────────────────────────────────

/// Browse lays each movie's state over the page -- played, in progress, or
/// untouched -- for the one signed in, and for no one else.
#[tokio::test]
async fn browse_carries_each_movies_state_for_the_viewer() {
    let fixture = fixture();
    let played = fixture.movie("Heat").await;
    let played_file = fixture.movie_file(played, 1_000).await;
    let started = fixture.movie("Ronin").await;
    let started_file = fixture.movie_file(started, 1_000).await;
    let untouched = fixture.movie("Thief").await;
    fixture.movie_file(untouched, 1_000).await;
    let client = client(&fixture);
    let (token, other) = (session(&fixture).await, session(&fixture).await);
    watch(&client, &token, played_file, 99.0).await;
    watch(&client, &token, started_file, 40.0).await;

    let browse = |token: String| {
        let client = &client;
        async move {
            let response = client
                .get("/v1/media")
                .cookie("beam_session", &token)
                .send()
                .await;
            assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
            response
                .json::<MediaConnection>()
                .items
                .into_iter()
                .filter_map(|title| match title {
                    MediaMetadata::Movie(movie) => Some((movie.id, movie.user_state)),
                    MediaMetadata::Show(_) => None,
                })
                .collect::<std::collections::HashMap<Uuid, UserTitleState>>()
        }
    };

    let states = browse(token.clone()).await;
    assert_eq!(states.len(), 3, "{states:?}");
    let heat = &states[&played];
    assert!(heat.played);
    assert_eq!((heat.play_count, heat.position_secs), (1, 0.0));
    let ronin = &states[&started];
    assert!(!ronin.played);
    assert_eq!(
        (ronin.position_secs, ronin.last_file_id),
        (40.0, Some(started_file))
    );
    assert_eq!(states[&untouched], UserTitleState::default());

    let theirs = browse(other).await;
    assert!(
        theirs
            .values()
            .all(|state| *state == UserTitleState::default()),
        "another viewer sees none of it: {theirs:?}"
    );
}

#[tokio::test]
async fn an_episodes_detail_carries_its_show_its_neighbours_and_the_viewers_state() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (special, _) = fixture.episode(show, 0, 1).await;
    let (e1, f1) = fixture.episode(show, 1, 1).await;
    // No file to play: navigation steps over it, as next-up does.
    fixture.bare_episode(show, 1, 2).await;
    let (e2, _) = fixture.episode(show, 2, 1).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    watch(&client, &token, f1, 42.0).await;

    let response = client
        .get(&format!("/v1/episodes/{e1}"))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let detail: EpisodeDetail = response.json();
    assert_eq!(detail.episode.id, e1);
    assert_eq!(detail.season_number, 1);
    assert_eq!(detail.show.id, show);
    assert_eq!(
        detail.previous_episode_id, None,
        "season 1 does not step back into the specials ({special})"
    );
    assert_eq!(
        detail.next_episode_id,
        Some(e2),
        "past an episode with no file, across the season boundary"
    );
    assert_eq!(detail.episode.user_state.position_secs, 42.0);
    assert_eq!(detail.episode.user_state.last_file_id, Some(f1));

    for path in [
        format!("/v1/episodes/{}", Uuid::new_v4()),
        format!("/v1/seasons/{}", Uuid::new_v4()),
        // A show's id is no episode's.
        format!("/v1/episodes/{show}"),
    ] {
        let response = client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    let malformed = client
        .get("/v1/episodes/nope")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
}

/// The next of an episode whose file holds a run is the one after the run:
/// playing the file plays the run, so "next" must not replay part of it.
#[tokio::test]
async fn an_episodes_next_is_after_the_run_its_file_holds() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let e1 = fixture.bare_episode(show, 1, 1).await;
    fixture
        .file(
            MediaFileContent::Episode {
                episode_id: e1,
                last_episode_number: Some(3),
            },
            1_000,
            100,
        )
        .await;
    // E2 and E3 also have files of their own.
    fixture.episode(show, 1, 2).await;
    let (e3, _) = fixture.episode(show, 1, 3).await;
    let (e4, _) = fixture.episode(show, 1, 4).await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    let detail = |id: Uuid| {
        let client = &client;
        let token = &token;
        async move {
            let response = client
                .get(&format!("/v1/episodes/{id}"))
                .cookie("beam_session", token)
                .send()
                .await;
            assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
            response.json::<EpisodeDetail>()
        }
    };
    let first = detail(e1).await;
    assert_eq!(
        (first.previous_episode_id, first.next_episode_id),
        (None, Some(e4))
    );
    let last = detail(e4).await;
    assert_eq!(
        (last.previous_episode_id, last.next_episode_id),
        (Some(e3), None)
    );
}

#[tokio::test]
async fn a_seasons_detail_carries_its_episodes_states_and_its_own() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (e1, f1) = fixture.episode(show, 1, 1).await;
    let (e2, _) = fixture.episode(show, 1, 2).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    watch(&client, &token, f1, 99.0).await;
    let season = fixture.shows.find_seasons_by_show_id(show).await.unwrap()[0].id;

    let response = client
        .get(&format!("/v1/seasons/{season}"))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let detail: SeasonDetail = response.json();
    assert_eq!(detail.show.id, show);
    let played: Vec<(Uuid, bool)> = detail
        .season
        .episodes
        .iter()
        .map(|e| (e.id, e.user_state.played))
        .collect();
    assert_eq!(played, vec![(e1, true), (e2, false)]);
    let group = detail.season.user_state.expect("the season's state");
    assert_eq!(
        (
            group.played,
            group.watched_episode_count,
            group.episode_count
        ),
        (false, 1, 2)
    );
}

#[tokio::test]
async fn a_movies_detail_carries_the_viewers_state() {
    let fixture = fixture();
    let movie = fixture.movie("Heat").await;
    let file = fixture.movie_file(movie, 1_000).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    watch(&client, &token, file, 99.0).await;

    let response = client
        .get(&format!("/v1/media/{movie}"))
        .cookie("beam_session", &token)
        .send()
        .await;
    let MediaMetadata::Movie(detail) = response.json() else {
        panic!("a movie's detail");
    };
    assert!(detail.user_state.played);
    assert_eq!(detail.user_state.play_count, 1);
}
