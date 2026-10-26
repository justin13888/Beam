//! Subcutaneous tests for `GET /v1/collections` and
//! `GET /v1/collections/{id}` (issue #262), and the collection a movie's
//! detail names: the real router, handlers and services over in-memory
//! stores, seeded through the collection store as enrichment writes it.

use std::sync::Arc;

use beam_auth::utils::session_store::SessionData;
use beam_domain::models::collection::{Collection as StoredCollection, CollectionSummary};
use beam_domain::models::movie::CreateMovie;
use beam_domain::providers::enrichment::{CollectionEnrichment, ExternalMediaRef};
use beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository;
use beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::{CollectionRepository, MovieRepository};
use beam_domain::services::TestClock;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::{CollectionConnection, CollectionDetail, MediaMetadata};
use crate::routes::collections::{get_collection, list_collections};
use crate::routes::media::get_media_detail;
use crate::routes::test_support::make_app_state;
use crate::services::collections::CollectionService;
use crate::services::metadata::{DbMetadataService, MetadataRepositories, MetadataService};
use crate::services::playback::{DbPlaybackService, PlaybackRepositories, PlaybackService};
use crate::services::sources::SourceCatalog;
use crate::state::{AppServices, AppState};

struct Fixture {
    state: AppState,
    movies: Arc<InMemoryMovieRepository>,
    collections: Arc<dyn CollectionRepository>,
}

fn fixture() -> Fixture {
    fixture_over(Arc::new(InMemoryCollectionRepository::default()))
}

fn fixture_over(collections: Arc<dyn CollectionRepository>) -> Fixture {
    let base = make_app_state();
    let clock = Arc::new(TestClock::new());
    let files = Arc::new(InMemoryFileRepository::default());
    let movies = Arc::new(InMemoryMovieRepository::with_files(files.clone()));
    let shows = Arc::new(InMemoryShowRepository::with_files(files.clone()));
    let genres = Arc::new(InMemoryGenreRepository::default());
    let sources = Arc::new(SourceCatalog::new(
        movies.clone(),
        files.clone(),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(
            beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository::default(),
        ),
    ));
    let playback: Arc<dyn PlaybackService> =
        Arc::new(DbPlaybackService::new(PlaybackRepositories {
            watch_state: Arc::new(
                beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::new(
                    clock,
                ),
            ),
            files: files.clone(),
            movies: movies.clone(),
            shows: shows.clone(),
            sources: sources.clone(),
        }));
    let metadata: Arc<dyn MetadataService> =
        Arc::new(DbMetadataService::new(MetadataRepositories {
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
            collections: collections.clone(),
        }));

    let services = AppServices {
        hash: base.services.hash.clone(),
        library: base.services.library.clone(),
        metadata: metadata.clone(),
        subtitles: base.services.subtitles.clone(),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        playback: playback.clone(),
        genre_repo: genres,
        library_repo: base.services.library_repo.clone(),
        file_repo: files,
        enrichment_repo: base.services.enrichment_repo.clone(),
        enrichment_control: base.services.enrichment_control.clone(),
        people: crate::routes::test_support::idle_people(movies.clone(), shows.clone()),
        lists: crate::routes::test_support::idle_lists(
            movies.clone(),
            shows.clone(),
            metadata.clone(),
            playback.clone(),
        ),
        collections: Arc::new(CollectionService::new(
            collections.clone(),
            metadata,
            playback,
        )),
        movie_repo: movies.clone(),
        show_repo: shows,
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
        movies,
        collections,
    }
}

fn client(fixture: &Fixture) -> TestClient<AppState> {
    let service = Router::new()
        .nest(
            "/v1",
            Router::new().mount(kynos::routes![
                list_collections,
                get_collection,
                get_media_detail
            ]),
        )
        .build(fixture.state.clone())
        .expect("the collections router describes itself");
    TestClient::new(service)
}

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

fn collection(native: &str, name: &str) -> CollectionEnrichment {
    CollectionEnrichment {
        collection_ref: ExternalMediaRef::new("tmdb", native),
        name: name.to_string(),
        poster_url: Some(format!("https://image.example/{native}.jpg")),
        backdrop_url: None,
    }
}

impl Fixture {
    /// A movie released on `released` (`YYYY-MM-DD`), or in no known year
    /// for `None`, in `collection` when one is given.
    async fn movie(
        &self,
        title: &str,
        released: Option<&str>,
        collection: Option<&CollectionEnrichment>,
    ) -> Uuid {
        let release_date =
            released.map(|day| chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap());
        let year = release_date.map(|date| chrono::Datelike::year(&date) as u32);
        let id = self
            .movies
            .find_or_create_by_identity(CreateMovie::new(title, year, None))
            .await
            .unwrap()
            .id;
        if let Some(stored) = self.movies.movies.lock().unwrap().get_mut(&id) {
            stored.release_date = release_date;
        }
        if collection.is_some() {
            self.collections
                .set_movie_collection(id, collection)
                .await
                .unwrap();
        }
        id
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &TestClient<AppState>,
    token: &str,
    path: &str,
) -> T {
    let response = client.get(path).cookie("beam_session", token).send().await;
    assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
    response.json()
}

#[tokio::test]
async fn the_listing_holds_each_collection_with_movies_by_name_a_page_at_a_time() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let matrix = collection("2344", "The Matrix Collection");
    fixture
        .movie("The Matrix", Some("1999-03-31"), Some(&matrix))
        .await;
    fixture
        .movie("The Matrix Reloaded", Some("2003-05-15"), Some(&matrix))
        .await;
    fixture
        .movie(
            "Alien",
            Some("1979-05-25"),
            Some(&collection("8091", "Alien Collection")),
        )
        .await;
    fixture.movie("Arrival", Some("2016-11-11"), None).await;

    let first: CollectionConnection = get_json(&client, &token, "/v1/collections?first=1").await;
    assert_eq!(first.total, 2);
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].name, "Alien Collection");
    assert_eq!(first.items[0].movie_count, 1);
    assert!(first.page_info.has_next_page);
    assert!(!first.page_info.has_previous_page);

    let cursor = first
        .page_info
        .end_cursor
        .expect("a cursor for the next page");
    let second: CollectionConnection = get_json(
        &client,
        &token,
        &format!("/v1/collections?first=1&after={cursor}"),
    )
    .await;
    let matrix = &second.items[0];
    assert_eq!(matrix.name, "The Matrix Collection");
    assert_eq!(matrix.movie_count, 2);
    assert_eq!(
        matrix.poster_url.as_deref(),
        Some(format!("/v1/artwork/collection/{}/poster", matrix.id).as_str()),
        "art is served by Beam, never by the provider"
    );
    assert_eq!(matrix.backdrop_url, None, "the provider has no backdrop");
    assert!(!second.page_info.has_next_page);
    assert!(second.page_info.has_previous_page);
}

#[tokio::test]
async fn a_collection_lists_its_movies_in_release_order() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let saga = collection("10", "Saga");
    let undated = fixture.movie("Saga: Lost Chapter", None, Some(&saga)).await;
    let third = fixture
        .movie("Saga III", Some("2005-05-19"), Some(&saga))
        .await;
    let first = fixture
        .movie("Saga I", Some("1999-05-19"), Some(&saga))
        .await;
    let second = fixture
        .movie("Saga II", Some("2002-05-16"), Some(&saga))
        .await;
    let id = fixture
        .collections
        .movie_collection(first)
        .await
        .unwrap()
        .expect("the saga")
        .id;

    let detail: CollectionDetail =
        get_json(&client, &token, &format!("/v1/collections/{id}")).await;
    assert_eq!(detail.id, id);
    assert_eq!(detail.name, "Saga");
    let order: Vec<Uuid> = detail.movies.iter().map(|movie| movie.id).collect();
    assert_eq!(
        order,
        [first, second, third, undated],
        "oldest first, a movie with no date last"
    );
    assert!(
        detail.movies.iter().all(|movie| !movie.user_state.played),
        "each movie carries the viewer's state"
    );
}

#[tokio::test]
async fn a_movies_detail_names_its_collection() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let collected = fixture
        .movie(
            "The Matrix",
            Some("1999-03-31"),
            Some(&collection("2344", "The Matrix Collection")),
        )
        .await;
    let alone = fixture.movie("Arrival", Some("2016-11-11"), None).await;

    let MediaMetadata::Movie(movie) =
        get_json(&client, &token, &format!("/v1/media/{collected}")).await
    else {
        panic!("a movie's detail is a movie");
    };
    let named = movie.collection.expect("the movie's collection");
    assert_eq!(named.name, "The Matrix Collection");
    let detail: CollectionDetail =
        get_json(&client, &token, &format!("/v1/collections/{}", named.id)).await;
    assert_eq!(detail.movies.len(), 1);

    let MediaMetadata::Movie(movie) =
        get_json(&client, &token, &format!("/v1/media/{alone}")).await
    else {
        panic!("a movie's detail is a movie");
    };
    assert!(
        movie.collection.is_none(),
        "a movie in no collection names none"
    );
}

#[tokio::test]
async fn a_collection_no_movie_is_in_any_more_is_not_found() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture
        .movie(
            "Leaver",
            Some("2000-01-01"),
            Some(&collection("1", "Abandoned")),
        )
        .await;
    let id = fixture
        .collections
        .movie_collection(movie)
        .await
        .unwrap()
        .unwrap()
        .id;
    fixture
        .collections
        .set_movie_collection(movie, None)
        .await
        .unwrap();

    for path in [
        format!("/v1/collections/{id}"),
        format!("/v1/collections/{}", Uuid::new_v4()),
    ] {
        client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND)
            .assert_problem_type(
                "https://beam.justinchung.net/reference/errors/#collection-not-found",
            );
    }
    let listing: CollectionConnection = get_json(&client, &token, "/v1/collections").await;
    assert!(listing.items.is_empty());
    assert_eq!(listing.total, 0);
}

#[tokio::test]
async fn a_cursor_the_listing_did_not_issue_is_a_400_invalid_cursor() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let people = crate::services::offset_cursor::encode(
        crate::services::offset_cursor::OffsetList::People,
        0,
    );

    for cursor in [people.as_str(), "bm90LWEtY3Vyc29y"] {
        client
            .get(&format!("/v1/collections?after={cursor}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-cursor");
    }
}

#[tokio::test]
async fn both_collection_routes_need_a_session() {
    let fixture = fixture();
    let client = client(&fixture);
    for path in [
        "/v1/collections".to_string(),
        format!("/v1/collections/{}", Uuid::new_v4()),
    ] {
        client
            .get(&path)
            .send()
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}

/// A collection store that fails every read: what a database outage looks
/// like from the service.
#[derive(Debug)]
struct FailingCollections;

fn outage() -> DbErr {
    DbErr::Custom("collection store unavailable".to_string())
}

#[async_trait::async_trait]
impl CollectionRepository for FailingCollections {
    async fn set_movie_collection(
        &self,
        _: Uuid,
        _: Option<&CollectionEnrichment>,
    ) -> Result<(), DbErr> {
        Err(outage())
    }
    async fn movie_collection(&self, _: Uuid) -> Result<Option<StoredCollection>, DbErr> {
        Err(outage())
    }
    async fn find_by_id(&self, _: Uuid) -> Result<Option<StoredCollection>, DbErr> {
        Err(outage())
    }
    async fn movie_ids(&self, _: Uuid) -> Result<Vec<Uuid>, DbErr> {
        Err(outage())
    }
    async fn list(&self, _: u64, _: u64) -> Result<Vec<CollectionSummary>, DbErr> {
        Err(outage())
    }
    async fn count(&self) -> Result<u64, DbErr> {
        Err(outage())
    }
}

#[tokio::test]
async fn a_collection_store_failure_is_a_500_never_an_empty_page_or_a_404() {
    let fixture = fixture_over(Arc::new(FailingCollections));
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture.movie("Arrival", Some("2016-11-11"), None).await;

    for path in [
        "/v1/collections".to_string(),
        format!("/v1/collections/{}", Uuid::new_v4()),
        format!("/v1/media/{movie}"),
    ] {
        client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
    }
}
