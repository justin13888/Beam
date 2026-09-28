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

use crate::models::{MediaMetadata, MediaSource, MediaSourceConnection, MovieMetadata, Title};
use crate::routes::media::{browse_media, get_media_detail, get_media_sources};
use crate::routes::test_support::make_app_state;
use crate::services::metadata::{
    BrowseRequest, DbMetadataService, MediaConnection, MetadataError, MetadataRepositories,
    MetadataService, PageInfo,
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
    async fn get_media_metadata(
        &self,
        media_id: uuid::Uuid,
    ) -> Result<Option<MediaMetadata>, MetadataError> {
        Ok(self.metadata.get(&media_id.to_string()).cloned())
    }

    async fn search_media(
        &self,
        _request: BrowseRequest,
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
    state_with_service(Arc::new(metadata))
}

/// [`state_with`] for any metadata service, the real one included.
fn state_with_service(metadata: Arc<dyn MetadataService>) -> AppState {
    let base = make_app_state();

    let services = AppServices {
        hash: base.services.hash.clone(),
        library: base.services.library.clone(),
        metadata,
        subtitles: crate::routes::test_support::idle_subtitles(),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        playback: base.services.playback.clone(),
        genre_repo: base.services.genre_repo.clone(),
        library_repo: base.services.library_repo.clone(),
        file_repo: base.services.file_repo.clone(),
        enrichment_repo: base.services.enrichment_repo.clone(),
        enrichment_control: base.services.enrichment_control.clone(),
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
        id: uuid::Uuid::parse_str(id).expect("a UUID"),
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
        file_id: None,
        source_count: None,
    })
}

fn movie_source(file_id: &str) -> MediaSource {
    MediaSource {
        file_id: uuid::Uuid::parse_str(file_id).expect("a UUID"),
        is_primary: true,
        edition: None,
        episode_span: None,
        size_bytes: 1_000_000,
        mime_type: Some("video/mp4".to_owned()),
        container_format: Some("mp4".to_owned()),
        duration_secs: Some(8160.0),
        video_tracks: vec![],
        audio_tracks: vec![],
        subtitle_tracks: vec![],
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

/// The detail route used to answer this 404: `get_media_metadata` returns an
/// `Option`, and the failed parse was folded into the miss while `/sources`
/// answered the same typo with 400. The parse now happens in the handler, and
/// this is the test that reaches it -- every other detail test uses a
/// well-formed id.
#[tokio::test]
async fn a_malformed_id_on_the_detail_route_is_a_400_not_a_404() {
    let (client, token) = signed_in(StubMetadataService::default()).await;

    client
        .get("/v1/media/not-a-uuid")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-media-id");
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
    assert!(body.items.is_empty());
    assert!(!body.page_info.has_next_page);
}

/// A movie titled `title` with one file behind it, through the same
/// repository calls the indexer makes. Returns the movie's and the file's ids.
async fn indexed_movie(
    movies: &beam_domain::repositories::movie::in_memory::InMemoryMovieRepository,
    files: &beam_domain::repositories::file::in_memory::InMemoryFileRepository,
    title: &str,
) -> (uuid::Uuid, uuid::Uuid) {
    use beam_domain::models::{
        CreateMediaFile, CreateMovie, CreateMovieEntry, FileStatus, MediaFileContent,
    };
    use beam_domain::repositories::{FileRepository, MovieRepository};

    let library_id = uuid::Uuid::new_v4();
    let movie = movies
        .find_or_create_by_identity(CreateMovie::new(title, None, None))
        .await
        .unwrap();
    let entry = movies
        .find_or_create_entry(CreateMovieEntry {
            library_id,
            movie_id: movie.id,
            edition: None,
        })
        .await
        .unwrap();
    let file = files
        .create(CreateMediaFile {
            library_id,
            path: std::path::PathBuf::from(format!("/videos/{title}.mkv")),
            hash: movie.id.as_u128() as u64,
            size_bytes: 1024,
            mtime: None,
            identity: None,
            mime_type: Some("video/x-matroska".to_string()),
            duration: None,
            container_format: None,
            content: Some(MediaFileContent::Movie {
                movie_entry_id: entry.id,
            }),
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
        })
        .await
        .unwrap();
    (movie.id, file.id)
}

/// Browse lists a title only while one of its files is present (issue #183):
/// the moment its only file goes missing it leaves the listing, without
/// waiting for a purge -- yet its own detail route still answers, so a
/// bookmark or a continue-watching tile does not dangle while the file is
/// away. Real metadata service, real repository doubles linked to one file
/// store.
#[tokio::test]
async fn browse_omits_a_title_whose_only_file_is_missing_but_its_detail_still_resolves() {
    use beam_domain::repositories::FileRepository;
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;

    let files = Arc::new(InMemoryFileRepository::default());
    let movies = Arc::new(InMemoryMovieRepository::with_files(files.clone()));
    let (present, _) = indexed_movie(&movies, &files, "Arrival").await;
    let (away, away_file) = indexed_movie(&movies, &files, "Contact").await;
    files
        .mark_missing(vec![away_file], chrono::Utc::now())
        .await
        .unwrap();

    let state = state_with_service(real_service(Library {
        movies: movies.clone(),
        shows: Arc::new(InMemoryShowRepository::with_files(files.clone())),
        genres: Arc::default(),
        streams: Arc::default(),
        sidecars: Arc::default(),
        files: files.clone(),
    }));
    let token = seed_session(&state).await;
    let client = client(state);

    let response = client
        .get("/v1/media")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: MediaConnection = response.json();
    let listed: Vec<uuid::Uuid> = body.items.iter().map(item_id).collect();
    assert_eq!(listed, vec![present]);

    let detail = client
        .get(&format!("/v1/media/{away}"))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(
        detail.status(),
        StatusCode::OK,
        "the hidden title still resolves"
    );

    // The file coming back puts the title back in the listing.
    files.restore(away_file).await.unwrap();
    let again: MediaConnection = client
        .get("/v1/media")
        .cookie("beam_session", &token)
        .send()
        .await
        .json();
    assert_eq!(again.items.len(), 2);
}

/// The doubles a real metadata service reads, linked to one file store so
/// liveness is what the files say.
struct Library {
    files: Arc<beam_domain::repositories::file::in_memory::InMemoryFileRepository>,
    movies: Arc<beam_domain::repositories::movie::in_memory::InMemoryMovieRepository>,
    shows: Arc<beam_domain::repositories::show::in_memory::InMemoryShowRepository>,
    genres: Arc<beam_domain::repositories::genre::in_memory::InMemoryGenreRepository>,
    streams: Arc<beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository>,
    sidecars: Arc<
        beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository,
    >,
}

impl Library {
    fn new() -> Self {
        let files =
            Arc::new(beam_domain::repositories::file::in_memory::InMemoryFileRepository::default());
        Self {
            movies: Arc::new(
                beam_domain::repositories::movie::in_memory::InMemoryMovieRepository::with_files(
                    files.clone(),
                ),
            ),
            shows: Arc::new(
                beam_domain::repositories::show::in_memory::InMemoryShowRepository::with_files(
                    files.clone(),
                ),
            ),
            genres: Arc::default(),
            streams: Arc::default(),
            sidecars: Arc::default(),
            files,
        }
    }

    /// A show titled `title` with one episode file, enriched with a rating
    /// and identifiers, and tagged with genres.
    async fn indexed_show(&self, title: &str) -> uuid::Uuid {
        use beam_domain::models::{
            CreateEpisode, CreateMediaFile, CreateShow, FileStatus, MediaFileContent,
        };
        use beam_domain::providers::enrichment::ShowEnrichment;
        use beam_domain::repositories::{FileRepository, GenreRepository, ShowRepository};

        let show = self
            .shows
            .find_or_create_by_identity(CreateShow::new(title, Some(2022)))
            .await
            .unwrap();
        self.shows
            .apply_enrichment(
                show.id,
                &ShowEnrichment {
                    title: title.to_string(),
                    year: Some(2022),
                    tmdb_id: Some(95396),
                    rating: Some(8.4),
                    ..Default::default()
                },
                &beam_domain::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
        let season = self.shows.find_or_create_season(show.id, 1).await.unwrap();
        let episode = self
            .shows
            .find_or_create_episode(CreateEpisode {
                season_id: season.id,
                episode_number: 1,
                title: "Pilot".to_string(),
                runtime: None,
                air_date: None,
            })
            .await
            .unwrap();
        self.files
            .create(CreateMediaFile {
                library_id: uuid::Uuid::new_v4(),
                path: std::path::PathBuf::from(format!("/videos/{title}/S01E01.mkv")),
                hash: show.id.as_u128() as u64,
                size_bytes: 1024,
                mtime: None,
                identity: None,
                mime_type: None,
                duration: None,
                container_format: None,
                content: Some(MediaFileContent::episode(episode.id)),
                status: FileStatus::Known,
                classifier_version: 0,
                container_tags: None,
            })
            .await
            .unwrap();
        self.genres
            .set_show_genres(show.id, &["Thriller".to_string()])
            .await
            .unwrap();
        show.id
    }
}

/// The real metadata service over `library`, its catalogue reading the same
/// doubles.
fn real_service(library: Library) -> Arc<dyn MetadataService> {
    real_service_over(
        &library,
        Arc::new(
            beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository::new(
                library.movies.clone(),
                library.shows.clone(),
                library.genres.clone(),
            ),
        ),
        library.movies.clone(),
    )
}

fn real_service_over(
    library: &Library,
    catalog: Arc<dyn beam_domain::repositories::CatalogRepository>,
    movies: Arc<dyn beam_domain::repositories::MovieRepository>,
) -> Arc<dyn MetadataService> {
    Arc::new(DbMetadataService::new(MetadataRepositories {
        sources: Arc::new(crate::services::sources::SourceCatalog::new(
            movies.clone(),
            library.files.clone(),
            library.streams.clone(),
            library.sidecars.clone(),
        )),
        movies,
        shows: library.shows.clone(),
        catalog,
        genres: library.genres.clone(),
    }))
}

fn item_id(item: &MediaMetadata) -> uuid::Uuid {
    match item {
        MediaMetadata::Movie(movie) => movie.id,
        MediaMetadata::Show(show) => show.id,
    }
}

async fn signed_in_to(service: Arc<dyn MetadataService>) -> (TestClient<AppState>, String) {
    let state = state_with_service(service);
    let token = seed_session(&state).await;
    (client(state), token)
}

/// Following `end_cursor` from the first page visits every title once, in
/// order, and the last page says there is nothing after it.
#[tokio::test]
async fn following_end_cursor_visits_every_title_once_in_order() {
    let library = Library::new();
    let mut expected = Vec::new();
    for title in ["Arrival", "Blade", "Contact"] {
        expected.push(
            indexed_movie(&library.movies, &library.files, title)
                .await
                .0,
        );
    }
    expected.push(library.indexed_show("Dark").await);
    let (client, token) = signed_in_to(real_service(library)).await;

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let url = match &after {
            Some(cursor) => format!("/v1/media?first=3&after={cursor}"),
            None => "/v1/media?first=3".to_string(),
        };
        let response = client.get(&url).cookie("beam_session", &token).send().await;
        assert_eq!(response.status(), StatusCode::OK);
        let page: MediaConnection = response.json();
        assert_eq!(page.page_info.has_previous_page, after.is_some());
        seen.extend(page.items.iter().map(item_id));
        if !page.page_info.has_next_page {
            break;
        }
        after = page.page_info.end_cursor;
    }
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn a_cursor_the_server_did_not_issue_is_a_400_invalid_cursor() {
    let (client, token) = signed_in_to(real_service(Library::new())).await;

    client
        .get("/v1/media?after=bm90LWEtY3Vyc29y")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-cursor");
}

/// Kynos percent-decodes `%00` into a NUL, which Postgres cannot bind as
/// text: it used to fail the catalogue statement and answer 500 `#internal`.
#[tokio::test]
async fn a_search_text_holding_nul_is_a_400_invalid_search_query() {
    let (client, token) = signed_in_to(real_service(Library::new())).await;

    for query in ["query=%00", "query=a%00b"] {
        client
            .get(&format!("/v1/media?{query}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type(
                "https://beam.justinchung.net/reference/errors/#invalid-search-query",
            );
    }
}

#[tokio::test]
async fn a_page_the_server_does_not_answer_is_a_400_invalid_pagination() {
    let (client, token) = signed_in_to(real_service(Library::new())).await;

    for query in [
        "first=101",
        "first=0",
        "last=101",
        "first=2&last=2",
        "after=a&before=b",
    ] {
        client
            .get(&format!("/v1/media?{query}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type(
                "https://beam.justinchung.net/reference/errors/#invalid-pagination",
            );
    }
}

/// NFR-205: a database failure while browsing answered 200 with an empty
/// page, which a client renders as "your library is empty".
#[tokio::test]
async fn a_database_failure_while_browsing_is_a_500_not_an_empty_page() {
    let library = Library::new();
    let mut catalog = beam_domain::repositories::catalog::MockCatalogRepository::new();
    catalog
        .expect_browse()
        .returning(|_| Err(sea_orm::DbErr::Custom("connection reset".to_string())));
    let movies = library.movies.clone();
    let (client, token) =
        signed_in_to(real_service_over(&library, Arc::new(catalog), movies)).await;

    client
        .get("/v1/media")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
}

/// NFR-205: a database failure reading a title answered 404, telling the
/// client the title was gone when the server had only failed to read it.
#[tokio::test]
async fn a_database_failure_reading_a_title_is_a_500_not_a_404() {
    let library = Library::new();
    let mut movies = beam_domain::repositories::movie::MockMovieRepository::new();
    movies
        .expect_find_by_id()
        .returning(|_| Err(sea_orm::DbErr::Custom("connection reset".to_string())));
    let catalog = Arc::new(beam_domain::repositories::catalog::MockCatalogRepository::new());
    let (client, token) =
        signed_in_to(real_service_over(&library, catalog, Arc::new(movies))).await;

    client
        .get(&format!("/v1/media/{MOVIE_ID}"))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
}

/// What a browse tile needs of a show is on the wire: genres, rating,
/// identifiers and counts -- read from the JSON itself, so a field the server
/// computes but never serializes still fails.
#[tokio::test]
async fn a_browsed_show_carries_its_genres_rating_identifiers_and_counts_on_the_wire() {
    let library = Library::new();
    let id = library.indexed_show("Severance").await;
    let (client, token) = signed_in_to(real_service(library)).await;

    let response = client
        .get("/v1/media?media_type=show&genre=thriller")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json();
    let show = &body["items"][0]["Show"];
    assert_eq!(show["id"], id.to_string());
    assert_eq!(show["genres"], serde_json::json!(["Thriller"]));
    assert_eq!(show["ratings"]["tmdb"], 84);
    assert_eq!(show["identifiers"]["tmdb_id"], 95396);
    assert_eq!(show["season_count"], 1);
    assert_eq!(show["episode_count"], 1);
    assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
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
    let MediaSourceConnection { items, page_info } = response.json();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].stream_url, format!("/v1/files/{FILE_ID}/stream"));
    assert_eq!(
        items[0].download_url,
        format!("/v1/files/{FILE_ID}/download")
    );
    // One complete page: nothing before it, nothing after.
    assert!(!page_info.has_next_page);
    assert!(!page_info.has_previous_page);
}

/// What a player reads off the wire (issue #189): each source says whether
/// it is the primary, and carries its tracks -- the subtitle files beside it
/// among them, with where to fetch each -- in the JSON a generated client
/// reads. Real metadata service; the tracks come from the index doubles.
#[tokio::test]
async fn sources_carry_their_tracks_and_the_subtitle_files_beside_them() {
    use beam_domain::models::CreateMediaStream;
    use beam_domain::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
    use beam_domain::models::stream::{StreamMetadata, StreamType, SubtitleStreamMetadata};
    use beam_domain::repositories::{MediaStreamRepository, SidecarSubtitleRepository};

    let library = Library::new();
    let (movie, file) = indexed_movie(&library.movies, &library.files, "Arrival").await;
    library
        .streams
        .insert_streams(vec![CreateMediaStream {
            file_id: file,
            index: 2,
            stream_type: StreamType::Subtitle,
            codec: "hdmv_pgs_subtitle".to_string(),
            metadata: StreamMetadata::Subtitle(SubtitleStreamMetadata {
                language: Some("eng".to_string()),
                title: None,
                is_default: false,
                is_forced: false,
                is_hearing_impaired: false,
            }),
        }])
        .await
        .unwrap();
    let sidecar = library
        .sidecars
        .upsert_by_path(UpsertSidecarSubtitle {
            file_id: file,
            library_id: uuid::Uuid::new_v4(),
            path: std::path::PathBuf::from("/videos/Arrival.en.sdh.srt"),
            info: SidecarInfo {
                format: SubtitleFormat::Srt,
                language: Some("eng".to_string()),
                title: None,
                is_forced: false,
                is_sdh: true,
                is_default: false,
            },
            size_bytes: 40_000,
            mtime: None,
        })
        .await
        .unwrap();
    let (client, token) = signed_in_to(real_service(library)).await;

    let response = client
        .get(&format!("/v1/media/{movie}/sources"))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json();
    assert_eq!(
        body["page_info"],
        serde_json::json!({
            "has_next_page": false,
            "has_previous_page": false,
            "start_cursor": null,
            "end_cursor": null,
        })
    );
    let source = &body["items"][0];
    assert_eq!(source["file_id"], file.to_string());
    assert_eq!(source["is_primary"], true);
    assert_eq!(source["edition"], serde_json::Value::Null);
    let subtitles = source["subtitle_tracks"].as_array().expect("an array");
    assert_eq!(
        subtitles[0],
        serde_json::json!({
            "origin": "embedded",
            "index": 2,
            "sidecar_id": null,
            "codec": "hdmv_pgs_subtitle",
            "language": "eng",
            "title": null,
            "is_default": false,
            "is_forced": false,
            "is_hearing_impaired": false,
            "is_text": false,
            "url": null,
            "webvtt_url": null,
        })
    );
    let id = sidecar.id;
    assert_eq!(
        subtitles[1],
        serde_json::json!({
            "origin": "sidecar",
            "index": null,
            "sidecar_id": id.to_string(),
            "codec": "subrip",
            "language": "eng",
            "title": null,
            "is_default": false,
            "is_forced": false,
            "is_hearing_impaired": true,
            "is_text": true,
            "url": format!("/v1/files/{file}/subtitles/{id}"),
            "webvtt_url": format!("/v1/files/{file}/subtitles/{id}/webvtt"),
        })
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
