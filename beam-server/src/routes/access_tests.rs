//! Subcutaneous tests for per-user access (issue #263): what a restricted
//! viewer is shown, and that every route reading or delivering a title
//! answers a hidden one exactly as one that does not exist.
//!
//! The router, the handlers, the metadata, playback, subtitle and access
//! services and the artwork cache are the real ones, over in-memory stores
//! seeded through their own traits; files are real files in a `TempDir`.
//! Each hidden case is paired with the same request for a title the viewer
//! may see, so a route that answered everything with 404 would fail.

use std::collections::{BTreeSet, HashSet};
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
use beam_domain::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
use beam_domain::models::trickplay::{TrickplayLayout, TrickplayRecord};
use beam_domain::models::user_list::UserList;
use beam_domain::models::watch_state::TitleRef;
use beam_domain::providers::artwork::ImageFormat;
use beam_domain::providers::artwork::test_utils::InMemoryArtworkFetcher;
use beam_domain::providers::enrichment::{
    CollectionEnrichment, CreditEnrichment, ExternalMediaRef,
};
use beam_domain::providers::enrichment::{MovieEnrichment, ShowEnrichment};
use beam_domain::repositories::access_policy::in_memory::InMemoryTitleAccess;
use beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository;
use beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::person::in_memory::InMemoryPersonRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::trickplay::in_memory::InMemoryTrickplayRepository;
use beam_domain::repositories::user_list::InMemoryUserListRepository;
use beam_domain::repositories::{
    CollectionRepository, FileRepository, GenreRepository, LibraryRepository, MovieRepository,
    PersonRepository, ShowRepository, SidecarSubtitleRepository, TrickplayRepository,
    UserListRepository,
};
use beam_domain::services::{RealClock, TestClock};
use beam_domain::test_utils::create_media_file;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use tempfile::TempDir;
use uuid::Uuid;

use crate::models::playback::{
    ContinueWatchingConnection, HistoryConnection, ReportProgressRequest,
};
use crate::models::{
    CollectionConnection, CollectionDetail, MediaConnection, MediaMetadata, PersonConnection,
    PersonDetail, UserListConnection,
};
use crate::routes::artwork::{get_artwork, head_artwork};
use crate::routes::collections::{get_collection, list_collections};
use crate::routes::genres::{GenreConnection, list_genres};
use crate::routes::lists::{
    add_to_favourites, add_to_watchlist, get_favourites, get_watchlist, remove_from_favourites,
    remove_from_watchlist,
};
use crate::routes::media::{
    browse_media, get_episode_detail, get_media_detail, get_media_sources, get_season_detail,
};
use crate::routes::people::{get_person, search_people};
use crate::routes::playback::{
    clear_title_progress, dismiss_continue_watching, get_continue_watching, get_history,
    get_title_progress, mark_unwatched, mark_watched, report_playback_progress,
};
use crate::routes::stream::{download_file, head_download_file, head_stream_file, stream_file};
use crate::routes::subtitle::{
    get_subtitle, get_subtitle_webvtt, head_subtitle, head_subtitle_webvtt,
};
use crate::routes::test_support::{access_over, make_app_state};
use crate::services::artwork::{ArtworkCache, ArtworkCacheConfig};
use crate::services::collections::CollectionService;
use crate::services::library::{LibraryError, LibraryService, LocatedFile};
use crate::services::lists::{ListDependencies, ListService};
use crate::services::metadata::{DbMetadataService, MetadataRepositories};
use crate::services::people::PeopleService;
use crate::services::playback::{DbPlaybackService, PlaybackRepositories};
use crate::services::sources::SourceCatalog;
use crate::services::trickplay::{TrickplayDependencies, TrickplayService};
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
    genres: Arc<InMemoryGenreRepository>,
    people: Arc<InMemoryPersonRepository>,
    collections: Arc<InMemoryCollectionRepository>,
    lists: Arc<InMemoryUserListRepository>,
    clock: Arc<TestClock>,
    sidecars: Arc<InMemorySidecarSubtitleRepository>,
    trickplay_records: Arc<InMemoryTrickplayRepository>,
    trickplay_root: PathBuf,
    root: TempDir,
}

fn fixture() -> Fixture {
    let base = make_app_state();
    let root = TempDir::new().expect("a temp dir");
    let files = Arc::new(InMemoryFileRepository::default());
    let movies = Arc::new(InMemoryMovieRepository::with_files(files.clone()));
    let shows = Arc::new(InMemoryShowRepository::with_files(files.clone()));
    // Every store that lists titles or what hangs off them reads the same
    // titles to apply a policy (issue #328).
    let titles = InMemoryTitleAccess::new(movies.clone(), shows.clone());
    let genres = Arc::new(InMemoryGenreRepository::with_titles(titles.clone()));
    let people = Arc::new(InMemoryPersonRepository::with_titles(titles.clone()));
    let collections = Arc::new(InMemoryCollectionRepository::with_titles(titles.clone()));
    let libraries = Arc::new(InMemoryLibraryRepository::default());
    let clock = Arc::new(TestClock::new());
    let lists = Arc::new(InMemoryUserListRepository::with_titles(
        clock.clone(),
        titles,
    ));
    let sidecars = Arc::new(InMemorySidecarSubtitleRepository::default());
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
        people: people.clone(),
        collections: collections.clone(),
    }));
    // A real trickplay service over a data directory in the temp dir, so a
    // hidden file can be given a sheet the route would otherwise serve.
    let trickplay_records = Arc::new(InMemoryTrickplayRepository::default());
    let trickplay_root = root.path().join("trickplay");
    let trickplay = Arc::new(TrickplayService::new(
        trickplay_root.clone(),
        TrickplayDependencies {
            libraries: libraries.clone(),
            files: files.clone(),
            records: trickplay_records.clone(),
            generator: Arc::new(beam_index::services::trickplay::MockTrickplayGenerator::new()),
            clock: Arc::new(RealClock),
        },
    ));
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
        metadata: metadata.clone(),
        subtitles: Arc::new(crate::services::subtitle::DbSubtitleService::new(
            libraries.clone(),
            files.clone(),
            sidecars.clone(),
        )),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        language_preferences: base.services.language_preferences.clone(),
        playback: playback.clone(),
        genre_repo: genres.clone(),
        library_repo: libraries.clone(),
        file_repo: files.clone(),
        enrichment_repo: base.services.enrichment_repo.clone(),
        enrichment_control: base.services.enrichment_control.clone(),
        people: Arc::new(PeopleService::new(
            people.clone(),
            movies.clone(),
            shows.clone(),
        )),
        lists: Arc::new(ListService::new(ListDependencies {
            lists: lists.clone(),
            movies: movies.clone(),
            shows: shows.clone(),
            metadata: metadata.clone(),
            playback: playback.clone(),
        })),
        collections: Arc::new(CollectionService::new(
            collections.clone(),
            metadata.clone(),
            playback.clone(),
        )),
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
        trickplay,
    };

    Fixture {
        state: AppState::new(base.config.clone(), services, base.probe.clone(), None),
        files,
        movies,
        shows,
        libraries,
        genres,
        people,
        collections,
        lists,
        clock,
        sidecars,
        trickplay_records,
        trickplay_root,
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
                    head_subtitle,
                    get_subtitle_webvtt,
                    head_subtitle_webvtt,
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
                ])
                .mount(kynos::routes![
                    get_watchlist,
                    add_to_watchlist,
                    remove_from_watchlist,
                    get_favourites,
                    add_to_favourites,
                    remove_from_favourites,
                ])
                .mount(kynos::routes![
                    list_collections,
                    get_collection,
                    search_people,
                    get_person,
                    list_genres,
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
                size_bytes: FILE_BYTES.len() as u64,
                duration: Some(Duration::from_secs(100)),
                content: Some(content),
                status: FileStatus::Known,
                ..create_media_file(library_id, path)
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

    /// A real SubRip subtitle beside `file_id`, indexed as the scanner would.
    async fn subtitle(&self, file_id: Uuid) -> Uuid {
        let file = self
            .files
            .find_by_id(file_id)
            .await
            .expect("the file store answers")
            .expect("the file exists");
        let path = file.path.with_extension("en.srt");
        let bytes = b"1\n00:00:01,000 --> 00:00:02,000\nHello\n";
        std::fs::write(&path, bytes).expect("write the subtitle");
        self.sidecars
            .upsert_by_path(UpsertSidecarSubtitle {
                file_id,
                library_id: file.library_id,
                path,
                info: SidecarInfo {
                    format: SubtitleFormat::Srt,
                    language: Some("eng".to_owned()),
                    title: None,
                    is_forced: false,
                    is_sdh: false,
                    is_default: false,
                },
                size_bytes: bytes.len() as u64,
                mtime: None,
            })
            .await
            .expect("index the subtitle")
            .id
    }

    /// Trickplay sheet 0 of `file_id`, recorded and written where the worker
    /// would have put it.
    async fn trickplay_sheet(&self, file_id: Uuid) {
        let file = self
            .files
            .find_by_id(file_id)
            .await
            .expect("the file store answers")
            .expect("the file exists");
        let dir = self
            .trickplay_root
            .join(file_id.to_string())
            .join(format!("{:016x}", file.hash));
        std::fs::create_dir_all(&dir).expect("a sheet folder");
        std::fs::write(dir.join("0.jpg"), POSTER_BYTES).expect("write the sheet");
        self.trickplay_records
            .upsert(TrickplayRecord {
                file_id,
                file_hash: file.hash,
                layout: Some(TrickplayLayout {
                    interval_ms: 10_000,
                    tile_width: 320,
                    tile_height: 180,
                    columns: 10,
                    rows: 10,
                    tile_count: 100,
                }),
                generated_at: chrono::Utc::now(),
            })
            .await
            .expect("record the sheet");
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

/// A season's poster and an episode's still are art of their show: hidden
/// with it from a viewer who may not see the show, served to one who may.
#[tokio::test]
async fn a_hidden_shows_season_and_episode_artwork_is_not_found() {
    use beam_domain::providers::enrichment::{EpisodeEnrichment, SeasonEnrichment};

    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    fixture
        .shows
        .apply_season_enrichment(
            titles.show,
            &SeasonEnrichment {
                season_number: 1,
                poster_url: Some(POSTER_URL.to_string()),
                episodes: vec![EpisodeEnrichment {
                    episode_number: 1,
                    thumbnail_url: Some(POSTER_URL.to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .expect("the season's art is stored");
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;
    let (open, _) = fixture.viewer().await;

    for path in [
        format!("/v1/artwork/season/{}/poster", titles.season),
        format!("/v1/artwork/episode/{}/thumbnail", titles.episode),
    ] {
        client
            .get(&path)
            .cookie("beam_session", &restricted)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND);
        client
            .head(&path)
            .cookie("beam_session", &restricted)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND);
        let seen = client.get(&path).cookie("beam_session", &open).send().await;
        seen.assert_status(StatusCode::OK);
        assert_eq!(seen.bytes().as_ref(), POSTER_BYTES, "{path}");
    }
}

/// An administrator is held to their own access as any user is (the
/// `/v1/admin` operations aside): restricted to the kids' library, an
/// administrator does not see the grown-ups' titles or files.
#[tokio::test]
async fn a_restricted_administrator_is_held_to_their_access() {
    use beam_auth::utils::models::CreateUser;

    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let admin = fixture
        .state
        .services
        .user_repo
        .create(CreateUser {
            oidc_issuer: "https://idp.test".to_owned(),
            oidc_subject: "restricted-admin".to_owned(),
            email: None,
            display_name: "Restricted Admin".to_owned(),
            avatar_url: None,
            is_admin: true,
        })
        .await
        .expect("an administrator");
    let token = fixture
        .state
        .services
        .session_store
        .create(
            &SessionData {
                user_id: admin.id.to_string(),
                device_hash: "test-device".to_owned(),
                ip: "127.0.0.1".to_owned(),
                created_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
            },
            86_400,
            86_400,
        )
        .await
        .expect("a session");
    fixture.restrict(admin.id, kids).await;

    for path in [
        format!("/v1/media/{}", titles.grown_movie),
        format!("/v1/media/{}", titles.show),
        format!("/v1/files/{}/stream", titles.grown_file),
    ] {
        assert_eq!(
            status(&client, &token, &path).await,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        status(&client, &token, &format!("/v1/media/{}", titles.kids_movie)).await,
        StatusCode::OK
    );
    let browsed: MediaConnection = client
        .get("/v1/media")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(
        browsed
            .items
            .iter()
            .map(|item| match item {
                MediaMetadata::Movie(movie) => movie.id,
                MediaMetadata::Show(show) => show.id,
            })
            .collect::<Vec<_>>(),
        vec![titles.kids_movie]
    );
}

/// An access-policy store that fails every call: what a database outage
/// looks like from the access service.
#[derive(Debug)]
struct FailingPolicies;

#[async_trait::async_trait]
impl beam_domain::repositories::AccessPolicyRepository for FailingPolicies {
    async fn find(&self, _: Uuid) -> Result<AccessPolicy, sea_orm::DbErr> {
        Err(crate::routes::test_support::outage())
    }
    async fn replace(&self, _: Uuid, _: &AccessPolicy) -> Result<(), sea_orm::DbErr> {
        Err(crate::routes::test_support::outage())
    }
}

/// NFR-205: a policy that cannot be read is a 500, never the title -- which
/// would show a restricted viewer what they may not see -- and never a 404,
/// which would tell them a title they may see is gone.
#[tokio::test]
async fn an_access_store_failure_is_a_500_never_the_title_or_not_found() {
    let fixture = fixture();
    let (_, titles) = fixture.catalogue().await;
    let state = {
        let base = &fixture.state;
        crate::routes::test_support::state_with(|services| {
            services.access = Arc::new(crate::services::access::AccessService::new(
                crate::services::access::AccessRepositories {
                    policies: Arc::new(FailingPolicies),
                    movies: fixture.movies.clone(),
                    shows: fixture.shows.clone(),
                    files: fixture.files.clone(),
                },
            ));
            services.metadata = base.services.metadata.clone();
            services.movie_repo = fixture.movies.clone();
            services.show_repo = fixture.shows.clone();
            services.file_repo = fixture.files.clone();
            services.library = base.services.library.clone();
        })
    };
    let token = crate::routes::test_support::signed_in(&state).await;
    let client = TestClient::new(
        Router::new()
            .nest(
                "/v1",
                Router::new()
                    .mount(kynos::routes![browse_media, get_media_detail])
                    .mount(kynos::routes![stream_file]),
            )
            .build(state)
            .expect("the router describes itself"),
    );

    for path in [
        "/v1/media".to_owned(),
        format!("/v1/media/{}", titles.kids_movie),
        format!("/v1/files/{}/stream", titles.kids_file),
    ] {
        let response = client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        response
            .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
        assert!(!response.text().contains("Paddington"), "{path}");
    }
}

#[tokio::test]
async fn a_hidden_titles_files_are_not_found_to_stream_download_or_subtitle() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;
    let (unrestricted, _) = fixture.viewer().await;

    for file in [titles.grown_file, titles.episode_file] {
        // Every path names something that exists: a real subtitle and a real
        // trickplay sheet, so only the access check can make it a 404.
        let subtitle = fixture.subtitle(file).await;
        fixture.trickplay_sheet(file).await;
        let paths = [
            format!("/v1/files/{file}/stream"),
            format!("/v1/files/{file}/download"),
            format!("/v1/files/{file}/subtitles/{subtitle}"),
            format!("/v1/files/{file}/subtitles/{subtitle}/webvtt"),
            format!("/v1/files/{file}/trickplay/0"),
        ];
        let mut visible_lengths = Vec::new();
        for path in &paths {
            assert_eq!(
                status(&client, &unrestricted, path).await,
                StatusCode::OK,
                "GET {path} for a viewer who may see it"
            );
            let head = client
                .head(path)
                .cookie("beam_session", &unrestricted)
                .send()
                .await;
            assert_eq!(head.status(), StatusCode::OK, "HEAD {path}");
            assert!(head.header("etag").is_some(), "HEAD {path} has a validator");
            visible_lengths.push(
                head.header("content-length")
                    .expect("HEAD tells a visible length")
                    .to_owned(),
            );
        }
        for path in &paths {
            client
                .get(path)
                .cookie("beam_session", &restricted)
                .send()
                .await
                .assert_status(StatusCode::NOT_FOUND)
                .assert_problem_type(FILE_NOT_FOUND);
        }
        // A HEAD answers what its GET would, and so must not tell a hidden
        // file's size or validator either.
        for (path, visible_length) in paths.iter().zip(&visible_lengths) {
            let response = client
                .head(path)
                .cookie("beam_session", &restricted)
                .send()
                .await;
            response.assert_status(StatusCode::NOT_FOUND);
            assert_eq!(response.header("etag"), None, "HEAD {path}");
            assert_ne!(
                response.header("content-length"),
                Some(visible_length.as_str()),
                "HEAD {path} tells the hidden length"
            );
        }
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
    let (restricted, restricted_id) = fixture.kids_viewer(kids).await;

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

    // The refusals wrote nothing: once the restriction is lifted, the
    // viewer's history holds only the title they were allowed to play.
    fixture
        .state
        .services
        .access
        .replace(restricted_id, &AccessPolicy::unrestricted())
        .await
        .expect("lift the restriction");
    let history: HistoryConnection = client
        .get("/v1/history")
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(
        history
            .items
            .iter()
            .map(|item| item.media_id)
            .collect::<Vec<_>>(),
        vec![titles.kids_movie],
        "no refused report or mark reached the viewer's history"
    );
    assert_eq!(history.total, 1);
    let shelf: ContinueWatchingConnection = client
        .get("/v1/continue-watching")
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert!(
        shelf.items.is_empty(),
        "nothing refused is on the shelf, and the one title played was finished"
    );
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

const COLLECTION_NOT_FOUND: &str =
    "https://beam.justinchung.net/reference/errors/#collection-not-found";
const PERSON_NOT_FOUND: &str = "https://beam.justinchung.net/reference/errors/#person-not-found";

/// The ids of a page of a list, newest first.
fn listed_ids(page: &UserListConnection) -> Vec<Uuid> {
    page.items
        .iter()
        .map(|item| match &item.media {
            MediaMetadata::Movie(movie) => movie.id,
            MediaMetadata::Show(show) => show.id,
        })
        .collect()
}

/// A watchlist and favourites list a hidden title as one that is not there:
/// pages and totals leave it out, and adding or removing it answers as an
/// unknown id does -- while its entry stays, for when the policy admits it
/// again (issue #328).
#[tokio::test]
async fn a_hidden_title_is_off_the_viewers_lists_and_cannot_be_put_on_or_taken_off() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let client = client(&fixture);
    let (token, user_id) = fixture.viewer().await;

    for id in [titles.kids_movie, titles.grown_movie, titles.show] {
        for list in ["watchlist", "favourites"] {
            client
                .put(&format!("/v1/{list}/{id}"))
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::NO_CONTENT);
        }
        fixture.clock.advance(Duration::from_secs(1));
    }
    fixture.restrict(user_id, kids).await;

    for list in ["watchlist", "favourites"] {
        let page: UserListConnection = client
            .get(&format!("/v1/{list}?first=1"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::OK)
            .json();
        assert_eq!(listed_ids(&page), [titles.kids_movie], "{list}");
        assert_eq!(page.total, 1, "{list}");
        assert!(
            !page.page_info.has_next_page,
            "a hidden entry does not hold a place on a page: {list}"
        );
    }

    for id in [
        titles.grown_movie,
        titles.show,
        titles.season,
        titles.episode,
    ] {
        for request in [
            client.put(&format!("/v1/watchlist/{id}")),
            client.delete(&format!("/v1/watchlist/{id}")),
            client.put(&format!("/v1/favourites/{id}")),
            client.delete(&format!("/v1/favourites/{id}")),
        ] {
            request
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::NOT_FOUND)
                .assert_problem_type(MEDIA_NOT_FOUND);
        }
    }
    client
        .delete(&format!("/v1/watchlist/{}", titles.kids_movie))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let open = AccessPolicy::unrestricted();
    let kept: HashSet<TitleRef> = fixture
        .lists
        .find_page(user_id, UserList::Watchlist, &open, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.title)
        .collect();
    assert_eq!(
        kept,
        HashSet::from([
            TitleRef::Movie(titles.grown_movie),
            TitleRef::Show(titles.show)
        ]),
        "the refused removals removed nothing; the visible one did"
    );
    assert_eq!(
        fixture
            .lists
            .count(user_id, UserList::Favourites, &open)
            .await
            .unwrap(),
        3
    );
}

/// The collection listing, a collection's movies and its art follow the
/// movies a viewer may see: one whose every movie is hidden is not there
/// (issue #328).
#[tokio::test]
async fn a_collection_holds_only_the_movies_the_viewer_may_see() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let more = fixture.library("more").await;
    let (sequel, _) = fixture.movie(more, "more", "Heat 2", "R").await;
    let collection = |native: &str, name: &str| CollectionEnrichment {
        collection_ref: ExternalMediaRef::new("tmdb", native),
        name: name.to_string(),
        poster_url: Some(POSTER_URL.to_string()),
        backdrop_url: None,
    };
    let mixed = collection("1", "Mixed Collection");
    let grown_only = collection("2", "Grown Collection");
    for (movie, member_of) in [
        (titles.kids_movie, &mixed),
        (titles.grown_movie, &mixed),
        (sequel, &grown_only),
    ] {
        fixture
            .collections
            .set_movie_collection(movie, Some(member_of))
            .await
            .unwrap();
    }
    let id_of = |movie| {
        let collections = fixture.collections.clone();
        async move {
            collections
                .movie_collection(movie)
                .await
                .unwrap()
                .expect("the movie's collection")
                .id
        }
    };
    let (mixed_id, grown_id) = (id_of(titles.kids_movie).await, id_of(sequel).await);
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;
    let (open, _) = fixture.viewer().await;

    let listing: CollectionConnection = client
        .get("/v1/collections")
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let listed: Vec<(Uuid, u64)> = listing
        .items
        .iter()
        .map(|c| (c.id, c.movie_count))
        .collect();
    assert_eq!(listed, [(mixed_id, 1)]);
    assert_eq!(listing.total, 1);
    let everything: CollectionConnection = client
        .get("/v1/collections")
        .cookie("beam_session", &open)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(everything.total, 2);

    let detail: CollectionDetail = client
        .get(&format!("/v1/collections/{mixed_id}"))
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let movies: Vec<Uuid> = detail.movies.iter().map(|movie| movie.id).collect();
    assert_eq!(movies, [titles.kids_movie]);

    let grown_path = format!("/v1/collections/{grown_id}");
    client
        .get(&grown_path)
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type(COLLECTION_NOT_FOUND);
    assert_eq!(status(&client, &open, &grown_path).await, StatusCode::OK);

    let grown_art = format!("/v1/artwork/collection/{grown_id}/poster");
    assert_eq!(
        status(&client, &restricted, &grown_art).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(status(&client, &open, &grown_art).await, StatusCode::OK);
    let mixed_art = client
        .get(&format!("/v1/artwork/collection/{mixed_id}/poster"))
        .cookie("beam_session", &restricted)
        .send()
        .await;
    assert_eq!(mixed_art.status(), StatusCode::OK);
    assert_eq!(
        mixed_art.header("cache-control"),
        Some("private, max-age=86400")
    );
}

/// People search, a filmography and a portrait follow the titles a viewer
/// may see: a person credited only on hidden titles is not there
/// (issue #328).
#[tokio::test]
async fn people_are_found_only_through_the_titles_the_viewer_may_see() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    let credit = |native: &str, name: &str, character: &str| CreditEnrichment {
        person_ref: ExternalMediaRef::new("tmdb", native),
        name: name.to_string(),
        profile_url: Some(POSTER_URL.to_string()),
        role: beam_domain::models::person::CreditRole::Cast {
            character: Some(character.to_string()),
            billing_order: 0,
        },
    };
    let both = credit("1", "Both Films", "Lead");
    fixture
        .people
        .set_movie_credits(
            titles.kids_movie,
            &[both.clone(), credit("2", "Kids Only", "Bear")],
        )
        .await
        .unwrap();
    fixture
        .people
        .set_movie_credits(
            titles.grown_movie,
            &[both, credit("3", "Grown Only", "Cop")],
        )
        .await
        .unwrap();
    fixture
        .people
        .set_show_credits(titles.show, &[credit("4", "Show Only", "Detective")])
        .await
        .unwrap();
    let person = |title, name: &'static str| {
        let people = fixture.people.clone();
        async move {
            people
                .movie_credits(title)
                .await
                .unwrap()
                .into_iter()
                .find(|c| c.person.name == name)
                .expect("the credited person")
                .person
                .id
        }
    };
    let (shared, grown_only) = (
        person(titles.kids_movie, "Both Films").await,
        person(titles.grown_movie, "Grown Only").await,
    );
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;
    let (open, _) = fixture.viewer().await;

    let found = |token: String, query: &'static str| {
        let client = &client;
        async move {
            let page: PersonConnection = client
                .get(&format!("/v1/people{query}"))
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::OK)
                .json();
            (
                page.items
                    .into_iter()
                    .map(|p| p.name)
                    .collect::<Vec<String>>(),
                page.total,
            )
        }
    };
    assert_eq!(
        found(restricted.clone(), "").await,
        (vec!["Both Films".to_string(), "Kids Only".to_string()], 2)
    );
    assert_eq!(
        found(restricted.clone(), "?query=only").await,
        (vec!["Kids Only".to_string()], 1)
    );
    assert_eq!(found(open.clone(), "").await.1, 4);

    let detail: PersonDetail = client
        .get(&format!("/v1/people/{shared}"))
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let credited: Vec<Uuid> = detail.cast.iter().map(|c| c.title.id).collect();
    assert_eq!(credited, [titles.kids_movie]);

    let hidden_path = format!("/v1/people/{grown_only}");
    client
        .get(&hidden_path)
        .cookie("beam_session", &restricted)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type(PERSON_NOT_FOUND);
    assert_eq!(status(&client, &open, &hidden_path).await, StatusCode::OK);

    let hidden_portrait = format!("/v1/artwork/person/{grown_only}/profile");
    assert_eq!(
        status(&client, &restricted, &hidden_portrait).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status(&client, &open, &hidden_portrait).await,
        StatusCode::OK
    );
    assert_eq!(
        status(
            &client,
            &restricted,
            &format!("/v1/artwork/person/{shared}/profile")
        )
        .await,
        StatusCode::OK
    );
}

/// The genre filter offers only the genres of titles a viewer may see.
#[tokio::test]
async fn the_genres_listed_are_those_of_the_titles_the_viewer_may_see() {
    let fixture = fixture();
    let (kids, titles) = fixture.catalogue().await;
    fixture
        .genres
        .set_movie_genres(titles.kids_movie, &["Family".to_string()])
        .await
        .unwrap();
    fixture
        .genres
        .set_movie_genres(
            titles.grown_movie,
            &["Crime".to_string(), "Family".to_string()],
        )
        .await
        .unwrap();
    fixture
        .genres
        .set_show_genres(titles.show, &["Drama".to_string()])
        .await
        .unwrap();
    let client = client(&fixture);
    let (restricted, _) = fixture.kids_viewer(kids).await;
    let (open, _) = fixture.viewer().await;

    let genres = |token: String| {
        let client = &client;
        async move {
            client
                .get("/v1/genres")
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::OK)
                .json::<GenreConnection>()
                .items
        }
    };
    assert_eq!(genres(restricted).await, ["Family"]);
    assert_eq!(genres(open).await, ["Crime", "Drama", "Family"]);
}
