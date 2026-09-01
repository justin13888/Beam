//! Subcutaneous tests for the `/v1/media` browse, detail and sources routes.
//!
//! The router, the handlers and the session scheme are the real ones; only the
//! metadata service below the trait line is a double, because every case these
//! tests care about -- a known id, an unknown one, and a show id that has no
//! files of its own -- is a *metadata* answer (NFR-205). Everything else on the
//! state comes from `test_support`, so there is no Postgres and no listener.

use std::collections::HashMap;
use std::sync::Arc;

use beam_auth::utils::session_store::SessionData;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;

use crate::models::{MediaMetadata, MediaSource, MovieMetadata, Title};
use crate::routes::media::{browse_media, get_media_detail, get_media_sources};
use crate::routes::test_support::make_app_state;
use crate::services::metadata::{
    MediaConnection, MediaFilter, MediaSearchFilters, MediaSortField, MetadataError,
    MetadataService, PageInfo, SortOrder,
};
use crate::state::{AppServices, AppState};

const MOVIE_ID: &str = "22222222-2222-2222-2222-222222222222";
const SHOW_ID: &str = "44444444-4444-4444-4444-444444444444";
const FILE_ID: &str = "33333333-3333-3333-3333-333333333333";

/// The metadata answers a test wants the handlers to see.
///
/// Bespoke rather than `test_support`'s stub: the endpoints under test *are*
/// the projection of this service onto HTTP, so the interesting cases are the
/// ones only a configurable one can produce.
#[derive(Debug, Default)]
struct StubMetadataService {
    metadata: HashMap<String, MediaMetadata>,
    sources: HashMap<String, Vec<MediaSource>>,
    unsupported: HashMap<String, String>,
}

#[async_trait::async_trait]
impl MetadataService for StubMetadataService {
    async fn get_media_metadata(&self, media_id: &str) -> Option<MediaMetadata> {
        self.metadata.get(media_id).cloned()
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

    async fn get_media_sources(&self, media_id: &str) -> Result<Vec<MediaSource>, MetadataError> {
        // Part of the trait's contract rather than a shortcut: a malformed id
        // is `InvalidId`, which the route owes a 400.
        uuid::Uuid::parse_str(media_id).map_err(|_| MetadataError::InvalidId)?;
        if let Some(msg) = self.unsupported.get(media_id) {
            return Err(MetadataError::Unsupported(msg.clone()));
        }
        self.sources
            .get(media_id)
            .cloned()
            .ok_or(MetadataError::MediaNotFound)
    }
}

/// `test_support`'s state with the metadata service swapped for the one this
/// test configured, so the stubs the media routes never touch stay shared.
fn state_with(metadata: StubMetadataService) -> AppState {
    let base = make_app_state();

    let services = AppServices {
        hash: base.services.hash.clone(),
        library: base.services.library.clone(),
        metadata: Arc::new(metadata),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        playback: base.services.playback.clone(),
        genre_repo: base.services.genre_repo.clone(),
        library_repo: base.services.library_repo.clone(),
        file_repo: base.services.file_repo.clone(),
        enrichment_repo: base.services.enrichment_repo.clone(),
        movie_repo: base.services.movie_repo.clone(),
        show_repo: base.services.show_repo.clone(),
        artwork: base.services.artwork.clone(),
        session_store: base.services.session_store.clone(),
        oidc_client: base.services.oidc_client.clone(),
        pending_auth_store: base.services.pending_auth_store.clone(),
        oidc_config: base.services.oidc_config.clone(),
    };

    AppState::new(base.config.clone(), services, base.probe.clone(), None)
}

fn client(state: AppState) -> TestClient<AppState> {
    let service = Router::new()
        .nest(
            "/v1",
            Router::new().mount(kynos::routes![
                browse_media,
                get_media_detail,
                get_media_sources
            ]),
        )
        .build(state)
        .expect("the media router describes itself");

    TestClient::new(service)
}

/// Issues a session directly, bypassing the OIDC login flow, which is not what
/// these tests are about.
async fn seed_session(state: &AppState) -> String {
    state
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

/// A client and a session cookie for it, for the authenticated cases.
async fn signed_in(metadata: StubMetadataService) -> (TestClient<AppState>, String) {
    let state = state_with(metadata);
    let token = seed_session(&state).await;
    (client(state), token)
}

fn movie_metadata(id: &str, title: &str) -> MediaMetadata {
    MediaMetadata::Movie(MovieMetadata {
        id: id.to_owned(),
        title: Title {
            original: title.to_owned(),
            localized: None,
            alternatives: None,
        },
        description: None,
        year: Some(1999),
        release_date: None,
        runtime: Some(136),
        duration: Some(8160.0),
        poster_url: None,
        backdrop_url: None,
        genres: vec![],
        ratings: None,
        identifiers: None,
        streams: vec![],
        file_id: None,
    })
}

fn movie_source(file_id: &str) -> MediaSource {
    MediaSource {
        file_id: file_id.to_owned(),
        size_bytes: 1_000_000,
        mime_type: Some("video/mp4".to_owned()),
        container_format: Some("mp4".to_owned()),
        duration_secs: Some(8160.0),
        video: None,
        audio_tracks: vec![],
        stream_url: format!("/v1/files/{file_id}/stream"),
        download_url: format!("/v1/files/{file_id}/download"),
    }
}

// ── GET /v1/media/{id} ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_known_id_yields_its_metadata() {
    let mut stub = StubMetadataService::default();
    stub.metadata
        .insert(MOVIE_ID.to_owned(), movie_metadata(MOVIE_ID, "The Matrix"));
    let (client, token) = signed_in(stub).await;

    let response = client
        .get(&format!("/v1/media/{MOVIE_ID}"))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: MediaMetadata = response.json();
    assert_eq!(body.title().original, "The Matrix");
}

#[tokio::test]
async fn an_unknown_id_is_a_404_problem_document() {
    let (client, token) = signed_in(StubMetadataService::default()).await;

    client
        .get(&format!("/v1/media/{MOVIE_ID}"))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#media-not-found");
}

#[tokio::test]
async fn the_detail_route_requires_a_session() {
    let client = client(state_with(StubMetadataService::default()));

    let response = client.get(&format!("/v1/media/{MOVIE_ID}")).send().await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_cookie_naming_no_session_is_not_a_session() {
    let client = client(state_with(StubMetadataService::default()));

    let response = client
        .get(&format!("/v1/media/{MOVIE_ID}"))
        .cookie("beam_session", "not-a-real-token")
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ── GET /v1/media ────────────────────────────────────────────────────────────

#[tokio::test]
async fn browsing_requires_a_session() {
    let client = client(state_with(StubMetadataService::default()));

    let response = client.get("/v1/media").send().await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn browsing_yields_a_connection_and_accepts_the_sort_parameters() {
    let (client, token) = signed_in(StubMetadataService::default()).await;

    let response = client
        .get("/v1/media?sort_by=year&sort_order=desc")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: MediaConnection = response.json();
    assert!(body.edges.is_empty());
    assert!(!body.page_info.has_next_page);
}

// ── GET /v1/media/{id}/sources ───────────────────────────────────────────────

#[tokio::test]
async fn a_playable_id_yields_its_stream_and_download_urls() {
    let mut stub = StubMetadataService::default();
    stub.sources
        .insert(MOVIE_ID.to_owned(), vec![movie_source(FILE_ID)]);
    let (client, token) = signed_in(stub).await;

    let response = client
        .get(&format!("/v1/media/{MOVIE_ID}/sources"))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Vec<MediaSource> = response.json();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0].stream_url, format!("/v1/files/{FILE_ID}/stream"));
    assert_eq!(
        body[0].download_url,
        format!("/v1/files/{FILE_ID}/download")
    );
}

#[tokio::test]
async fn sources_for_an_unknown_id_are_a_404() {
    let (client, token) = signed_in(StubMetadataService::default()).await;

    let response = client
        .get(&format!("/v1/media/{MOVIE_ID}/sources"))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// A show has no files of its own, so asking for its sources is a caller
/// mistake rather than a missing resource: 400, and the caller asks again with
/// an episode id.
#[tokio::test]
async fn sources_for_a_show_id_are_a_400() {
    let mut stub = StubMetadataService::default();
    stub.unsupported.insert(
        SHOW_ID.to_owned(),
        "sources are not available at the show level; use an episode id".to_owned(),
    );
    let (client, token) = signed_in(stub).await;

    client
        .get(&format!("/v1/media/{SHOW_ID}/sources"))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type(
            "https://beam.justinchung.net/reference/errors/#sources-not-available-for-show",
        );
}

/// A malformed media id is a 400, where it used to be a 500.
///
/// The service folded the failed UUID parse into `InternalError`, so a typo in
/// a URL was reported as a server fault on this route while the very same typo
/// on `/v1/media/{id}` answered 404 -- three operations over one resource
/// giving three answers to one condition (issue #123).
#[tokio::test]
async fn sources_for_a_malformed_id_are_a_400_not_a_500() {
    let (client, token) = signed_in(StubMetadataService::default()).await;

    client
        .get("/v1/media/not-a-uuid/sources")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-media-id");
}

#[tokio::test]
async fn the_sources_route_requires_a_session() {
    let client = client(state_with(StubMetadataService::default()));

    let response = client
        .get(&format!("/v1/media/{MOVIE_ID}/sources"))
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
