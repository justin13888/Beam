//! Subcutaneous tests for the `/v1/media` detail and sources routes, and the
//! fixtures the browse tests in `media_browse_tests.rs` share.
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

use crate::models::search::{MediaConnection, PageInfo};
use crate::models::{MediaMetadata, MediaSource, MediaSourceConnection, MovieMetadata, Title};
use crate::routes::media::{browse_media, get_media_detail, get_media_sources};
use crate::routes::test_support::make_app_state;
use crate::services::metadata::{
    BrowseRequest, DbMetadataService, MetadataError, MetadataRepositories, MetadataService,
};
use crate::state::{AppServices, AppState};

pub(super) const MOVIE_ID: &str = "22222222-2222-2222-2222-222222222222";
const SHOW_ID: &str = "44444444-4444-4444-4444-444444444444";
const FILE_ID: &str = "33333333-3333-3333-3333-333333333333";

/// The metadata answers a test wants the handlers to see.
///
/// Bespoke rather than `test_support`'s stub: the endpoints under test *are*
/// the projection of this service onto HTTP, so the interesting cases are the
/// ones only a configurable one can produce.
#[derive(Debug, Default)]
pub(super) struct StubMetadataService {
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

    async fn get_media_sources(
        &self,
        media_id: uuid::Uuid,
    ) -> Result<Vec<MediaSource>, MetadataError> {
        let media_id = media_id.to_string();
        if let Some(msg) = self.unsupported.get(&media_id) {
            return Err(MetadataError::Unsupported(msg.clone()));
        }
        self.sources
            .get(&media_id)
            .cloned()
            .ok_or(MetadataError::MediaNotFound)
    }

    async fn get_episode_detail(
        &self,
        _episode_id: uuid::Uuid,
    ) -> Result<Option<crate::models::EpisodeDetail>, MetadataError> {
        Ok(None)
    }

    async fn get_season_detail(
        &self,
        _season_id: uuid::Uuid,
    ) -> Result<Option<crate::models::SeasonDetail>, MetadataError> {
        Ok(None)
    }

    async fn get_titles(
        &self,
        _titles: &[beam_domain::models::watch_state::TitleRef],
    ) -> Result<Vec<crate::models::MediaMetadata>, MetadataError> {
        Ok(vec![])
    }
}

/// `test_support`'s state with the metadata service swapped for the one this
/// test configured, so the stubs the media routes never touch stay shared.
pub(super) fn state_with(metadata: StubMetadataService) -> AppState {
    state_with_service(Arc::new(metadata))
}

/// [`state_with`] for any metadata service, the real one included.
pub(super) fn state_with_service(metadata: Arc<dyn MetadataService>) -> AppState {
    state_over(make_app_state(), metadata)
}

/// `base` with `metadata` as its metadata service, every other seam shared.
fn state_over(base: AppState, metadata: Arc<dyn MetadataService>) -> AppState {
    let access = base.services.access.clone();
    state_over_access(base, metadata, access)
}

/// [`state_over`] with the access service chosen too.
pub(super) fn state_over_access(
    base: AppState,
    metadata: Arc<dyn MetadataService>,
    access: Arc<crate::services::access::AccessService>,
) -> AppState {
    let services = AppServices {
        access,
        metadata,
        ..base.services.clone()
    };

    AppState::new(base.config.clone(), services, base.probe.clone(), None)
}

pub(super) fn client(state: AppState) -> TestClient<AppState> {
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
pub(super) async fn seed_session(state: &AppState) -> String {
    session_for(state, uuid::Uuid::new_v4()).await
}

/// A session for the user `user_id`.
async fn session_for(state: &AppState, user_id: uuid::Uuid) -> String {
    state
        .services
        .session_store
        .create(
            &SessionData {
                user_id: user_id.to_string(),
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
        runtime_mins: Some(136),
        duration_secs: Some(8160.0),
        poster_url: None,
        backdrop_url: None,
        genres: vec![],
        ratings: None,
        content_rating: None,
        identifiers: None,
        file_id: None,
        source_count: None,
        user_state: crate::models::UserTitleState::default(),
        cast: None,
        crew: None,
        collection: None,
    })
}

fn movie_source(file_id: &str) -> MediaSource {
    MediaSource {
        file_id: uuid::Uuid::parse_str(file_id).expect("a UUID"),
        is_primary: true,
        edition: None,
        episode_span: None,
        disc_structure: None,
        parts: vec![crate::models::SourcePart {
            file_id: uuid::Uuid::parse_str(file_id).expect("a UUID"),
            part_number: None,
            size_bytes: 1_000_000,
            duration_secs: Some(8160.0),
            subtitle_tracks: vec![],
            default_subtitle_track_index: None,
            chapters: vec![],
            markers: vec![],
            trickplay: None,
            stream_url: format!("/v1/files/{file_id}/stream"),
            download_url: format!("/v1/files/{file_id}/download"),
        }],
        size_bytes: 1_000_000,
        mime_type: Some("video/mp4".to_owned()),
        container_format: Some("mp4".to_owned()),
        duration_secs: Some(8160.0),
        video_tracks: vec![],
        audio_tracks: vec![],
        subtitle_tracks: vec![],
        default_audio_track_index: None,
        default_subtitle_track_index: None,
        chapters: vec![],
        markers: vec![],
        trickplay: None,
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

/// A malformed id is a 400 on both routes over one title, answered by the
/// `Path` extractor before the handler runs -- the detail route once folded
/// it into the 404 and `/sources` into a 500 (issue #123). `/v1/media/{id}`
/// is typed `Uuid`, so the two cannot drift apart again.
#[tokio::test]
async fn a_malformed_id_is_a_400_on_the_detail_and_sources_routes() {
    let (client, token) = signed_in(StubMetadataService::default()).await;

    for path in ["/v1/media/not-a-uuid", "/v1/media/not-a-uuid/sources"] {
        let response = client.get(path).cookie("beam_session", &token).send().await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
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

// ── The real metadata service, shared with `media_browse_tests` ─────────────

/// A movie titled `title` with one file behind it, through the same
/// repository calls the indexer makes. Returns the movie's and the file's ids.
pub(super) async fn indexed_movie(
    movies: &beam_domain::repositories::movie::in_memory::InMemoryMovieRepository,
    files: &beam_domain::repositories::file::in_memory::InMemoryFileRepository,
    title: &str,
) -> (uuid::Uuid, uuid::Uuid) {
    indexed_movie_of(movies, files, title, None).await
}

/// [`indexed_movie`], released in `year`.
pub(super) async fn indexed_movie_of(
    movies: &beam_domain::repositories::movie::in_memory::InMemoryMovieRepository,
    files: &beam_domain::repositories::file::in_memory::InMemoryFileRepository,
    title: &str,
    year: Option<u32>,
) -> (uuid::Uuid, uuid::Uuid) {
    use beam_domain::models::{
        CreateMediaFile, CreateMovie, CreateMovieEntry, FileStatus, MediaFileContent,
    };
    use beam_domain::repositories::{FileRepository, MovieRepository};
    use beam_domain::test_utils::create_media_file;

    let library_id = uuid::Uuid::new_v4();
    let movie = movies
        .find_or_create_by_identity(CreateMovie::new(title, year, None))
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
            content: Some(MediaFileContent::movie(entry.id)),
            status: FileStatus::Known,
            ..create_media_file(library_id, format!("/videos/{title}.mkv"))
        })
        .await
        .unwrap();
    (movie.id, file.id)
}

/// The doubles a real metadata service reads, linked to one file store so
/// liveness is what the files say.
pub(super) struct Library {
    pub(super) files: Arc<beam_domain::repositories::file::in_memory::InMemoryFileRepository>,
    pub(super) movies: Arc<beam_domain::repositories::movie::in_memory::InMemoryMovieRepository>,
    pub(super) shows: Arc<beam_domain::repositories::show::in_memory::InMemoryShowRepository>,
    pub(super) genres: Arc<beam_domain::repositories::genre::in_memory::InMemoryGenreRepository>,
    pub(super) people: Arc<beam_domain::repositories::person::in_memory::InMemoryPersonRepository>,
    pub(super) streams:
        Arc<beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository>,
    pub(super) sidecars: Arc<
        beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository,
    >,
    pub(super) chapters:
        Arc<beam_domain::repositories::chapter::in_memory::InMemoryChapterRepository>,
    pub(super) trickplay:
        Arc<beam_domain::repositories::trickplay::in_memory::InMemoryTrickplayRepository>,
}

impl Library {
    pub(super) fn new() -> Self {
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
            people: Arc::default(),
            streams: Arc::default(),
            sidecars: Arc::default(),
            chapters: Arc::default(),
            trickplay: Arc::default(),
            files,
        }
    }

    /// A show titled `title` with one episode file, enriched with a rating
    /// and identifiers, and tagged with genres.
    pub(super) async fn indexed_show(&self, title: &str) -> uuid::Uuid {
        use beam_domain::models::{
            CreateEpisode, CreateMediaFile, CreateShow, FileStatus, MediaFileContent,
        };
        use beam_domain::providers::enrichment::ShowEnrichment;
        use beam_domain::repositories::{FileRepository, GenreRepository, ShowRepository};
        use beam_domain::test_utils::create_media_file;

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
                content: Some(MediaFileContent::episode(episode.id)),
                status: FileStatus::Known,
                ..create_media_file(uuid::Uuid::new_v4(), format!("/videos/{title}/S01E01.mkv"))
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
pub(super) fn real_service(library: Library) -> Arc<dyn MetadataService> {
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

pub(super) fn real_service_over(
    library: &Library,
    catalog: Arc<dyn beam_domain::repositories::CatalogRepository>,
    movies: Arc<dyn beam_domain::repositories::MovieRepository>,
) -> Arc<dyn MetadataService> {
    Arc::new(DbMetadataService::new(MetadataRepositories {
        sources: Arc::new(crate::services::sources::SourceCatalog::new(
            crate::services::sources::SourceRepositories {
                movies: movies.clone(),
                files: library.files.clone(),
                streams: library.streams.clone(),
                sidecars: library.sidecars.clone(),
                chapters: library.chapters.clone(),
                trickplay: library.trickplay.clone(),
            },
        )),
        movies,
        shows: library.shows.clone(),
        catalog,
        genres: library.genres.clone(),
        people: library.people.clone(),
        collections: Arc::new(
            beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository::default(
            ),
        ),
    }))
}

pub(super) async fn signed_in_to(
    service: Arc<dyn MetadataService>,
) -> (TestClient<AppState>, String) {
    let state = state_with_service(service);
    let token = seed_session(&state).await;
    (client(state), token)
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
    // The service's sources, as it gave them: URLs and all. That those URLs
    // are served routes is `access_tests`'
    // `every_url_a_source_carries_is_served_and_delivers_what_it_names`.
    assert_eq!(
        serde_json::to_value(&items).unwrap(),
        serde_json::to_value(vec![movie_source(FILE_ID)]).unwrap()
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
    // Where to fetch the file, as stored and as WebVTT: two distinct URLs.
    // That each is a served route delivering this subtitle is `access_tests`'
    // `every_url_a_source_carries_is_served_and_delivers_what_it_names`.
    let mut sidecar_track = subtitles[1].clone();
    let fields = sidecar_track.as_object_mut().expect("an object");
    let url = fields.remove("url").expect("a url field");
    let webvtt_url = fields.remove("webvtt_url").expect("a webvtt_url field");
    assert!(
        url.is_string() && webvtt_url.is_string() && url != webvtt_url,
        "{url} / {webvtt_url}"
    );
    assert_eq!(
        sidecar_track,
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
        })
    );
}

/// A source carries its file's chapters, the intro and credits spans the
/// chapters titled for them mark, and -- once Beam has made them for the
/// file as it is -- its trickplay sheets, with where to fetch each (issue
/// #265). The first part's on the source, and each part's on the part.
#[tokio::test]
async fn sources_carry_their_chapters_markers_and_trickplay_sheets() {
    use beam_domain::models::chapter::Chapter as StoredChapter;
    use beam_domain::models::trickplay::{TrickplayLayout, TrickplayRecord};
    use beam_domain::repositories::{ChapterRepository, FileRepository, TrickplayRepository};

    let library = Library::new();
    let (movie, file) = indexed_movie(&library.movies, &library.files, "Arrival").await;
    let hash = library.files.find_by_id(file).await.unwrap().unwrap().hash;
    library
        .chapters
        .replace_for_file(
            file,
            [
                (0, 20_000, Some("Previously On")),
                (20_000, 95_000, Some("Opening Credits")),
                (95_000, 6_900_000, None),
                (6_900_000, 7_000_000, Some("End Credits")),
                (7_000_000, 7_030_000, Some("Next Episode")),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (start_ms, end_ms, title))| StoredChapter {
                index: index as u32,
                start_ms,
                end_ms,
                title: title.map(str::to_owned),
            })
            .collect(),
        )
        .await
        .unwrap();
    library
        .trickplay
        .upsert(TrickplayRecord {
            file_id: file,
            file_hash: hash,
            layout: Some(TrickplayLayout {
                interval_ms: 10_000,
                tile_width: 320,
                tile_height: 134,
                columns: 10,
                rows: 10,
                tile_count: 150,
            }),
            generated_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    let (client, token) = signed_in_to(real_service(library)).await;

    let body: serde_json::Value = client
        .get(&format!("/v1/media/{movie}/sources"))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    let source = &body["items"][0];
    assert_eq!(
        source["chapters"],
        serde_json::json!([
            {"start_ms": 0, "end_ms": 20_000, "title": "Previously On"},
            {"start_ms": 20_000, "end_ms": 95_000, "title": "Opening Credits"},
            {"start_ms": 95_000, "end_ms": 6_900_000, "title": null},
            {"start_ms": 6_900_000, "end_ms": 7_000_000, "title": "End Credits"},
            {"start_ms": 7_000_000, "end_ms": 7_030_000, "title": "Next Episode"},
        ])
    );
    assert_eq!(
        source["markers"],
        serde_json::json!([
            {"kind": "recap", "start_ms": 0, "end_ms": 20_000},
            {"kind": "intro", "start_ms": 20_000, "end_ms": 95_000},
            {"kind": "credits", "start_ms": 6_900_000, "end_ms": 7_000_000},
            {"kind": "preview", "start_ms": 7_000_000, "end_ms": 7_030_000},
        ])
    );
    // A URL per sheet: 150 tiles at 10 by 10 are two. That each is a served
    // route delivering its own sheet is `access_tests`'
    // `every_url_a_source_carries_is_served_and_delivers_what_it_names`.
    let mut trickplay = source["trickplay"].clone();
    let sheet_urls = trickplay
        .as_object_mut()
        .expect("an object")
        .remove("sheet_urls")
        .expect("a sheet_urls field");
    let sheet_urls = sheet_urls.as_array().expect("an array");
    assert_eq!(sheet_urls.len(), 2);
    assert!(
        sheet_urls[0].is_string() && sheet_urls[1].is_string() && sheet_urls[0] != sheet_urls[1],
        "{sheet_urls:?}"
    );
    assert_eq!(
        trickplay,
        serde_json::json!({
            "interval_ms": 10_000,
            "tile_width": 320,
            "tile_height": 134,
            "columns": 10,
            "rows": 10,
            "tile_count": 150,
        })
    );
    let part = &source["parts"][0];
    for field in ["chapters", "markers", "trickplay"] {
        assert_eq!(part[field], source[field], "the part's own {field}");
    }
}

/// Sheets made from a file's earlier content describe a picture that is no
/// longer there, and a file that gave none has none: neither is offered.
#[tokio::test]
async fn sources_offer_no_trickplay_made_from_other_content_or_none_at_all() {
    use beam_domain::models::trickplay::{TrickplayLayout, TrickplayRecord};
    use beam_domain::repositories::{FileRepository, TrickplayRepository};

    let library = Library::new();
    let (stale_movie, stale_file) = indexed_movie(&library.movies, &library.files, "Blade").await;
    let (empty_movie, empty_file) = indexed_movie(&library.movies, &library.files, "Contact").await;
    let stale_hash = library
        .files
        .find_by_id(stale_file)
        .await
        .unwrap()
        .unwrap()
        .hash;
    let empty_hash = library
        .files
        .find_by_id(empty_file)
        .await
        .unwrap()
        .unwrap()
        .hash;
    for (file_id, file_hash, layout) in [
        (
            stale_file,
            stale_hash.wrapping_add(1),
            Some(TrickplayLayout {
                interval_ms: 10_000,
                tile_width: 320,
                tile_height: 180,
                columns: 10,
                rows: 10,
                tile_count: 5,
            }),
        ),
        (empty_file, empty_hash, None),
    ] {
        library
            .trickplay
            .upsert(TrickplayRecord {
                file_id,
                file_hash,
                layout,
                generated_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
    }
    let (client, token) = signed_in_to(real_service(library)).await;

    for movie in [stale_movie, empty_movie] {
        let body: serde_json::Value = client
            .get(&format!("/v1/media/{movie}/sources"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::OK)
            .json();
        let source = &body["items"][0];
        assert_eq!(source["trickplay"], serde_json::Value::Null);
        assert_eq!(source["parts"][0]["trickplay"], serde_json::Value::Null);
        assert_eq!(source["chapters"], serde_json::json!([]));
        assert_eq!(source["markers"], serde_json::json!([]));
    }
}

/// Each source names the tracks to start on for the caller (issue #264):
/// the file's own choices for a caller who set nothing, their languages
/// once they have, and someone else's preferences never.
#[tokio::test]
async fn sources_name_the_callers_default_tracks() {
    use beam_domain::models::CreateMediaStream;
    use beam_domain::models::language_preferences::SubtitleMode;
    use beam_domain::models::language_preferences::test_utils::prefs;
    use beam_domain::models::stream::{
        AudioStreamMetadata, StreamMetadata, StreamType, SubtitleStreamMetadata,
    };
    use beam_domain::repositories::MediaStreamRepository;

    let library = Library::new();
    let (movie, file) = indexed_movie(&library.movies, &library.files, "Arrival").await;
    let audio = |index: u32, language: &str, is_default: bool| CreateMediaStream {
        file_id: file,
        index,
        stream_type: StreamType::Audio,
        codec: "aac".to_string(),
        metadata: StreamMetadata::Audio(AudioStreamMetadata {
            language: Some(language.to_string()),
            title: None,
            channels: 2,
            sample_rate: 48_000,
            channel_layout: None,
            bit_rate: None,
            is_default,
            is_forced: false,
        }),
    };
    let subtitle = |index: u32, language: &str, is_forced: bool| CreateMediaStream {
        file_id: file,
        index,
        stream_type: StreamType::Subtitle,
        codec: "subrip".to_string(),
        metadata: StreamMetadata::Subtitle(SubtitleStreamMetadata {
            language: Some(language.to_string()),
            title: None,
            is_default: false,
            is_forced,
            is_hearing_impaired: false,
        }),
    };
    library
        .streams
        .insert_streams(vec![
            audio(1, "eng", true),
            audio(2, "jpn", false),
            subtitle(3, "eng", true),
            subtitle(4, "eng", false),
            subtitle(5, "jpn", true),
        ])
        .await
        .unwrap();
    let state = state_with_service(real_service(library));
    let viewer = uuid::Uuid::new_v4();
    let token = session_for(&state, viewer).await;
    let other = session_for(&state, uuid::Uuid::new_v4()).await;
    let client = client(state.clone());
    let defaults = |token: String| {
        let client = &client;
        async move {
            let body: serde_json::Value = client
                .get(&format!("/v1/media/{movie}/sources"))
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::OK)
                .json();
            let source = &body["items"][0];
            (
                source["default_audio_track_index"].clone(),
                source["default_subtitle_track_index"].clone(),
                source["parts"][0]["default_subtitle_track_index"].clone(),
            )
        }
    };

    // Nothing set: the file's default audio, and the forced English track
    // beside it.
    assert_eq!(
        defaults(token.clone()).await,
        (
            serde_json::json!(0),
            serde_json::json!(0),
            serde_json::json!(0)
        )
    );

    state
        .services
        .language_preferences
        .replace(viewer, &prefs(&["ja"], &["eng"], SubtitleMode::On))
        .await
        .unwrap();
    assert_eq!(
        defaults(token.clone()).await,
        (
            serde_json::json!(1),
            serde_json::json!(1),
            serde_json::json!(1)
        ),
        "Japanese audio, full English subtitles"
    );

    state
        .services
        .language_preferences
        .replace(viewer, &prefs(&["jpn"], &[], SubtitleMode::Off))
        .await
        .unwrap();
    assert_eq!(
        defaults(token).await,
        (
            serde_json::json!(1),
            serde_json::Value::Null,
            serde_json::Value::Null
        ),
        "subtitles off"
    );
    assert_eq!(
        defaults(other).await,
        (
            serde_json::json!(0),
            serde_json::json!(0),
            serde_json::json!(0)
        ),
        "another viewer keeps the defaults"
    );
}

/// Each part of a multi-part source names its subtitle default from its own
/// subtitle files (issue #264): the second disc carries a Danish file before
/// its English one, so its English track sits at a position the first
/// disc's does not, and copying the source's index would name the Danish.
#[tokio::test]
async fn each_part_names_its_default_subtitle_from_its_own_tracks() {
    use beam_domain::models::language_preferences::SubtitleMode;
    use beam_domain::models::language_preferences::test_utils::prefs;
    use beam_domain::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
    use beam_domain::models::{
        CreateMediaFile, CreateMovie, CreateMovieEntry, FileStatus, MediaFileContent,
    };
    use beam_domain::repositories::{FileRepository, MovieRepository, SidecarSubtitleRepository};
    use beam_domain::test_utils::create_media_file;

    let library = Library::new();
    let library_id = uuid::Uuid::new_v4();
    let movie = library
        .movies
        .find_or_create_by_identity(CreateMovie::new("Heat", None, None))
        .await
        .unwrap();
    let entry = library
        .movies
        .find_or_create_entry(CreateMovieEntry {
            library_id,
            movie_id: movie.id,
            edition: None,
        })
        .await
        .unwrap();
    let mut parts = Vec::new();
    for part_number in [1_u32, 2] {
        let file = library
            .files
            .create(CreateMediaFile {
                content: Some(MediaFileContent::Movie {
                    movie_entry_id: entry.id,
                    part_number: Some(part_number),
                }),
                status: FileStatus::Known,
                ..create_media_file(
                    library_id,
                    format!("/videos/Heat/Heat - CD{part_number}.mkv"),
                )
            })
            .await
            .unwrap();
        parts.push(file.id);
    }
    let sidecar = |file_id: uuid::Uuid, name: &str, language: &str| UpsertSidecarSubtitle {
        file_id,
        library_id,
        path: std::path::PathBuf::from(format!("/videos/Heat/{name}")),
        info: SidecarInfo {
            format: SubtitleFormat::Srt,
            language: Some(language.to_string()),
            title: None,
            is_forced: false,
            is_sdh: false,
            is_default: false,
        },
        size_bytes: 1_000,
        mtime: None,
    };
    let mut english = Vec::new();
    for upsert in [
        sidecar(parts[0], "Heat - CD1.eng.srt", "eng"),
        sidecar(parts[1], "Heat - CD2.dan.srt", "dan"),
        sidecar(parts[1], "Heat - CD2.eng.srt", "eng"),
    ] {
        let is_english = upsert.info.language.as_deref() == Some("eng");
        let row = library.sidecars.upsert_by_path(upsert).await.unwrap();
        if is_english {
            english.push(row.id.to_string());
        }
    }
    // Only the second disc has chapters (issue #265).
    {
        use beam_domain::repositories::ChapterRepository;
        library
            .chapters
            .replace_for_file(
                parts[1],
                vec![beam_domain::models::chapter::Chapter {
                    index: 0,
                    start_ms: 3_000_000,
                    end_ms: 3_090_000,
                    title: Some("End Credits".into()),
                }],
            )
            .await
            .unwrap();
    }
    let state = state_with_service(real_service(library));
    let viewer = uuid::Uuid::new_v4();
    let token = session_for(&state, viewer).await;
    state
        .services
        .language_preferences
        .replace(viewer, &prefs(&[], &["eng"], SubtitleMode::On))
        .await
        .unwrap();

    let body: serde_json::Value = client(state)
        .get(&format!("/v1/media/{}/sources", movie.id))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    let source = &body["items"][0];
    assert_eq!(
        source["parts"]
            .as_array()
            .expect("an array")
            .iter()
            .map(|part| part["file_id"].as_str().expect("an id").to_owned())
            .collect::<Vec<_>>(),
        parts.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "one source of both discs, in order"
    );
    assert_eq!(source["default_subtitle_track_index"], serde_json::json!(0));
    let named = |part: &serde_json::Value| {
        let index = part["default_subtitle_track_index"]
            .as_u64()
            .expect("a default subtitle track");
        part["subtitle_tracks"][usize::try_from(index).unwrap()]["sidecar_id"].clone()
    };
    assert_eq!(named(&source["parts"][0]), serde_json::json!(english[0]));
    assert_eq!(
        source["parts"][1]["default_subtitle_track_index"],
        serde_json::json!(1),
        "the second disc's English file follows its Danish one"
    );
    assert_eq!(named(&source["parts"][1]), serde_json::json!(english[1]));
    // Each part's chapters and markers are its own, on its own timeline; the
    // source's are the first part's.
    assert_eq!(source["chapters"], serde_json::json!([]));
    assert_eq!(source["parts"][0]["markers"], serde_json::json!([]));
    assert_eq!(
        source["parts"][1]["markers"],
        serde_json::json!([{"kind": "credits", "start_ms": 3_000_000, "end_ms": 3_090_000}])
    );
}

/// The preferences cannot be read: a 500, never sources whose defaults are
/// silently someone else's choice.
#[tokio::test]
async fn sources_whose_preferences_cannot_be_read_are_a_500() {
    #[derive(Debug)]
    struct Unreadable;

    #[async_trait::async_trait]
    impl beam_domain::repositories::LanguagePreferenceRepository for Unreadable {
        async fn find(
            &self,
            _user_id: uuid::Uuid,
        ) -> Result<beam_domain::models::language_preferences::LanguagePreferences, sea_orm::DbErr>
        {
            Err(sea_orm::DbErr::Custom("the store is down".to_owned()))
        }

        async fn replace(
            &self,
            _user_id: uuid::Uuid,
            _preferences: &beam_domain::models::language_preferences::LanguagePreferences,
        ) -> Result<(), sea_orm::DbErr> {
            Err(sea_orm::DbErr::Custom("the store is down".to_owned()))
        }
    }

    let mut stub = StubMetadataService::default();
    stub.sources
        .insert(MOVIE_ID.to_owned(), vec![movie_source(FILE_ID)]);
    let state = state_over(
        crate::routes::test_support::state_with_language_preferences(Arc::new(Unreadable)),
        Arc::new(stub),
    );
    let token = seed_session(&state).await;

    client(state)
        .get(&format!("/v1/media/{MOVIE_ID}/sources"))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
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

#[tokio::test]
async fn the_sources_route_requires_a_session() {
    let client = client(state_with(StubMetadataService::default()));

    let response = client
        .get(&format!("/v1/media/{MOVIE_ID}/sources"))
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
