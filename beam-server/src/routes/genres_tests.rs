//! Subcutaneous tests for `GET /v1/genres`.
//!
//! The route reads only the injected `GenreRepository`, so these drive the
//! real handler through Kynos's in-process `TestClient` over a state built by
//! `test_support` -- no Redis, no PostgreSQL, no listener.

use beam_auth::utils::session_store::SessionData;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;

use crate::routes::genres::{GenreConnection, list_genres};
use crate::routes::test_support::make_app_state;
use crate::state::AppState;

/// The genres endpoint alone, plus the state its seeding goes through.
fn client(state: AppState) -> TestClient<AppState> {
    let service = Router::new()
        .nest("/v1", Router::new().mount(kynos::routes![list_genres]))
        .build(state)
        .expect("the genres router describes itself");

    TestClient::new(service)
}

/// Issues a session directly, bypassing the OIDC login flow -- which is not
/// what these tests are about.
///
/// No user row: `SessionAuth` carries no scope, so nothing resolves the id
/// against `user_repo`. A seeded user would only assert that the fixture
/// seeded a user.
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

#[tokio::test]
async fn listing_genres_requires_a_session() {
    let response = client(make_app_state()).get("/v1/genres").send().await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// NFR-205: a genre store that cannot read is a 500, never an empty catalogue
/// that tells a client the library has no genres.
#[tokio::test]
async fn a_genre_store_failure_is_a_500_never_an_empty_catalogue() {
    use crate::routes::test_support::{outage, signed_in, state_with};
    use beam_domain::repositories::genre::MockGenreRepository;

    let mut genres = MockGenreRepository::new();
    genres.expect_find_carried().returning(|_| Err(outage()));
    let state = state_with(|services| services.genre_repo = std::sync::Arc::new(genres));
    let token = signed_in(&state).await;

    let response = client(state)
        .get("/v1/genres")
        .cookie("beam_session", &token)
        .send()
        .await;
    response
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
    assert!(
        !response.text().contains("items"),
        "no catalogue is answered"
    );
}

#[tokio::test]
async fn the_catalog_is_deduplicated_and_sorted_case_insensitively() {
    let state = make_app_state();

    // Seeded out of order and with one name repeated across two titles in a
    // different case: the repo dedupes by slug and the endpoint sorts
    // case-insensitively.
    state
        .services
        .genre_repo
        .set_movie_genres(
            uuid::Uuid::new_v4(),
            &[
                "Science Fiction".to_owned(),
                "Action".to_owned(),
                "Drama".to_owned(),
            ],
        )
        .await
        .expect("seeding movie genres");
    state
        .services
        .genre_repo
        .set_show_genres(
            uuid::Uuid::new_v4(),
            &["Comedy".to_owned(), "action".to_owned()],
        )
        .await
        .expect("seeding show genres");

    let token = seed_session(&state).await;
    let response = client(state)
        .get("/v1/genres")
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body: GenreConnection = response.json();
    assert_eq!(
        body.items,
        vec![
            "Action".to_owned(),
            "Comedy".to_owned(),
            "Drama".to_owned(),
            "Science Fiction".to_owned(),
        ]
    );
}
