//! Subcutaneous tests for per-user access (issue #263): what a restricted
//! viewer is shown, and that every route reading or delivering a title
//! answers a hidden one exactly as one that does not exist.
//!
//! The router, the handlers, the metadata, playback, subtitle and access
//! services and the artwork cache are the real ones, over in-memory stores
//! seeded through their own traits; files are real files in a `TempDir`.
//! Each hidden case is paired with the same request for a title the viewer
//! may see, so a route that answered everything with 404 would fail.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use beam_auth::utils::session_store::SessionData;
use beam_domain::models::access::{AccessPolicy, ContentRating, LibraryAccess};
use beam_domain::models::enrichment::FieldLocks;
use beam_domain::models::file::{CreateMediaFile, FileStatus, MediaFileContent};
use beam_domain::models::library::CreateLibrary;
use beam_domain::models::movie::{CreateMovie, CreateMovieEntry};
use beam_domain::models::show::{CreateEpisode, CreateShow};
use beam_domain::providers::artwork::ImageFormat;
use beam_domain::providers::artwork::test_utils::InMemoryArtworkFetcher;
use beam_domain::providers::enrichment::{MovieEnrichment, ShowEnrichment};
use beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::{
    FileRepository, LibraryRepository, MovieRepository, ShowRepository,
};
use beam_domain::services::{RealClock, TestClock};
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use tempfile::TempDir;
use uuid::Uuid;

use crate::models::playback::{
    ContinueWatchingConnection, HistoryConnection, ReportProgressRequest,
};
use crate::models::{MediaConnection, MediaMetadata};
use crate::routes::artwork::{get_artwork, head_artwork};
use crate::routes::media::{
    browse_media, get_episode_detail, get_media_detail, get_media_sources, get_season_detail,
};
use crate::routes::playback::{
    clear_title_progress, dismiss_continue_watching, get_continue_watching, get_history,
    get_title_progress, mark_unwatched, mark_watched, report_playback_progress,
};
use crate::routes::stream::{download_file, head_download_file, head_stream_file, stream_file};
use crate::routes::subtitle::{get_subtitle, get_subtitle_webvtt};
use crate::routes::test_support::{access_over, make_app_state};
use crate::services::artwork::{ArtworkCache, ArtworkCacheConfig};
use crate::services::library::{LibraryError, LibraryService, LocatedFile};
use crate::services::metadata::{DbMetadataService, MetadataRepositories};
use crate::services::playback::{DbPlaybackService, PlaybackRepositories};
use crate::services::sources::SourceCatalog;
use crate::state::{AppServices, AppState};

const POSTER_URL: &str = "https://image.tmdb.org/t/p/w500/poster.jpg";
const POSTER_BYTES: &[u8] = b"\xff\xd8\xff-not-really-a-jpeg";
const FILE_BYTES: &[u8] = b"not really a film, but bytes to serve";

const MEDIA_NOT_FOUND: &str = "https://beam.justinchung.net/reference/errors/#media-not-found";
const FILE_NOT_FOUND: &str = "https://beam.justinchung.net/reference/errors/#file-not-found";

/// Locates a file by its row, beneath the one root every test library is
/// in. The rest of the library service is not what these tests read.
#[derive(Debug)]
struct FileLocator {
    files: Arc<InMemoryFileRepository>,
    root: PathBuf,
}

#[async_trait::async_trait]
impl LibraryService for FileLocator {
    async fn get_libraries(
        &self,
        _user_id: String,
    ) -> Result<Vec<crate::models::Library>, LibraryError> {
        unimplemented!("not called by the routes under test")
    }
    async fn get_library_by_id(
        &self,
        _library_id: Uuid,
    ) -> Result<Option<crate::models::Library>, LibraryError> {
        unimplemented!("not called by the routes under test")
    }
    async fn get_library_files(
        &self,
        _library_id: Uuid,
        _after: Option<(String, Uuid)>,
        _first: std::num::NonZeroU32,
    ) -> Result<crate::services::library::LibraryFilePage, LibraryError> {
        unimplemented!("not called by the routes under test")
    }
    async fn get_file_by_id(&self, file_id: Uuid) -> Result<Option<LocatedFile>, LibraryError> {
        Ok(self
            .files
            .find_by_id(file_id)
            .await?
            .map(|file| LocatedFile {
                id: file.id,
                path: file.path,
                library_root: self.root.clone(),
                mime_type: file.mime_type,
            }))
    }
    async fn create_library(
        &self,
        _name: String,
        _root_path: String,
    ) -> Result<crate::models::Library, LibraryError> {
        unimplemented!("not called by the routes under test")
    }
    async fn start_scan(&self, _library_id: Uuid) -> Result<crate::models::ScanJob, LibraryError> {
        unimplemented!("not called by the routes under test")
    }
    async fn get_scan(
        &self,
        _library_id: Uuid,
    ) -> Result<Option<crate::models::ScanJob>, LibraryError> {
        unimplemented!("not called by the routes under test")
    }
    async fn delete_library(&self, _library_id: Uuid) -> Result<bool, LibraryError> {
        unimplemented!("not called by the routes under test")
    }
}

/// The titles every test starts from: a `PG` movie in the kids' library,
/// and an `R` movie and a `TV-MA` show in the grown-ups'.
struct Catalogue {
    kids_movie: Uuid,
    kids_file: Uuid,
    grown_movie: Uuid,
    grown_file: Uuid,
    show: Uuid,
    season: Uuid,
    episode: Uuid,
    episode_file: Uuid,
}

struct Fixture {
    state: AppState,
    files: Arc<InMemoryFileRepository>,
    movies: Arc<InMemoryMovieRepository>,
    shows: Arc<InMemoryShowRepository>,
    libraries: Arc<InMemoryLibraryRepository>,
    clock: Arc<TestClock>,
    root: TempDir,
}

fn fixture() -> Fixture {
    let base = make_app_state();
    let root = TempDir::new().expect("a temp dir");
    let files = Arc::new(InMemoryFileRepository::default());
    let movies = Arc::new(InMemoryMovieRepository::with_files(files.clone()));
    let shows = Arc::new(InMemoryShowRepository::with_files(files.clone()));
    let genres = Arc::new(InMemoryGenreRepository::default());
    let libraries = Arc::new(InMemoryLibraryRepository::default());
    let clock = Arc::new(TestClock::new());
    let sidecars = Arc::new(
        beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository::default(),
    );
    let sources = Arc::new(SourceCatalog::new(crate::services::sources::SourceRepositories {movies: movies.clone(), files: files.clone(), streams: Arc::new(InMemoryMediaStreamRepository::default()), sidecars: sidecars.clone(), chapters: Arc::new(beam_domain::repositories::chapter::in_memory::InMemoryChapterRepository::default()), trickplay: Arc::new(beam_domain::repositories::trickplay::in_memory::InMemoryTrickplayRepository::default()),}));
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
        people: Arc::new(
            beam_domain::repositories::person::in_memory::InMemoryPersonRepository::default(),
        ),
        collections: Arc::new(
            beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository::default(
            ),
        ),
    }));
    let artwork = Arc::new(ArtworkCache::new(
        ArtworkCacheConfig {
            root: root.path().join("artwork"),
            max_bytes: 1_000_000,
            negative_ttl: Duration::from_secs(300),
        },
        Arc::new(InMemoryArtworkFetcher::new().with_image(
            POSTER_URL,
            ImageFormat::Jpeg,
            POSTER_BYTES,
        )),
        Arc::new(RealClock),
    ));

    let services = AppServices {
        hash: base.services.hash.clone(),
        access: access_over(movies.clone(), shows.clone(), files.clone()),
        library: Arc::new(FileLocator {
            files: files.clone(),
            root: root.path().to_path_buf(),
        }),
        metadata,
        subtitles: Arc::new(crate::services::subtitle::DbSubtitleService::new(
            libraries.clone(),
            files.clone(),
            sidecars,
        )),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        language_preferences: base.services.language_preferences.clone(),
        playback,
        genre_repo: genres,
        library_repo: libraries.clone(),
        file_repo: files.clone(),
        enrichment_repo: base.services.enrichment_repo.clone(),
        enrichment_control: base.services.enrichment_control.clone(),
        people: crate::routes::test_support::idle_people(movies.clone(), shows.clone()),
        lists: crate::routes::test_support::unused_lists(),
        collections: crate::routes::test_support::unused_collections(),
        movie_repo: movies.clone(),
        show_repo: shows.clone(),
        artwork,
        session_store: base.services.session_store.clone(),
        oidc_client: base.services.oidc_client.clone(),
        pending_auth_store: base.services.pending_auth_store.clone(),
        device_auth_store: base.services.device_auth_store.clone(),
        oidc_config: base.services.oidc_config.clone(),
        watch_status: Arc::new(beam_index::services::watch_status::WatchStatus::new()),
        telemetry: crate::routes::test_support::idle_library_report(),
        playback_telemetry: crate::routes::test_support::idle_playback_telemetry(),
        trickplay: crate::routes::test_support::idle_trickplay(),
    };

    Fixture {
        state: AppState::new(base.config.clone(), services, base.probe.clone(), None),
        files,
        movies,
        shows,
        libraries,
        clock,
        root,
    }
}

fn client(fixture: &Fixture) -> TestClient<AppState> {
    let service = Router::new()
        .nest(
            "/v1",
            Router::new()
                .mount(kynos::routes![
                    browse_media,
                    get_media_detail,
                    get_media_sources,
                    get_episode_detail,
                    get_season_detail,
                ])
                .mount(kynos::routes![get_artwork, head_artwork])
                .mount(kynos::routes![
                    stream_file,
                    head_stream_file,
                    download_file,
                    head_download_file,
                    get_subtitle,
                    get_subtitle_webvtt,
                ])
                .mount(kynos::routes![
                    crate::routes::trickplay::get_trickplay_sheet,
                    crate::routes::trickplay::head_trickplay_sheet,
                ])
                .mount(kynos::routes![
                    report_playback_progress,
                    get_title_progress,
                    clear_title_progress,
                    mark_watched,
                    mark_unwatched,
                    get_continue_watching,
                    dismiss_continue_watching,
                    get_history,
                ]),
        )
        .build(fixture.state.clone())
        .expect("the router describes itself");
    TestClient::new(service)
}

impl Fixture {
    async fn library(&self, name: &str) -> Uuid {
        let root = self.root.path().join(name);
        std::fs::create_dir_all(&root).expect("a library folder");
        self.libraries
            .create(CreateLibrary {
                name: name.to_string(),
                root_path: self.root.path().to_path_buf(),
                description: None,
            })
            .await
            .expect("a library")
            .id
    }

    /// A real file in `library_id`'s folder holding `content`.
    async fn file(&self, library_id: Uuid, folder: &str, content: MediaFileContent) -> Uuid {
        let path = self
            .root
            .path()
            .join(folder)
            .join(format!("{}.mkv", Uuid::new_v4()));
        std::fs::write(&path, FILE_BYTES).expect("write a file");
        self.files
            .create(CreateMediaFile {
                library_id,
                path,
                hash: 0,
                size_bytes: FILE_BYTES.len() as u64,
                mtime: None,
                identity: None,
                mime_type: Some("video/x-matroska".to_string()),
                duration: Some(Duration::from_secs(100)),
                container_format: Some("matroska".to_string()),
                content: Some(content),
                status: FileStatus::Known,
                classifier_version: 0,
                container_tags: None,
            })
            .await
            .expect("a file row")
            .id
    }

    /// A movie indexed in `library_id`, certified, with a poster and a file.
    async fn movie(
        &self,
        library_id: Uuid,
        folder: &str,
        title: &str,
        certification: &str,
    ) -> (Uuid, Uuid) {
        let movie = self
            .movies
            .find_or_create_by_identity(CreateMovie::new(title, None, None))
            .await
            .unwrap();
        self.movies
            .apply_enrichment(
                movie.id,
                &MovieEnrichment {
                    title: title.to_string(),
                    poster_url: Some(POSTER_URL.to_string()),
                    content_rating: Some(certification.to_string()),
                    ..Default::default()
                },
                &FieldLocks::none(),
            )
            .await
            .unwrap();
        let entry = self
            .movies
            .find_or_create_entry(CreateMovieEntry {
                library_id,
                movie_id: movie.id,
                edition: None,
            })
            .await
            .unwrap();
        self.movies
            .ensure_library_association(library_id, movie.id)
            .await
            .unwrap();
        let file = self
            .file(library_id, folder, MediaFileContent::movie(entry.id))
            .await;
        (movie.id, file)
    }

    async fn catalogue(&self) -> (Uuid, Catalogue) {
        let kids = self.library("kids").await;
        let grown = self.library("grown").await;
        let (kids_movie, kids_file) = self.movie(kids, "kids", "Paddington", "PG").await;
        let (grown_movie, grown_file) = self.movie(grown, "grown", "Heat", "R").await;

        let show = self
            .shows
            .find_or_create_by_identity(CreateShow::new("The Wire", None))
            .await
            .unwrap();
        self.shows
            .apply_enrichment(
                show.id,
                &ShowEnrichment {
                    title: "The Wire".to_string(),
                    poster_url: Some(POSTER_URL.to_string()),
                    content_rating: Some("TV-MA".to_string()),
                    ..Default::default()
                },
                &FieldLocks::none(),
            )
            .await
            .unwrap();
        let season = self.shows.find_or_create_season(show.id, 1).await.unwrap();
        let episode = self
            .shows
            .find_or_create_episode(CreateEpisode {
                season_id: season.id,
                episode_number: 1,
                title: "The Target".to_string(),
                runtime: None,
                air_date: None,
            })
            .await
            .unwrap();
        self.shows
            .ensure_library_association(grown, show.id)
            .await
            .unwrap();
        let episode_file = self
            .file(grown, "grown", MediaFileContent::episode(episode.id))
            .await;

        (
            kids,
            Catalogue {
                kids_movie,
                kids_file,
                grown_movie,
                grown_file,
                show: show.id,
                season: season.id,
                episode: episode.id,
                episode_file,
            },
        )
    }

    /// A session for a new user, and the user.
    async fn viewer(&self) -> (String, Uuid) {
        let user_id = Uuid::new_v4();
        let token = self
            .state
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
            .expect("the in-memory session store issues a session");
        (token, user_id)
    }

    /// A viewer restricted to the kids' library, at most `PG`.
    async fn kids_viewer(&self, kids: Uuid) -> (String, Uuid) {
        let (token, user_id) = self.viewer().await;
        self.restrict(user_id, kids).await;
        (token, user_id)
    }

    async fn restrict(&self, user_id: Uuid, kids: Uuid) {
        self.state
            .services
            .access
            .replace(
                user_id,
                &AccessPolicy {
                    libraries: LibraryAccess::Only(BTreeSet::from([kids])),
                    max_rating: Some(ContentRating::ParentalGuidance),
                },
            )
            .await
            .expect("store the policy");
    }
}

async fn status(client: &TestClient<AppState>, token: &str, path: &str) -> StatusCode {
    client
        .get(path)
        .cookie("beam_session", token)
        .send()
        .await
        .status()
}

#[tokio::test]
async fn browse_lists_only_what_the_viewer_may_see() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let (open, _) = fixture.viewer().await;
    let (restricted, _) = fixture.kids_viewer(kids).await;

    let listed = |token: String| {
        let client = &client;
        async move {
            let page: MediaConnection = client
                .get("/v1/media")
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::OK)
                .json();
            page.items
                .iter()
                .map(|item| match item {
                    MediaMetadata::Movie(movie) => movie.id,
                    MediaMetadata::Show(show) => show.id,
                })
                .collect::<BTreeSet<Uuid>>()
        }
    };

    assert_eq!(
        listed(open).await,
        BTreeSet::from([titles.kids_movie, titles.grown_movie, titles.show])
    );
    assert_eq!(
        listed(restricted).await,
        BTreeSet::from([titles.kids_movie])
    );
}

#[tokio::test]
async fn a_hidden_title_and_every_part_of_it_is_not_found() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;
    let (open, _) = fixture.viewer().await;

    let hidden = [
        format!("/v1/media/{}", titles.grown_movie),
        format!("/v1/media/{}", titles.show),
        format!("/v1/media/{}/sources", titles.grown_movie),
        format!("/v1/media/{}/sources", titles.episode),
        format!("/v1/episodes/{}", titles.episode),
        format!("/v1/seasons/{}", titles.season),
        format!("/v1/media/{}/progress", titles.grown_movie),
    ];
    for path in &hidden {
        client
            .get(path)
            .cookie("beam_session", &restricted)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND)
            .assert_problem_type(MEDIA_NOT_FOUND);
        assert_eq!(
            status(&client, &open, path).await,
            StatusCode::OK,
            "{path} for a viewer nobody restricted"
        );
    }
    // And what the restricted viewer may see answers as it always did.
    for path in [
        format!("/v1/media/{}", titles.kids_movie),
        format!("/v1/media/{}/sources", titles.kids_movie),
        format!("/v1/media/{}/progress", titles.kids_movie),
    ] {
        assert_eq!(
            status(&client, &restricted, &path).await,
            StatusCode::OK,
            "{path}"
        );
    }
}

/// The certification a viewer's ceiling is compared against is on the
/// title's detail and on its browse row alike.
#[tokio::test]
async fn a_titles_certification_is_on_its_detail_and_its_browse_row() {
    let fixture = fixture();
    let (_, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let (open, _) = fixture.viewer().await;

    let detail_path = format!("/v1/media/{}", titles.show);
    let detail = client
        .get(&detail_path)
        .cookie("beam_session", &open)
        .send()
        .await;
    assert_eq!(detail.status(), StatusCode::OK);
    let MediaMetadata::Show(show) = detail.json::<MediaMetadata>() else {
        panic!("a show's detail");
    };
    assert_eq!(show.content_rating.as_deref(), Some("TV-MA"));

    let page = client
        .get("/v1/media")
        .cookie("beam_session", &open)
        .send()
        .await;
    let certifications: BTreeSet<Option<String>> = page
        .json::<MediaConnection>()
        .items
        .into_iter()
        .map(|item| match item {
            MediaMetadata::Movie(movie) => movie.content_rating,
            MediaMetadata::Show(show) => show.content_rating,
        })
        .collect();
    assert_eq!(
        certifications,
        BTreeSet::from([
            Some("PG".to_string()),
            Some("R".to_string()),
            Some("TV-MA".to_string())
        ])
    );
}

#[tokio::test]
async fn a_hidden_title_has_no_artwork_and_art_is_never_shared_between_viewers() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;

    for path in [
        format!("/v1/artwork/movie/{}/poster", titles.grown_movie),
        format!("/v1/artwork/show/{}/poster", titles.show),
    ] {
        client
            .get(&path)
            .cookie("beam_session", &restricted)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
    let seen_path = format!("/v1/artwork/movie/{}/poster", titles.kids_movie);
    let seen = client
        .get(&seen_path)
        .cookie("beam_session", &restricted)
        .send()
        .await;
    assert_eq!(seen.status(), StatusCode::OK);
    assert_eq!(seen.header("cache-control"), Some("private, max-age=86400"));
}

#[tokio::test]
async fn a_hidden_titles_files_are_not_found_to_stream_download_or_subtitle() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;

    for file in [titles.grown_file, titles.episode_file] {
        for path in [
            format!("/v1/files/{file}/stream"),
            format!("/v1/files/{file}/download"),
            format!("/v1/files/{file}/subtitles/{}", Uuid::new_v4()),
            format!("/v1/files/{file}/subtitles/{}/webvtt", Uuid::new_v4()),
            format!("/v1/files/{file}/trickplay/0"),
        ] {
            client
                .get(&path)
                .cookie("beam_session", &restricted)
                .send()
                .await
                .assert_status(StatusCode::NOT_FOUND)
                .assert_problem_type(FILE_NOT_FOUND);
        }
        client
            .head(&format!("/v1/files/{file}/stream"))
            .cookie("beam_session", &restricted)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }

    let stream_path = format!("/v1/files/{}/stream", titles.kids_file);
    let streamed = client
        .get(&stream_path)
        .cookie("beam_session", &restricted)
        .send()
        .await;
    assert_eq!(streamed.status(), StatusCode::OK);
    assert_eq!(streamed.bytes().as_ref(), FILE_BYTES);
    assert_eq!(
        streamed.header("cache-control"),
        Some("private, max-age=3600")
    );
    // The visible file has no subtitle with that id: a different 404.
    let missing_subtitle = format!(
        "/v1/files/{}/subtitles/{}",
        titles.kids_file,
        Uuid::new_v4()
    );
    client
        .get(&missing_subtitle)
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#subtitle-not-found");
    // Nor any trickplay sheet yet (issue #265): the file's own 404.
    client
        .get(&format!("/v1/files/{}/trickplay/0", titles.kids_file))
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#trickplay-not-found");
}

#[tokio::test]
async fn a_hidden_title_cannot_be_played_marked_or_dismissed() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;

    let report = |file: Uuid| {
        let client = &client;
        let token = restricted.clone();
        async move {
            client
                .put(&format!("/v1/files/{file}/progress"))
                .cookie("beam_session", &token)
                .json(&ReportProgressRequest {
                    position_secs: 10.0,
                    duration_secs: Some(100.0),
                })
                .send()
                .await
        }
    };
    report(titles.grown_file)
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type(FILE_NOT_FOUND);
    report(titles.kids_file).await.assert_status(StatusCode::OK);

    for id in [titles.grown_movie, titles.show, titles.episode] {
        let (watched, dismissed) = (
            format!("/v1/media/{id}/watched"),
            format!("/v1/continue-watching/{id}"),
        );
        for request in [
            client.put(&watched),
            client.delete(&watched),
            client.delete(&dismissed),
        ] {
            request
                .cookie("beam_session", &restricted)
                .send()
                .await
                .assert_status(StatusCode::NOT_FOUND)
                .assert_problem_type(MEDIA_NOT_FOUND);
        }
    }
    client
        .delete(&format!("/v1/media/{}/progress", titles.grown_movie))
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    client
        .put(&format!("/v1/media/{}/watched", titles.kids_movie))
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
}

/// What a viewer watched before an administrator restricted them leaves
/// their shelf and their history once its title is hidden.
#[tokio::test]
async fn continue_watching_and_history_drop_what_the_viewer_may_no_longer_see() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let (token, user_id) = fixture.viewer().await;

    for file in [titles.kids_file, titles.grown_file, titles.episode_file] {
        client
            .put(&format!("/v1/files/{file}/progress"))
            .cookie("beam_session", &token)
            .json(&ReportProgressRequest {
                position_secs: 10.0,
                duration_secs: Some(100.0),
            })
            .send()
            .await
            .assert_status(StatusCode::OK);
        fixture.clock.advance(Duration::from_secs(60));
    }
    let shelf = |token: String| {
        let client = &client;
        async move {
            let shelf: ContinueWatchingConnection = client
                .get("/v1/continue-watching")
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::OK)
                .json();
            let history: HistoryConnection = client
                .get("/v1/history")
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::OK)
                .json();
            (
                shelf
                    .items
                    .iter()
                    .map(|item| item.media_id)
                    .collect::<BTreeSet<_>>(),
                history
                    .items
                    .iter()
                    .map(|item| item.media_id)
                    .collect::<BTreeSet<_>>(),
                history.total,
            )
        }
    };
    let everything = BTreeSet::from([titles.kids_movie, titles.grown_movie, titles.show]);
    assert_eq!(
        shelf(token.clone()).await,
        (everything.clone(), everything, 3)
    );

    fixture.restrict(user_id, kids).await;

    let only_kids = BTreeSet::from([titles.kids_movie]);
    assert_eq!(
        shelf(token).await,
        (only_kids.clone(), only_kids, 3),
        "the history still counts what it no longer lists"
    );
}

#[tokio::test]
async fn an_unknown_id_is_not_found_whatever_the_viewers_access() {
    let fixture = fixture();
    let (kids, _) = fixture.catalogue().await;
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;
    let (open, _) = fixture.viewer().await;
    let unknown = Uuid::new_v4();

    for token in [&restricted, &open] {
        client
            .get(&format!("/v1/media/{unknown}"))
            .cookie("beam_session", token)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND)
            .assert_problem_type(MEDIA_NOT_FOUND);
        client
            .get(&format!("/v1/files/{unknown}/stream"))
            .cookie("beam_session", token)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND)
            .assert_problem_type(FILE_NOT_FOUND);
    }
}
