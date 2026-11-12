//! Subcutaneous tests for the `/v1/media` browse route.
//!
//! Same harness as `media_tests.rs`, whose fixtures these share: the real
//! router, handlers and session scheme over either the stub metadata service or
//! the real one reading in-memory repository doubles.

use std::sync::Arc;

use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;

use super::media_tests::{
    Library, MOVIE_ID, StubMetadataService, client, indexed_movie, indexed_movie_of, real_service,
    real_service_over, seed_session, signed_in_to, state_over_access, state_with,
    state_with_service,
};
use crate::models::MediaMetadata;
use crate::models::search::MediaConnection;
use crate::routes::test_support::make_app_state;

// ── GET /v1/media ────────────────────────────────────────────────────────────

#[tokio::test]
async fn browsing_requires_a_session() {
    let client = client(state_with(StubMetadataService::default()));

    let response = client.get("/v1/media").send().await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// `sort_by` and `sort_order` reach the browse order over the real metadata
/// service: the same three titles come back in title order, in reverse title
/// order, and in release order both ways -- four different orders, because
/// the years (Blade 1990, Contact 1997, Arrival 2016) follow neither title
/// order, so a parameter dropped on the way would show as one of the others.
#[tokio::test]
async fn sort_parameters_reach_the_browse_order() {
    let library = Library::new();
    let blade = indexed_movie_of(&library.movies, &library.files, "Blade", Some(1990))
        .await
        .0;
    let arrival = indexed_movie_of(&library.movies, &library.files, "Arrival", Some(2016))
        .await
        .0;
    let contact = indexed_movie_of(&library.movies, &library.files, "Contact", Some(1997))
        .await
        .0;
    let (client, token) = signed_in_to(real_service(library)).await;

    let order = |query: &'static str| {
        let client = &client;
        let token = token.clone();
        async move {
            let response = client
                .get(&format!("/v1/media{query}"))
                .cookie("beam_session", &token)
                .send()
                .await;
            assert_eq!(response.status(), StatusCode::OK, "{query}");
            response
                .json::<MediaConnection>()
                .items
                .iter()
                .map(item_id)
                .collect::<Vec<_>>()
        }
    };

    assert_eq!(order("").await, [arrival, blade, contact], "the default");
    assert_eq!(
        order("?sort_by=title&sort_order=desc").await,
        [contact, blade, arrival]
    );
    assert_eq!(
        order("?sort_by=year&sort_order=asc").await,
        [blade, contact, arrival]
    );
    assert_eq!(
        order("?sort_by=year&sort_order=desc").await,
        [arrival, contact, blade]
    );
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
        people: Arc::default(),
        streams: Arc::default(),
        sidecars: Arc::default(),
        chapters: Arc::default(),
        trickplay: Arc::default(),
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

fn item_id(item: &MediaMetadata) -> uuid::Uuid {
    match item {
        MediaMetadata::Movie(movie) => movie.id,
        MediaMetadata::Show(show) => show.id,
    }
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

/// `sort_by=relevance` lists the best match first and pages on through its
/// own cursors, the cursor carrying the distance it was issued at (#247).
#[tokio::test]
async fn a_relevance_sort_lists_the_best_match_first_and_pages_by_cursor() {
    let library = Library::new();
    let mut expected = Vec::new();
    for title in ["Matrix", "Matrix Reloaded", "The Matrix"] {
        expected.push(
            indexed_movie(&library.movies, &library.files, title)
                .await
                .0,
        );
    }
    indexed_movie(&library.movies, &library.files, "Arrival").await;
    let (client, token) = signed_in_to(real_service(library)).await;

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut url = "/v1/media?query=matrix&sort_by=relevance&first=1".to_string();
        if let Some(cursor) = &after {
            url.push_str(&format!("&after={cursor}"));
        }
        let response = client.get(&url).cookie("beam_session", &token).send().await;
        assert_eq!(response.status(), StatusCode::OK);
        let page: MediaConnection = response.json();
        seen.extend(page.items.iter().map(item_id));
        if !page.page_info.has_next_page {
            break;
        }
        after = page.page_info.end_cursor;
    }
    assert_eq!(seen, expected, "best match first, Arrival not at all");

    // A relevance cursor pages only a relevance listing.
    let response = client
        .get(&format!(
            "/v1/media?query=matrix&sort_by=title&after={}",
            after.expect("a cursor past the first page")
        ))
        .cookie("beam_session", &token)
        .send()
        .await;
    response
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-cursor");
}

/// Relevance is closeness to a `query`; asked for without one it is the
/// caller's error, not an arbitrary order and not the store's 500.
#[tokio::test]
async fn a_relevance_sort_without_a_query_is_a_400_invalid_search_query() {
    let library = Library::new();
    indexed_movie(&library.movies, &library.files, "Matrix").await;
    let (client, token) = signed_in_to(real_service(library)).await;

    client
        .get("/v1/media?sort_by=relevance")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-search-query");
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

    for query in ["first=2&last=2", "after=a&before=b"] {
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

/// A page size outside its published 1-100 is refused by Kynos's
/// `QueryParams` check before the handler runs, naming the parameter.
#[tokio::test]
async fn a_page_size_outside_its_published_bounds_is_refused_at_the_edge() {
    let (client, token) = signed_in_to(real_service(Library::new())).await;

    for (query, parameter) in [
        ("first=101", "first"),
        ("first=0", "first"),
        ("last=101", "last"),
    ] {
        let response = client
            .get(&format!("/v1/media?{query}"))
            .cookie("beam_session", &token)
            .send()
            .await;
        response
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type("about:blank");
        let body = response.json::<serde_json::Value>();
        assert_eq!(
            body["detail"],
            format!("query parameter `{parameter}` does not satisfy its schema"),
            "{query}"
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

/// NFR-108: a 500's `detail` is one fixed sentence, never its cause. The
/// cause -- here a database driver's message -- goes to the log instead, so
/// the operator still sees what failed.
#[tokio::test]
async fn an_internal_failure_never_echoes_its_cause() {
    const CAUSE: &str = "connection to 10.0.0.7:5432 reset by peer";
    fn failing<T>() -> Result<T, sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(CAUSE.to_string()))
    }

    let library = Library::new();
    let mut catalog = beam_domain::repositories::catalog::MockCatalogRepository::new();
    catalog.expect_browse().returning(|_| failing());
    let browse = signed_in_to(real_service_over(
        &library,
        Arc::new(catalog),
        library.movies.clone(),
    ))
    .await;

    let library = Library::new();
    let mut movies = beam_domain::repositories::movie::MockMovieRepository::new();
    movies.expect_find_by_id().returning(|_| failing());
    let read = signed_in_to(real_service_over(
        &library,
        Arc::new(beam_domain::repositories::catalog::MockCatalogRepository::new()),
        Arc::new(movies),
    ))
    .await;

    let logs: Arc<std::sync::Mutex<Vec<u8>>> = Arc::default();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer({
                let logs = logs.clone();
                move || LogWriter(logs.clone())
            })
            .finish(),
    );

    for (route, (client, token), path) in [
        ("browse", browse, "/v1/media".to_owned()),
        ("title", read, format!("/v1/media/{MOVIE_ID}")),
    ] {
        logs.lock().unwrap().clear();
        let response = client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        response
            .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
        let body = response.json::<serde_json::Value>();
        assert_eq!(
            body["detail"],
            crate::routes::api_error::INTERNAL_DETAIL,
            "{route}"
        );
        assert!(
            !body.to_string().contains("10.0.0.7"),
            "{route}: the cause reached the wire: {body}"
        );
        let logged = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains(CAUSE),
            "{route}: the cause is logged for the operator: {logged}"
        );
    }
}

/// An access-policy store that fails every read with a cause naming a host.
#[derive(Debug)]
struct FailingPolicies;

const POLICY_CAUSE: &str = "policy read from 10.0.0.9:5432 timed out";

#[async_trait::async_trait]
impl beam_domain::repositories::AccessPolicyRepository for FailingPolicies {
    async fn find(
        &self,
        _user_id: uuid::Uuid,
    ) -> Result<beam_domain::models::access::AccessPolicy, sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(POLICY_CAUSE.to_owned()))
    }

    async fn replace(
        &self,
        _user_id: uuid::Uuid,
        _policy: &beam_domain::models::access::AccessPolicy,
    ) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(POLICY_CAUSE.to_owned()))
    }
}

/// NFR-108 across every route module that reads the caller's access policy:
/// genres, media, people, collections, artwork, stream, subtitle, trickplay,
/// playback, lists and the library reads. Each answers the fixed `detail`,
/// keeps the cause off the wire, and logs it exactly once -- a second
/// `ServerFault` wrapped around the one the access lookup built would log a
/// second event carrying only the fixed sentence.
#[tokio::test]
async fn an_access_lookup_failure_never_echoes_its_cause_on_any_route() {
    let library = Library::new();
    let access = Arc::new(crate::services::access::AccessService::new(
        crate::services::access::AccessRepositories {
            policies: Arc::new(FailingPolicies),
            movies: library.movies.clone(),
            shows: library.shows.clone(),
            files: library.files.clone(),
        },
    ));
    let state = state_over_access(make_app_state(), real_service(library), access);
    let token = seed_session(&state).await;
    let client = TestClient::new(
        Router::new()
            .nest("/v1", crate::routes::rest_routes())
            .build(state)
            .expect("the REST router describes itself"),
    );

    let logs: Arc<std::sync::Mutex<Vec<u8>>> = Arc::default();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer({
                let logs = logs.clone();
                move || LogWriter(logs.clone())
            })
            .finish(),
    );

    let id = uuid::Uuid::new_v4();
    for path in [
        "/v1/genres".to_owned(),
        "/v1/media".to_owned(),
        format!("/v1/media/{id}"),
        format!("/v1/media/{id}/sources"),
        format!("/v1/episodes/{id}"),
        format!("/v1/seasons/{id}"),
        "/v1/people?query=keanu".to_owned(),
        format!("/v1/people/{id}"),
        "/v1/collections".to_owned(),
        format!("/v1/collections/{id}"),
        format!("/v1/artwork/movie/{id}/poster"),
        format!("/v1/files/{id}/stream"),
        format!("/v1/files/{id}/download"),
        format!("/v1/files/{id}/subtitles/{id}"),
        format!("/v1/files/{id}/trickplay/0"),
        format!("/v1/media/{id}/progress"),
        "/v1/continue-watching".to_owned(),
        "/v1/history".to_owned(),
        "/v1/watchlist".to_owned(),
        "/v1/favourites".to_owned(),
        "/v1/libraries".to_owned(),
        format!("/v1/libraries/{id}"),
        format!("/v1/libraries/{id}/files"),
    ] {
        logs.lock().unwrap().clear();
        let response = client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        response
            .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
        let body = response.json::<serde_json::Value>();
        assert_eq!(
            body["detail"],
            crate::routes::api_error::INTERNAL_DETAIL,
            "{path}"
        );
        assert!(
            !body.to_string().contains("10.0.0.9"),
            "{path}: the cause reached the wire: {body}"
        );
        let logged = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        assert_eq!(
            logged.matches(POLICY_CAUSE).count(),
            1,
            "{path}: the cause is logged once for the operator: {logged}"
        );
        assert!(
            !logged.contains(crate::routes::api_error::INTERNAL_DETAIL),
            "{path}: an event recorded only the fixed sentence: {logged}"
        );
    }
}

/// A `tracing` writer appending to a shared buffer.
struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
    // Tagged beside its fields, not wrapped under a `Show` key (wire rule R9).
    let show = &body["items"][0];
    assert_eq!(show["media_type"], "show");
    assert_eq!(show["id"], id.to_string());
    assert_eq!(show["genres"], serde_json::json!(["Thriller"]));
    // On TMDB's own 0-10 scale, and the decimal it was stored as rather than
    // the single-precision float's exact expansion (8.399999618530273).
    assert_eq!(show["ratings"]["tmdb"], 8.4);
    assert_eq!(show["identifiers"]["tmdb_id"], 95396);
    assert_eq!(show["season_count"], 1);
    assert_eq!(show["episode_count"], 1);
    assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
}

/// `min_rating` is on the scale `Ratings.tmdb` is shown on, and a title rated
/// exactly the minimum is kept: the bound is compared in the precision the
/// rating is stored in, so 8.4 is not 8.39999... short of itself.
#[tokio::test]
async fn min_rating_filters_on_the_scale_the_rating_is_shown_on() {
    let library = Library::new();
    let id = library.indexed_show("Severance").await;
    let (client, token) = signed_in_to(real_service(library)).await;

    for (query, expected) in [
        ("min_rating=8.4", vec![id]),
        ("min_rating=8.5", vec![]),
        ("min_rating=0", vec![id]),
    ] {
        let body: MediaConnection = client
            .get(&format!("/v1/media?{query}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::OK)
            .json();
        let listed: Vec<uuid::Uuid> = body.items.iter().map(item_id).collect();
        assert_eq!(listed, expected, "{query}");
    }
}

/// A numeric filter outside the range it can take is refused, never bound.
/// `year_from=4294967295` used to reach the store as `-1` (a `u32` cast to
/// the column's `i32`) and match every title; a `min_rating` off the 0-10
/// scale, `NaN` included, would match everything or nothing silently.
#[tokio::test]
async fn an_out_of_range_numeric_filter_is_a_400_invalid_filter() {
    let library = Library::new();
    library.indexed_show("Severance").await;
    let (client, token) = signed_in_to(real_service(library)).await;

    for query in [
        "year=4294967295",
        "year_from=4294967295",
        "year_to=2147483648",
    ] {
        client
            .get(&format!("/v1/media?{query}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-filter");
    }

    // A rating outside its published 0-10 is Kynos's `QueryParams` check
    // answering, before the handler's own -- `NaN` too, which satisfies no
    // bound.
    for query in [
        "min_rating=10.5",
        "min_rating=-1",
        "min_rating=inf",
        "min_rating=NaN",
    ] {
        client
            .get(&format!("/v1/media?{query}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type("about:blank");
    }

    // The largest bounds each filter can take are answered, not refused.
    for query in ["year_from=2147483647", "min_rating=10"] {
        client
            .get(&format!("/v1/media?{query}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::OK);
    }
}
