//! The administrator's enrichment routes (issue #185) through the served
//! router: real handlers over the real `EnrichmentControl`, with in-memory
//! stores and the in-memory provider below the trait line. Each test asserts
//! the response and what it changed in the stores.

use std::sync::Arc;

use beam_auth::utils::models::CreateUser;
use beam_auth::utils::session_store::SessionData;
use beam_domain::models::enrichment::{EnrichmentStatus, EnrichmentTargetId, MetadataField};
use beam_domain::models::{CreateLibrary, CreateMovie, CreateShow, PinSource};
use beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider;
use beam_domain::providers::enrichment::{EnrichmentError, ExternalMediaRef, MovieSearchHit};
use chrono::Utc;
use kynos::http::StatusCode;
use kynos::test::TestClient;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::models::{MediaEnrichment, MediaEnrichmentConnection};
use crate::state::AppState;

const VALIDATION_FAILED: &str = "https://beam.justinchung.net/reference/errors/#validation-failed";

struct Fixture {
    client: TestClient<AppState>,
    state: AppState,
    admin: String,
}

async fn fixture(provider: InMemoryEnrichmentProvider) -> Fixture {
    let state = crate::routes::test_support::make_app_state_with_enrichment(Arc::new(provider));
    let router = crate::routes::create_router()
        .build(state.clone())
        .expect("the served router describes itself");
    let admin = session(&state, true).await;
    Fixture {
        client: TestClient::new(router),
        state,
        admin,
    }
}

/// Seeds a user and a session and returns the `beam_session` cookie value.
async fn session(state: &AppState, is_admin: bool) -> String {
    let user = state
        .services
        .user_repo
        .create(CreateUser {
            oidc_issuer: "https://test.example".to_string(),
            oidc_subject: format!("subject-{is_admin}"),
            email: None,
            display_name: "Someone".to_string(),
            avatar_url: None,
            is_admin,
        })
        .await
        .unwrap();
    state
        .services
        .session_store
        .create(
            &SessionData {
                user_id: user.id.to_string(),
                device_hash: "test-device".to_string(),
                ip: "127.0.0.1".to_string(),
                created_at: Utc::now().timestamp(),
                last_active: Utc::now().timestamp(),
            },
            86400,
            86400,
        )
        .await
        .unwrap()
}

impl Fixture {
    /// A movie, queued.
    async fn movie(&self, title: &str, year: Option<u32>) -> Uuid {
        let movie = self
            .state
            .services
            .movie_repo
            .find_or_create_by_identity(CreateMovie::new(title.to_string(), year, None))
            .await
            .unwrap();
        self.state
            .services
            .enrichment_repo
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();
        movie.id
    }

    /// A show, queued.
    async fn show(&self, title: &str) -> Uuid {
        let show = self
            .state
            .services
            .show_repo
            .find_or_create_by_identity(CreateShow::new(title.to_string(), None))
            .await
            .unwrap();
        self.state
            .services
            .enrichment_repo
            .ensure_pending(EnrichmentTargetId::Show(show.id))
            .await
            .unwrap();
        show.id
    }

    async fn mark_unmatched(&self, target: EnrichmentTargetId, secs: i64) {
        let repo = &self.state.services.enrichment_repo;
        let row = repo.find_by_target(target).await.unwrap().unwrap();
        repo.mark_unmatched(
            row.id,
            "no candidate cleared the match threshold",
            chrono::DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap(),
        )
        .await
        .unwrap();
    }

    async fn row(&self, target: EnrichmentTargetId) -> beam_domain::models::EnrichmentState {
        self.state
            .services
            .enrichment_repo
            .find_by_target(target)
            .await
            .unwrap()
            .unwrap()
    }
}

fn hit(provider_ref: &str, title: &str, year: Option<u32>) -> MovieSearchHit {
    let (provider, native) = provider_ref.split_once(':').unwrap();
    MovieSearchHit {
        external_ref: ExternalMediaRef::new(provider, native),
        title: title.to_string(),
        original_title: None,
        year,
        popularity: None,
        vote_average: None,
    }
}

#[tokio::test]
async fn every_enrichment_route_is_for_administrators_only() {
    let f = fixture(InMemoryEnrichmentProvider::new(&["tmdb"])).await;
    let regular = session(&f.state, false).await;
    let id = f.movie("Heat", None).await;
    let routes = [
        ("GET", "/v1/admin/enrichment".to_string()),
        ("GET", format!("/v1/admin/media/{id}/enrichment")),
        ("GET", format!("/v1/admin/media/{id}/match-candidates")),
        ("POST", format!("/v1/admin/media/{id}/match")),
        ("DELETE", format!("/v1/admin/media/{id}/match")),
        ("PUT", format!("/v1/admin/media/{id}/enrichment/locks")),
        ("POST", "/v1/admin/media/refresh".to_string()),
        ("POST", format!("/v1/admin/libraries/{id}/refresh")),
    ];
    for (method, path) in &routes {
        let request = |cookie: Option<&str>| {
            let builder = match *method {
                "GET" => f.client.get(path),
                "PUT" => f.client.put(path).json(&json!({ "locked_fields": [] })),
                "DELETE" => f.client.delete(path),
                _ => f
                    .client
                    .post(path)
                    .json(&json!({ "external_ref": "tmdb:1" })),
            };
            match cookie {
                Some(cookie) => builder.cookie("beam_session", cookie),
                None => builder,
            }
        };
        assert_eq!(
            request(None).send().await.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
        assert_eq!(
            request(Some(&regular)).send().await.status(),
            StatusCode::FORBIDDEN,
            "{method} {path}"
        );
    }
    assert_eq!(
        f.row(EnrichmentTargetId::Movie(id)).await.status,
        EnrichmentStatus::Pending,
        "a refused request changed nothing"
    );
}

#[tokio::test]
async fn the_list_pages_unmatched_titles_newest_first_with_their_errors() {
    let f = fixture(InMemoryEnrichmentProvider::new(&["tmdb"])).await;
    let older = f.movie("Heat", Some(1995)).await;
    let newer = f.show("The Office").await;
    let _pending = f.movie("Alien", None).await;
    f.mark_unmatched(EnrichmentTargetId::Movie(older), 1).await;
    f.mark_unmatched(EnrichmentTargetId::Show(newer), 2).await;

    let first: MediaEnrichmentConnection = f
        .client
        .get("/v1/admin/enrichment?status=unmatched&first=1")
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(first.total, 2);
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].media_id, newer);
    assert_eq!(first.items[0].title, "The Office");
    assert_eq!(
        first.items[0].last_error.as_deref(),
        Some("no candidate cleared the match threshold")
    );
    assert!(first.page_info.has_next_page);
    assert!(!first.page_info.has_previous_page);

    let after = first.page_info.end_cursor.expect("a cursor to page on");
    let second: MediaEnrichmentConnection = f
        .client
        .get(&format!(
            "/v1/admin/enrichment?status=unmatched&first=1&after={after}"
        ))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].media_id, older);
    assert_eq!(second.items[0].year, Some(1995));
    assert!(!second.page_info.has_next_page);
    assert!(second.page_info.has_previous_page);

    let movies: MediaEnrichmentConnection = f
        .client
        .get("/v1/admin/enrichment?kind=movie")
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(movies.total, 2, "both movies, whatever their status");
}

#[tokio::test]
async fn the_list_refuses_a_page_it_cannot_serve() {
    let f = fixture(InMemoryEnrichmentProvider::new(&["tmdb"])).await;
    for (query, code) in [
        ("first=0", "invalid-pagination"),
        ("first=101", "invalid-pagination"),
        ("after=not-a-cursor", "invalid-cursor"),
    ] {
        f.client
            .get(&format!("/v1/admin/enrichment?{query}"))
            .cookie("beam_session", &f.admin)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type(&format!(
                "https://beam.justinchung.net/reference/errors/#{code}"
            ));
    }
    assert_eq!(
        f.client
            .get("/v1/admin/enrichment?status=lost")
            .cookie("beam_session", &f.admin)
            .send()
            .await
            .status(),
        StatusCode::BAD_REQUEST,
        "a status no title has"
    );
}

#[tokio::test]
async fn one_titles_enrichment_is_read_by_its_id() {
    let f = fixture(InMemoryEnrichmentProvider::new(&["tmdb"])).await;
    let id = f.movie("Heat", Some(1995)).await;
    f.mark_unmatched(EnrichmentTargetId::Movie(id), 1).await;

    let detail: MediaEnrichment = f
        .client
        .get(&format!("/v1/admin/media/{id}/enrichment"))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(detail.media_id, id);
    assert_eq!(detail.status, crate::models::EnrichmentStatus::Unmatched);
    assert_eq!(detail.kind, crate::models::MediaTypeFilter::Movie);
    assert!(detail.locked_fields.is_empty());

    f.client
        .get(&format!("/v1/admin/media/{}/enrichment", Uuid::new_v4()))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#media-not-found");
    assert_eq!(
        f.client
            .get("/v1/admin/media/not-a-uuid/enrichment")
            .cookie("beam_session", &f.admin)
            .send()
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn candidates_come_from_the_providers_search_best_first() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"]).with_movie_search(
        "Heat",
        vec![
            hit("tmdb:1", "Heat Wave", Some(1990)),
            hit("tmdb:949", "Heat", Some(1995)),
        ],
    );
    let f = fixture(provider).await;
    let id = f.movie("Heat", Some(1995)).await;

    let body: Value = f
        .client
        .get(&format!("/v1/admin/media/{id}/match-candidates"))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let refs: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["external_ref"].as_str().unwrap())
        .collect();
    assert_eq!(refs, vec!["tmdb:949", "tmdb:1"]);
    assert_eq!(body["page_info"]["has_next_page"], false);
}

#[tokio::test]
async fn candidates_say_when_no_provider_can_search_or_the_provider_fails() {
    let none = fixture(InMemoryEnrichmentProvider::new(&[])).await;
    let id = none.movie("Heat", None).await;
    none.client
        .get(&format!("/v1/admin/media/{id}/match-candidates"))
        .cookie("beam_session", &none.admin)
        .send()
        .await
        .assert_status(StatusCode::CONFLICT)
        .assert_problem_type(
            "https://beam.justinchung.net/reference/errors/#provider-not-configured",
        );

    let failing = fixture(
        InMemoryEnrichmentProvider::new(&["tmdb"])
            .with_search_error(|| EnrichmentError::RateLimited { retry_after: None }),
    )
    .await;
    let id = failing.movie("Heat", None).await;
    failing
        .client
        .get(&format!("/v1/admin/media/{id}/match-candidates?query=Heat"))
        .cookie("beam_session", &failing.admin)
        .send()
        .await
        .assert_status(StatusCode::BAD_GATEWAY)
        .assert_problem_type(
            "https://beam.justinchung.net/reference/errors/#enrichment-provider-error",
        );
}

#[tokio::test]
async fn fixing_a_match_pins_the_title_as_the_administrators_and_queues_it() {
    let f = fixture(InMemoryEnrichmentProvider::new(&["tmdb"])).await;
    let id = f.movie("Heat", Some(1995)).await;
    let target = EnrichmentTargetId::Movie(id);

    let detail: MediaEnrichment = f
        .client
        .post(&format!("/v1/admin/media/{id}/match"))
        .cookie("beam_session", &f.admin)
        .json(&json!({ "external_ref": "tmdb:949" }))
        .send()
        .await
        .assert_status(StatusCode::ACCEPTED)
        .json();

    assert_eq!(detail.pinned_ref.as_deref(), Some("tmdb:949"));
    assert_eq!(detail.pin_source, Some(crate::models::PinSource::Admin));
    let movie = f
        .state
        .services
        .movie_repo
        .find_by_id(id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(movie.pin_source, Some(PinSource::Admin));
    let row = f.row(target).await;
    assert_eq!(row.status, EnrichmentStatus::Pending);
    assert!(row.force_refresh);

    // Cleared: back to no pin (this fixture's titles have no NFO).
    let cleared: MediaEnrichment = f
        .client
        .delete(&format!("/v1/admin/media/{id}/match"))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::ACCEPTED)
        .json();
    assert_eq!((cleared.pinned_ref, cleared.pin_source), (None, None));
}

#[tokio::test]
async fn a_fix_match_is_refused_for_an_id_that_cannot_be_pinned() {
    let f = fixture(InMemoryEnrichmentProvider::new(&["anilist"])).await;
    let id = f.movie("Heat", None).await;
    let other = f.movie("Other", None).await;
    f.client
        .post(&format!("/v1/admin/media/{other}/match"))
        .cookie("beam_session", &f.admin)
        .json(&json!({ "external_ref": "anilist:5" }))
        .send()
        .await
        .assert_status(StatusCode::ACCEPTED);

    let invalid: Value = f
        .client
        .post(&format!("/v1/admin/media/{id}/match"))
        .cookie("beam_session", &f.admin)
        .json(&json!({ "external_ref": "603" }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_problem_type(VALIDATION_FAILED)
        .json();
    assert_eq!(invalid["errors"][0]["pointer"], "/external_ref");
    assert_eq!(
        f.client
            .post(&format!("/v1/admin/media/{id}/match"))
            .cookie("beam_session", &f.admin)
            .json(&json!({ "externalRef": "anilist:7" }))
            .send()
            .await
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "a body without the id is refused, never read as a clear"
    );

    for (external_ref, code) in [
        ("tmdb:603", "provider-not-configured"),
        ("anilist:5", "external-ref-taken"),
    ] {
        f.client
            .post(&format!("/v1/admin/media/{id}/match"))
            .cookie("beam_session", &f.admin)
            .json(&json!({ "external_ref": external_ref }))
            .send()
            .await
            .assert_status(StatusCode::CONFLICT)
            .assert_problem_type(&format!(
                "https://beam.justinchung.net/reference/errors/#{code}"
            ));
    }
    f.client
        .post(&format!("/v1/admin/media/{}/match", Uuid::new_v4()))
        .cookie("beam_session", &f.admin)
        .json(&json!({ "external_ref": "anilist:6" }))
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    let movie = f
        .state
        .services
        .movie_repo
        .find_by_id(id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(movie.pinned_ref, None, "no refusal pinned anything");
}

#[tokio::test]
async fn locks_are_replaced_whole_and_a_show_has_no_release_date_to_lock() {
    let f = fixture(InMemoryEnrichmentProvider::new(&["tmdb"])).await;
    let movie = f.movie("Heat", None).await;
    let show = f.show("The Office").await;

    let detail: MediaEnrichment = f
        .client
        .put(&format!("/v1/admin/media/{movie}/enrichment/locks"))
        .cookie("beam_session", &f.admin)
        .json(&json!({ "locked_fields": ["poster", "title", "runtime"] }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(
        detail.locked_fields,
        vec![
            crate::models::MetadataField::Title,
            crate::models::MetadataField::Runtime,
            crate::models::MetadataField::Poster,
        ]
    );
    let stored = f.row(EnrichmentTargetId::Movie(movie)).await.locked_fields;
    assert!(stored.is_locked(MetadataField::Runtime));
    assert!(!stored.is_locked(MetadataField::Genres));

    let refused: Value = f
        .client
        .put(&format!("/v1/admin/media/{show}/enrichment/locks"))
        .cookie("beam_session", &f.admin)
        .json(&json!({ "locked_fields": ["poster", "release_date"] }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_problem_type(VALIDATION_FAILED)
        .json();
    assert_eq!(refused["errors"][0]["pointer"], "/locked_fields/1");
    assert!(
        f.row(EnrichmentTargetId::Show(show))
            .await
            .locked_fields
            .is_empty()
    );
    assert_eq!(
        f.client
            .put(&format!("/v1/admin/media/{movie}/enrichment/locks"))
            .cookie("beam_session", &f.admin)
            .json(&json!({ "locked_fields": ["tmdb_id"] }))
            .send()
            .await
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "an id is not a lockable field"
    );
}

#[tokio::test]
async fn refreshes_queue_everything_or_one_librarys_titles() {
    let f = fixture(InMemoryEnrichmentProvider::new(&["tmdb"])).await;
    let library = f
        .state
        .services
        .library_repo
        .create(CreateLibrary {
            name: "Films".to_string(),
            root_path: "/films".into(),
            description: None,
        })
        .await
        .unwrap();
    let inside = f.movie("Heat", None).await;
    let outside = f.movie("Alien", None).await;
    f.state
        .services
        .movie_repo
        .ensure_library_association(library.id, inside)
        .await
        .unwrap();
    for id in [inside, outside] {
        f.mark_unmatched(EnrichmentTargetId::Movie(id), 1).await;
    }

    let queued: Value = f
        .client
        .post(&format!("/v1/admin/libraries/{}/refresh", library.id))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::ACCEPTED)
        .json();
    assert_eq!(queued["queued_count"], 1);
    assert_eq!(
        f.row(EnrichmentTargetId::Movie(inside)).await.status,
        EnrichmentStatus::Pending
    );
    assert_eq!(
        f.row(EnrichmentTargetId::Movie(outside)).await.status,
        EnrichmentStatus::Unmatched
    );

    f.client
        .post(&format!("/v1/admin/libraries/{}/refresh", Uuid::new_v4()))
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#library-not-found");

    let all: Value = f
        .client
        .post("/v1/admin/media/refresh")
        .cookie("beam_session", &f.admin)
        .send()
        .await
        .assert_status(StatusCode::ACCEPTED)
        .json();
    assert_eq!(all["queued_count"], 2);
    assert_eq!(
        f.row(EnrichmentTargetId::Movie(outside)).await.status,
        EnrichmentStatus::Pending
    );
}
