//! Subcutaneous tests for `GET /v1/people` and `GET /v1/people/{id}`: the
//! real router and handlers over the real `PeopleService`, with in-memory
//! stores below the repository traits.

use std::sync::Arc;

use beam_auth::utils::session_store::SessionData;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use uuid::Uuid;

use beam_domain::models::CreateMovie;
use beam_domain::models::person::CreditRole;
use beam_domain::providers::enrichment::{CreditEnrichment, ExternalMediaRef};
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::person::in_memory::InMemoryPersonRepository;
use beam_domain::repositories::{MovieRepository, PersonRepository};

use crate::models::{PersonConnection, PersonDetail};
use crate::routes::people::{get_person, search_people};
use crate::routes::test_support::make_app_state;
use crate::services::people::PeopleService;
use crate::state::{AppServices, AppState};

/// `test_support`'s state with the people service swapped for `people`.
fn state_with(people: PeopleService) -> AppState {
    let base = make_app_state();
    let services = AppServices {
        people: Arc::new(people),
        ..base.services.clone()
    };
    AppState::new(base.config.clone(), services, base.probe.clone(), None)
}

fn client(state: AppState) -> TestClient<AppState> {
    let service = Router::new()
        .nest(
            "/v1",
            Router::new().mount(kynos::routes![search_people, get_person]),
        )
        .build(state)
        .expect("the people router describes itself");
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

/// A library with one movie and its credits, and a signed-in client over it.
async fn credited_library() -> (
    TestClient<AppState>,
    String,
    Arc<InMemoryPersonRepository>,
    Uuid,
) {
    let movies = Arc::new(InMemoryMovieRepository::default());
    let people = Arc::new(InMemoryPersonRepository::default());
    let movie = movies
        .find_or_create_by_identity(CreateMovie::new("The Matrix", Some(1999), None))
        .await
        .unwrap()
        .id;
    let credit = |native: &str, name: &str, role: CreditRole| CreditEnrichment {
        person_ref: ExternalMediaRef::new("tmdb", native),
        name: name.to_string(),
        profile_url: Some(format!("https://image.example/{native}.jpg")),
        role,
    };
    people
        .set_movie_credits(
            movie,
            &[
                credit(
                    "6384",
                    "Keanu Reeves",
                    CreditRole::Cast {
                        character: Some("Neo".to_string()),
                        billing_order: 0,
                    },
                ),
                credit(
                    "9339",
                    "Lana Wachowski",
                    CreditRole::Crew {
                        job: "Director".to_string(),
                        department: Some("Directing".to_string()),
                    },
                ),
            ],
        )
        .await
        .unwrap();
    let state = state_with(PeopleService::new(
        people.clone(),
        movies,
        Arc::new(beam_domain::repositories::show::in_memory::InMemoryShowRepository::default()),
    ));
    let token = seed_session(&state).await;
    (client(state), token, people, movie)
}

#[tokio::test]
async fn people_search_and_detail_require_a_session() {
    let (client, _token, _people, _movie) = credited_library().await;
    for path in ["/v1/people", &format!("/v1/people/{}", Uuid::new_v4())] {
        client
            .get(path)
            .send()
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn a_search_finds_people_by_name_and_counts_them() {
    let (client, token, _people, _movie) = credited_library().await;

    let page: PersonConnection = client
        .get("/v1/people?query=keanu")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(page.total, 1);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "Keanu Reeves");
    let id = page.items[0].id;
    assert_eq!(
        page.items[0].profile_url,
        Some(format!("/v1/artwork/person/{id}/profile"))
    );
    assert!(!page.page_info.has_next_page);

    let everyone: PersonConnection = client
        .get("/v1/people?first=1")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(everyone.total, 2);
    assert_eq!(everyone.items[0].name, "Keanu Reeves");
    let cursor = everyone
        .page_info
        .end_cursor
        .expect("a page with a person has a cursor");
    let next: PersonConnection = client
        .get(&format!("/v1/people?first=1&after={cursor}"))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(next.items[0].name, "Lana Wachowski");
    assert!(!next.page_info.has_next_page);
}

#[tokio::test]
async fn a_cursor_people_search_did_not_issue_is_a_400() {
    let (client, token, _people, _movie) = credited_library().await;
    client
        .get("/v1/people?after=not-a-cursor")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-cursor");
}

#[tokio::test]
async fn a_person_lists_their_filmography() {
    let (client, token, people, movie) = credited_library().await;
    let director = people.movie_credits(movie).await.unwrap();
    let director = director
        .iter()
        .find(|c| c.person.name == "Lana Wachowski")
        .unwrap()
        .person
        .id;

    let detail: PersonDetail = client
        .get(&format!("/v1/people/{director}"))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(detail.name, "Lana Wachowski");
    assert!(detail.cast.is_empty());
    assert_eq!(detail.crew.len(), 1);
    assert_eq!(detail.crew[0].job, "Director");
    assert_eq!(detail.crew[0].title.id, movie);
    assert_eq!(detail.crew[0].title.title.original, "The Matrix");
    assert_eq!(detail.crew[0].title.year, Some(1999));
}

#[tokio::test]
async fn an_unknown_person_is_a_404_problem_document() {
    let (client, token, _people, _movie) = credited_library().await;
    client
        .get(&format!("/v1/people/{}", Uuid::new_v4()))
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#person-not-found");
}

#[tokio::test]
async fn a_failed_read_is_a_500_not_a_404_or_an_empty_page() {
    let state = state_with(crate::services::people::people_tests::failing_people_service());
    let token = seed_session(&state).await;
    let client = client(state);
    for path in [
        "/v1/people".to_string(),
        format!("/v1/people/{}", Uuid::new_v4()),
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
