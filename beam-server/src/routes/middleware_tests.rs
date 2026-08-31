//! Subcutaneous tests for the same-origin enforcement itself.
//!
//! The unit tests beside `extract_origin` cover parsing in isolation; these
//! drive the interceptor against a real `AppState`, which is the only way to
//! reach the allow-list and the decision built on it. This is a CSRF control:
//! when it stops working, nothing observable changes until someone exploits it.

use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use kynos::test::TestClient;

use crate::routes::middleware::EnforceSameOrigin;
use crate::routes::test_support::make_app_state_with;
use crate::state::AppState;

/// A route that does nothing, so the only thing under test is the interceptor.
#[kynos::post("/thing", operation_id = "csrfProbe")]
async fn probe() -> NoContent {
    NoContent
}

/// The same route on a safe method, to prove those are never checked.
#[kynos::get("/thing", operation_id = "csrfProbeSafe")]
async fn probe_safe() -> NoContent {
    NoContent
}

fn client(extra_allowed_origins: Option<&str>) -> TestClient<AppState> {
    let state = make_app_state_with(|config| {
        config.web_url = "http://localhost:5173".to_owned();
        config.server_url = "http://localhost:3000".to_owned();
        config.extra_allowed_origins = extra_allowed_origins.map(str::to_owned);
    });

    let service = Router::new()
        .mount(kynos::routes![probe, probe_safe])
        .intercept(EnforceSameOrigin)
        .build(state)
        .expect("the probe router describes itself");

    TestClient::new(service)
}

async fn post_with(client: &TestClient<AppState>, header: &str, value: &str) -> StatusCode {
    client
        .post("/thing")
        .header(header, value)
        .send()
        .await
        .status()
}

#[tokio::test]
async fn the_configured_web_and_server_origins_are_both_accepted() {
    let client = client(None);

    for origin in ["http://localhost:5173", "http://localhost:3000"] {
        assert_eq!(
            post_with(&client, "Origin", origin).await,
            StatusCode::NO_CONTENT,
            "{origin} is configured and must be accepted"
        );
    }
}

#[tokio::test]
async fn an_unrecognised_origin_is_rejected() {
    let client = client(None);
    assert_eq!(
        post_with(&client, "Origin", "http://evil.example.com").await,
        StatusCode::FORBIDDEN
    );
}

/// The reason `extract_origin` compares whole origins rather than prefixes.
#[tokio::test]
async fn a_lookalike_host_is_rejected() {
    let client = client(None);
    assert_eq!(
        post_with(&client, "Origin", "http://localhost:5173.evil.example").await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn extra_allowed_origins_are_honoured_and_normalised() {
    // Trailing slashes and surrounding whitespace are what a human writing an
    // environment variable actually produces.
    let client = client(Some(" https://beam.example.com/ , https://tv.example.com "));

    for origin in ["https://beam.example.com", "https://tv.example.com"] {
        assert_eq!(
            post_with(&client, "Origin", origin).await,
            StatusCode::NO_CONTENT,
            "{origin} was configured and must be accepted"
        );
    }
}

#[tokio::test]
async fn referer_is_used_when_there_is_no_origin_header() {
    let client = client(None);
    assert_eq!(
        post_with(&client, "Referer", "http://localhost:5173/library/42?x=1").await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn a_malformed_header_is_rejected_rather_than_ignored() {
    let client = client(None);
    assert_eq!(
        post_with(&client, "Origin", "not-a-url").await,
        StatusCode::FORBIDDEN
    );
}

/// Non-browser clients send neither header and never carry the cookie, so
/// `SameSite=Lax` is what actually stops the attack this guards against.
#[tokio::test]
async fn a_request_with_no_origin_or_referer_is_allowed() {
    let client = client(None);
    let status = client.post("/thing").send().await.status();
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn safe_methods_are_never_checked() {
    let client = client(None);
    let status = client
        .get("/thing")
        .header("Origin", "http://evil.example.com")
        .send()
        .await
        .status();
    // `probe_safe` answers `NoContent`, so reaching the handler is a 204. What
    // matters is that it is not the interceptor's 403.
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// The interceptor's 403 is a declared response, not something it renders on
/// the side. Under Salvo this status existed only at run time (issue #123).
#[tokio::test]
async fn the_rejection_is_a_declared_problem_document() {
    let client = client(None);
    client
        .post("/thing")
        .header("Origin", "http://evil.example.com")
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/cross-origin");
}
