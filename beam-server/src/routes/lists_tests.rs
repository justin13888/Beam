//! Subcutaneous tests for the watchlist and favourites (issue #262): adding,
//! removing, paging, and the flags a title's detail carries.
//!
//! The list, metadata and playback services are the real ones over
//! in-memory stores, because what these tests assert is how one request's
//! change comes back out of another.

use std::sync::Arc;
use std::time::Duration;

use beam_auth::utils::session_store::SessionData;
use beam_domain::models::movie::CreateMovie;
use beam_domain::models::show::{CreateEpisode, CreateShow};
use beam_domain::models::user_list::{UserList, UserListEntry, UserListPosition};
use beam_domain::models::watch_state::TitleRef;
use beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::user_list::InMemoryUserListRepository;
use beam_domain::repositories::{MovieRepository, ShowRepository, UserListRepository};
use beam_domain::services::TestClock;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::{MediaMetadata, UserListConnection};
use crate::routes::lists::{
    add_to_favourites, add_to_watchlist, get_favourites, get_watchlist, remove_from_favourites,
    remove_from_watchlist,
};
use crate::routes::media::get_media_detail;
use crate::routes::test_support::make_app_state;
use crate::services::lists::{ListDependencies, ListService};
use crate::services::metadata::{DbMetadataService, MetadataRepositories, MetadataService};
use crate::services::playback::{DbPlaybackService, PlaybackRepositories, PlaybackService};
use crate::services::sources::SourceCatalog;
use crate::state::{AppServices, AppState};

/// A state whose list, metadata and playback services are real, over the
/// stores kept here so a test can seed titles through them.
struct Fixture {
    state: AppState,
    movies: Arc<InMemoryMovieRepository>,
    shows: Arc<InMemoryShowRepository>,
    /// What `added_at` is stamped from, so a test orders additions by
    /// advancing it rather than by racing the wall clock.
    clock: Arc<TestClock>,
}

fn fixture() -> Fixture {
    let clock = Arc::new(TestClock::new());
    fixture_over(
        Arc::new(InMemoryUserListRepository::new(clock.clone())),
        clock,
    )
}

fn fixture_over(lists: Arc<dyn UserListRepository>, clock: Arc<TestClock>) -> Fixture {
    let base = make_app_state();
    let files = Arc::new(InMemoryFileRepository::default());
    let movies = Arc::new(InMemoryMovieRepository::with_files(files.clone()));
    let shows = Arc::new(InMemoryShowRepository::with_files(files.clone()));
    let genres = Arc::new(InMemoryGenreRepository::default());
    let sources = Arc::new(SourceCatalog::new(crate::services::sources::SourceRepositories {movies: movies.clone(), files: files.clone(), streams: Arc::new(InMemoryMediaStreamRepository::default()), sidecars: Arc::new(
            beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository::default(),
        ), chapters: Arc::new(beam_domain::repositories::chapter::in_memory::InMemoryChapterRepository::default()), trickplay: Arc::new(beam_domain::repositories::trickplay::in_memory::InMemoryTrickplayRepository::default()),}));
    let playback: Arc<dyn PlaybackService> =
        Arc::new(DbPlaybackService::new(PlaybackRepositories {
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
            collections: Arc::new(
                beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository::default(),
            ),
        }));
    let lists = Arc::new(ListService::new(ListDependencies {
        lists,
        movies: movies.clone(),
        shows: shows.clone(),
        metadata: metadata.clone(),
        playback: playback.clone(),
    }));

    let services = AppServices {
        hash: base.services.hash.clone(),
        access: base.services.access.clone(),
        library: base.services.library.clone(),
        metadata: metadata.clone(),
        subtitles: base.services.subtitles.clone(),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        language_preferences: base.services.language_preferences.clone(),
        playback: playback.clone(),
        genre_repo: genres,
        library_repo: base.services.library_repo.clone(),
        file_repo: files,
        enrichment_repo: base.services.enrichment_repo.clone(),
        enrichment_control: base.services.enrichment_control.clone(),
        people: crate::routes::test_support::idle_people(movies.clone(), shows.clone()),
        lists,
        collections: crate::routes::test_support::idle_collections(metadata, playback),
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
        trickplay: crate::routes::test_support::idle_trickplay(),
    };

    Fixture {
        state: AppState::new(base.config.clone(), services, base.probe.clone(), None),
        movies,
        shows,
        clock,
    }
}

fn client(fixture: &Fixture) -> TestClient<AppState> {
    let service = Router::new()
        .nest(
            "/v1",
            Router::new().mount(kynos::routes![
                add_to_watchlist,
                remove_from_watchlist,
                get_watchlist,
                add_to_favourites,
                remove_from_favourites,
                get_favourites,
                get_media_detail,
            ]),
        )
        .build(fixture.state.clone())
        .expect("the lists router describes itself");
    TestClient::new(service)
}

/// A session for a new user: every entry is keyed by the user, so one token
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
        self.movies
            .find_or_create_by_identity(CreateMovie::new(title, None, None))
            .await
            .unwrap()
            .id
    }

    async fn show(&self, title: &str) -> Uuid {
        self.shows
            .find_or_create_by_identity(CreateShow::new(title, None))
            .await
            .unwrap()
            .id
    }

    /// The first season of `show_id` and its first episode.
    async fn season_and_episode(&self, show_id: Uuid) -> (Uuid, Uuid) {
        let season = self.shows.find_or_create_season(show_id, 1).await.unwrap();
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
        (season.id, episode.id)
    }
}

async fn put(client: &TestClient<AppState>, token: &str, path: &str) -> StatusCode {
    client
        .put(path)
        .cookie("beam_session", token)
        .send()
        .await
        .status()
}

async fn delete(client: &TestClient<AppState>, token: &str, path: &str) -> StatusCode {
    client
        .delete(path)
        .cookie("beam_session", token)
        .send()
        .await
        .status()
}

async fn page(client: &TestClient<AppState>, token: &str, path: &str) -> UserListConnection {
    let response = client.get(path).cookie("beam_session", token).send().await;
    assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
    response.json()
}

fn ids(connection: &UserListConnection) -> Vec<Uuid> {
    connection
        .items
        .iter()
        .map(|item| match &item.media {
            MediaMetadata::Movie(movie) => movie.id,
            MediaMetadata::Show(show) => show.id,
        })
        .collect()
}

async fn detail(client: &TestClient<AppState>, token: &str, id: Uuid) -> MediaMetadata {
    let response = client
        .get(&format!("/v1/media/{id}"))
        .cookie("beam_session", token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
    response.json()
}

/// `(in_watchlist, in_favourites)` on a title's detail.
fn flags(media: &MediaMetadata) -> (Option<bool>, Option<bool>) {
    match media {
        MediaMetadata::Movie(movie) => (
            movie.user_state.in_watchlist,
            movie.user_state.in_favourites,
        ),
        MediaMetadata::Show(show) => {
            let state = show.user_state.as_ref().expect("a show's detail state");
            (state.in_watchlist, state.in_favourites)
        }
    }
}

#[tokio::test]
async fn titles_put_on_the_watchlist_list_newest_first_with_their_metadata() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture.movie("Arrival").await;
    let show = fixture.show("Dark").await;

    assert_eq!(
        put(&client, &token, &format!("/v1/watchlist/{movie}")).await,
        StatusCode::NO_CONTENT
    );
    fixture.clock.advance(Duration::from_secs(60));
    assert_eq!(
        put(&client, &token, &format!("/v1/watchlist/{show}")).await,
        StatusCode::NO_CONTENT
    );

    let watchlist = page(&client, &token, "/v1/watchlist").await;
    assert_eq!(ids(&watchlist), [show, movie], "newest addition first");
    assert_eq!(watchlist.total, 2);
    assert!(!watchlist.page_info.has_next_page);
    assert!(!watchlist.page_info.has_previous_page);
    assert_eq!(
        watchlist.items[1].added_at,
        watchlist.items[0].added_at - chrono::Duration::seconds(60)
    );
    let MediaMetadata::Movie(listed) = &watchlist.items[1].media else {
        panic!("the movie is listed as a movie");
    };
    assert_eq!(listed.title.original, "Arrival");
    assert!(
        page(&client, &token, "/v1/favourites")
            .await
            .items
            .is_empty(),
        "the other list is its own"
    );
}

#[tokio::test]
async fn adding_a_title_twice_keeps_it_once_where_it_was() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let (first, second) = (fixture.movie("Arrival").await, fixture.movie("Blade").await);

    put(&client, &token, &format!("/v1/favourites/{first}")).await;
    fixture.clock.advance(Duration::from_secs(1));
    put(&client, &token, &format!("/v1/favourites/{second}")).await;
    fixture.clock.advance(Duration::from_secs(1));
    assert_eq!(
        put(&client, &token, &format!("/v1/favourites/{first}")).await,
        StatusCode::NO_CONTENT
    );

    let favourites = page(&client, &token, "/v1/favourites").await;
    assert_eq!(ids(&favourites), [second, first]);
    assert_eq!(favourites.total, 2);
}

#[tokio::test]
async fn removing_takes_a_title_off_and_removing_it_again_is_no_change() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture.movie("Arrival").await;
    put(&client, &token, &format!("/v1/watchlist/{movie}")).await;
    put(&client, &token, &format!("/v1/favourites/{movie}")).await;

    for _ in 0..2 {
        assert_eq!(
            delete(&client, &token, &format!("/v1/watchlist/{movie}")).await,
            StatusCode::NO_CONTENT
        );
    }

    let watchlist = page(&client, &token, "/v1/watchlist").await;
    assert!(watchlist.items.is_empty());
    assert_eq!(watchlist.total, 0);
    assert_eq!(
        ids(&page(&client, &token, "/v1/favourites").await),
        [movie],
        "only the list named lost it"
    );
    assert_eq!(
        delete(&client, &token, &format!("/v1/favourites/{movie}")).await,
        StatusCode::NO_CONTENT
    );
    assert!(
        page(&client, &token, "/v1/favourites")
            .await
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn a_titles_detail_says_which_lists_it_is_on() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture.movie("Arrival").await;
    let show = fixture.show("Dark").await;

    assert_eq!(
        flags(&detail(&client, &token, movie).await),
        (Some(false), Some(false)),
        "present, and false, on a title on no list"
    );
    put(&client, &token, &format!("/v1/watchlist/{movie}")).await;
    put(&client, &token, &format!("/v1/favourites/{show}")).await;

    assert_eq!(
        flags(&detail(&client, &token, movie).await),
        (Some(true), Some(false))
    );
    assert_eq!(
        flags(&detail(&client, &token, show).await),
        (Some(false), Some(true))
    );

    let stranger = session(&fixture).await;
    assert_eq!(
        flags(&detail(&client, &stranger, movie).await),
        (Some(false), Some(false)),
        "one viewer's lists are not another's"
    );
    assert!(
        page(&client, &stranger, "/v1/watchlist")
            .await
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn a_season_or_an_episode_is_refused_as_not_a_title() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let show = fixture.show("Dark").await;
    let (season, episode) = fixture.season_and_episode(show).await;

    for id in [season, episode] {
        for path in [
            format!("/v1/watchlist/{id}"),
            format!("/v1/favourites/{id}"),
        ] {
            client
                .put(&path)
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::BAD_REQUEST)
                .assert_problem_type("https://beam.justinchung.net/reference/errors/#not-a-title");
            client
                .delete(&path)
                .cookie("beam_session", &token)
                .send()
                .await
                .assert_status(StatusCode::BAD_REQUEST)
                .assert_problem_type("https://beam.justinchung.net/reference/errors/#not-a-title");
        }
    }
    assert!(
        page(&client, &token, "/v1/watchlist")
            .await
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn an_id_naming_nothing_is_a_404_media_not_found() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let nothing = Uuid::new_v4();

    for request in [
        client.put(&format!("/v1/watchlist/{nothing}")),
        client.delete(&format!("/v1/favourites/{nothing}")),
    ] {
        request
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::NOT_FOUND)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#media-not-found");
    }
}

#[tokio::test]
async fn following_end_cursor_visits_every_title_once_in_order() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let mut expected = Vec::new();
    for title in ["Arrival", "Blade", "Contact"] {
        let movie = fixture.movie(title).await;
        put(&client, &token, &format!("/v1/watchlist/{movie}")).await;
        fixture.clock.advance(Duration::from_secs(1));
        expected.push(movie);
    }
    expected.reverse();

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let path = match &after {
            Some(cursor) => format!("/v1/watchlist?first=2&after={cursor}"),
            None => "/v1/watchlist?first=2".to_string(),
        };
        let connection = page(&client, &token, &path).await;
        assert_eq!(connection.page_info.has_previous_page, after.is_some());
        assert_eq!(connection.total, 3);
        seen.extend(ids(&connection));
        if !connection.page_info.has_next_page {
            break;
        }
        after = connection.page_info.end_cursor;
    }
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn one_lists_cursor_is_refused_by_the_other() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    for title in ["Arrival", "Blade"] {
        let movie = fixture.movie(title).await;
        put(&client, &token, &format!("/v1/watchlist/{movie}")).await;
    }
    let cursor = page(&client, &token, "/v1/watchlist?first=1")
        .await
        .page_info
        .end_cursor
        .expect("a cursor for the first page");

    for path in [
        format!("/v1/favourites?after={cursor}"),
        "/v1/watchlist?after=bm90LWEtY3Vyc29y".to_string(),
    ] {
        client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-cursor");
    }
}

#[tokio::test]
async fn every_list_operation_needs_a_session() {
    let fixture = fixture();
    let client = client(&fixture);
    let id = fixture.movie("Arrival").await;

    for response in [
        client.get("/v1/watchlist").send().await,
        client.put(&format!("/v1/watchlist/{id}")).send().await,
        client.delete(&format!("/v1/favourites/{id}")).send().await,
    ] {
        response.assert_status(StatusCode::UNAUTHORIZED);
    }
}

/// A list store that fails every call: what a database outage looks like
/// from the service.
#[derive(Debug)]
struct FailingLists;

fn outage() -> DbErr {
    DbErr::Custom("list store unavailable".to_string())
}

#[async_trait::async_trait]
impl UserListRepository for FailingLists {
    async fn add(&self, _: Uuid, _: UserList, _: TitleRef) -> Result<UserListEntry, DbErr> {
        Err(outage())
    }
    async fn remove(&self, _: Uuid, _: UserList, _: TitleRef) -> Result<bool, DbErr> {
        Err(outage())
    }
    async fn lists_of(&self, _: Uuid, _: TitleRef) -> Result<Vec<UserList>, DbErr> {
        Err(outage())
    }
    async fn find_page(
        &self,
        _: Uuid,
        _: UserList,
        _: Option<UserListPosition>,
        _: u64,
    ) -> Result<Vec<UserListEntry>, DbErr> {
        Err(outage())
    }
    async fn count(&self, _: Uuid, _: UserList) -> Result<u64, DbErr> {
        Err(outage())
    }
    async fn carry(&self, _: TitleRef, _: TitleRef) -> Result<(), DbErr> {
        Err(outage())
    }
}

#[tokio::test]
async fn a_list_store_failure_is_a_500_never_an_empty_list_or_a_missing_flag() {
    let fixture = fixture_over(Arc::new(FailingLists), Arc::new(TestClock::new()));
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture.movie("Arrival").await;

    for request in [
        client.get("/v1/watchlist"),
        client.put(&format!("/v1/watchlist/{movie}")),
        client.delete(&format!("/v1/favourites/{movie}")),
        client.get(&format!("/v1/media/{movie}")),
    ] {
        request
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
    }
}
