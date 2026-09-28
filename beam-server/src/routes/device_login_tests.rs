//! Subcutaneous tests for the device authorization grant (ADR-0017).
//!
//! These drive `POST /v1/auth/device` and `POST /v1/auth/device/token`
//! through the router the process serves -- `create_router`, with its
//! same-origin check and rate limiters -- over a `FakeOidcClient` and the
//! in-memory stores, all on one `TestClock`. No IdP, no Postgres (NFR-201).
//!
//! Every test asserts the response *and* what it left behind: which flows the
//! store still holds, how often the IdP was asked, and whether the session the
//! grant minted is one `GET /v1/me` accepts.

use std::sync::Arc;
use std::time::Duration;

use beam_auth::utils::device_auth_store::hash_handle;
use beam_auth::utils::device_auth_store::in_memory::InMemoryDeviceAuthStore;
use beam_auth::utils::oidc::fake::{
    FAKE_DEVICE_EXPIRES_IN_SECS, FAKE_DEVICE_INTERVAL_SECS, FAKE_USER_CODE, FakeOidcClient,
};
use beam_auth::utils::oidc::{
    DevicePoll, NotConfiguredOidcClient, OidcClient, OidcError, OidcIdentity,
};
use beam_auth::utils::oidc_config::OidcRuntimeConfig;
use beam_auth::utils::repository::UserRepository;
use beam_auth::utils::repository::in_memory::InMemoryUserRepository;
use beam_auth::utils::session_store::SessionStore;
use beam_auth::utils::session_store::in_memory::InMemorySessionStore;
use beam_domain::services::TestClock;
use kynos::http::StatusCode;
use kynos::test::{TestClient, TestResponse};
use serde_json::{Value, json};

use crate::routes::api_error::SESSION_COOKIE;
use crate::routes::auth::DEVICE_LOGIN_MAX_SECS;
use crate::routes::create_router;
use crate::routes::test_support::make_app_state_full;
use crate::services::health::InMemoryDependencyProbe;
use crate::state::{AppServices, AppState};

const ERRORS: &str = "https://beam.justinchung.net/reference/errors/";

/// A socket to send from when a test needs the rate limiter to see a client.
const PEER: &str = "203.0.113.7:41000";

struct Harness {
    client: TestClient<AppState>,
    oidc: Arc<FakeOidcClient>,
    flows: Arc<InMemoryDeviceAuthStore>,
    users: Arc<InMemoryUserRepository>,
    sessions: Arc<InMemorySessionStore>,
    clock: Arc<TestClock>,
    oidc_config: OidcRuntimeConfig,
}

fn identity(claims: Value) -> OidcIdentity {
    OidcIdentity {
        issuer: "https://dex.test".to_owned(),
        subject: "tv-user".to_owned(),
        email: Some("tv@example.com".to_owned()),
        email_verified: true,
        name: Some("TV User".to_owned()),
        picture: None,
        claims,
    }
}

fn harness(oidc: FakeOidcClient) -> Harness {
    harness_with(Arc::new(oidc), |_| {})
}

/// The served router over the given fake, with `adjust` applied to the
/// server configuration.
fn harness_with(
    oidc: Arc<FakeOidcClient>,
    adjust: impl FnOnce(&mut crate::config::ServerConfig),
) -> Harness {
    let oidc_dyn: Arc<dyn OidcClient> = oidc.clone();
    let built = build(oidc_dyn, adjust);
    Harness {
        client: built.0,
        oidc,
        flows: built.1,
        users: built.2,
        sessions: built.3,
        clock: built.4,
        oidc_config: built.5,
    }
}

type Built = (
    TestClient<AppState>,
    Arc<InMemoryDeviceAuthStore>,
    Arc<InMemoryUserRepository>,
    Arc<InMemorySessionStore>,
    Arc<TestClock>,
    OidcRuntimeConfig,
);

fn build(
    oidc: Arc<dyn OidcClient>,
    adjust: impl FnOnce(&mut crate::config::ServerConfig),
) -> Built {
    let clock = Arc::new(TestClock::new());
    let flows = Arc::new(InMemoryDeviceAuthStore::new(clock.clone()));
    let users = Arc::new(InMemoryUserRepository::new(clock.clone()));
    let sessions = Arc::new(InMemorySessionStore::new(clock.clone()));

    let base = make_app_state_full(
        adjust,
        clock.clone(),
        Arc::new(InMemoryDependencyProbe::healthy()),
        None,
    );
    let oidc_config = OidcRuntimeConfig {
        admin_claim: Some("groups".to_owned()),
        admin_value: Some("beam-admin".to_owned()),
        // `SessionAuthenticator` slides the idle expiry from `ServerConfig`;
        // the mint reads `OidcRuntimeConfig`. They must agree, as in `main`.
        session_idle_days: base.config.session_idle_days,
        session_max_days: base.config.session_max_days,
        ..base.services.oidc_config.clone()
    };

    let session_dyn: Arc<dyn SessionStore> = sessions.clone();
    let users_dyn: Arc<dyn UserRepository> = users.clone();
    let services = AppServices {
        hash: base.services.hash.clone(),
        library: base.services.library.clone(),
        metadata: base.services.metadata.clone(),
        subtitles: base.services.subtitles.clone(),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: users_dyn,
        playback: base.services.playback.clone(),
        genre_repo: base.services.genre_repo.clone(),
        library_repo: base.services.library_repo.clone(),
        file_repo: base.services.file_repo.clone(),
        enrichment_repo: base.services.enrichment_repo.clone(),
        movie_repo: base.services.movie_repo.clone(),
        show_repo: base.services.show_repo.clone(),
        artwork: base.services.artwork.clone(),
        session_store: session_dyn,
        oidc_client: oidc,
        pending_auth_store: base.services.pending_auth_store.clone(),
        device_auth_store: flows.clone(),
        oidc_config: oidc_config.clone(),
        watch_status: base.services.watch_status.clone(),
        telemetry: base.services.telemetry.clone(),
        playback_telemetry: base.services.playback_telemetry.clone(),
    };
    let state = AppState::with_clock(
        base.config.clone(),
        services,
        base.probe.clone(),
        clock.clone(),
        None,
    );
    let service = create_router()
        .build(state)
        .expect("the served router describes itself");

    (
        TestClient::new(service),
        flows,
        users,
        sessions,
        clock,
        oidc_config,
    )
}

async fn start(harness: &Harness) -> TestResponse {
    harness.client.post("/v1/auth/device").send().await
}

/// Starts a flow and returns its handle.
async fn started(harness: &Harness) -> String {
    let response = start(harness).await;
    response.assert_status(StatusCode::OK);
    response.json::<Value>()["device_handle"]
        .as_str()
        .expect("a handle")
        .to_owned()
}

async fn poll(harness: &Harness, handle: &str) -> TestResponse {
    harness
        .client
        .post("/v1/auth/device/token")
        .json(&json!({ "device_handle": handle }))
        .send()
        .await
}

fn problem(name: &str) -> String {
    format!("{ERRORS}#{name}")
}

// ─── POST /v1/auth/device ─────────────────────────────────────────────────────

#[tokio::test]
async fn starting_shows_the_user_code_and_keeps_only_the_hash_of_the_handle() {
    let harness = harness(FakeOidcClient::default());

    let response = start(&harness).await;
    response.assert_status(StatusCode::OK);
    let body: Value = response.json();

    assert_eq!(body["user_code"], FAKE_USER_CODE);
    assert_eq!(body["verification_uri"], "https://fake-idp.test/device");
    assert_eq!(
        body["verification_uri_complete"],
        format!("https://fake-idp.test/device?user_code={FAKE_USER_CODE}")
    );
    assert_eq!(body["expires_in_secs"], FAKE_DEVICE_EXPIRES_IN_SECS);
    assert_eq!(body["interval_secs"], FAKE_DEVICE_INTERVAL_SECS);

    let handle = body["device_handle"].as_str().unwrap();
    assert_eq!(
        harness.flows.handle_hashes(),
        [hash_handle(handle)],
        "the store is keyed by the handle's hash, never the handle"
    );
    assert!(
        !response.text().contains("fake-device-code"),
        "the IdP's device code must never reach the client: {}",
        response.text()
    );
}

#[tokio::test]
async fn an_idp_without_the_device_grant_is_501_so_the_client_falls_back() {
    let harness = harness(FakeOidcClient::default().without_device_flow());

    start(&harness)
        .await
        .assert_status(StatusCode::NOT_IMPLEMENTED)
        .assert_problem_type(&problem("device-login-unsupported"));
    assert!(harness.flows.handle_hashes().is_empty());
}

#[tokio::test]
async fn an_unconfigured_idp_is_503() {
    let (client, flows, ..) = build(
        Arc::new(NotConfiguredOidcClient::new("OIDC not configured")),
        |_| {},
    );

    client
        .post("/v1/auth/device")
        .send()
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE)
        .assert_problem_type(&problem("oidc-unavailable"));
    assert!(flows.handle_hashes().is_empty());
}

// ─── POST /v1/auth/device/token ───────────────────────────────────────────────

#[tokio::test]
async fn a_poll_before_approval_is_202_pending_and_polls_the_idp_with_the_kept_code() {
    let harness = harness(FakeOidcClient::default());
    let handle = started(&harness).await;

    let response = poll(&harness, &handle).await;
    response.assert_status(StatusCode::ACCEPTED);
    let body: Value = response.json();
    assert_eq!(body["status"], "authorization_pending");
    assert_eq!(body["interval_secs"], FAKE_DEVICE_INTERVAL_SECS);

    assert_eq!(harness.oidc.polled_device_codes(), ["fake-device-code-1"]);
    assert_eq!(
        harness.flows.handle_hashes().len(),
        1,
        "the flow stays open"
    );
}

#[tokio::test]
async fn a_poll_inside_the_interval_is_slowed_down_without_asking_the_idp() {
    let harness = harness(FakeOidcClient::default());
    let handle = started(&harness).await;
    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::ACCEPTED);

    harness.clock.advance(Duration::from_secs(1));
    let response = poll(&harness, &handle).await;

    response.assert_status(StatusCode::ACCEPTED);
    let body: Value = response.json();
    assert_eq!(body["status"], "slow_down");
    assert_eq!(body["interval_secs"], FAKE_DEVICE_INTERVAL_SECS + 5);
    assert_eq!(
        harness.oidc.device_poll_count(),
        1,
        "an early poll must be answered by Beam, not forwarded"
    );
}

#[tokio::test]
async fn an_idp_slow_down_grows_the_interval_the_client_is_told() {
    let harness =
        harness(FakeOidcClient::default().with_device_script(vec![Ok(DevicePoll::SlowDown)]));
    let handle = started(&harness).await;

    let response = poll(&harness, &handle).await;
    response.assert_status(StatusCode::ACCEPTED);
    let body: Value = response.json();
    assert_eq!(body["status"], "slow_down");
    assert_eq!(body["interval_secs"], FAKE_DEVICE_INTERVAL_SECS + 5);

    // The grown interval is enforced, not merely reported.
    harness
        .clock
        .advance(Duration::from_secs(FAKE_DEVICE_INTERVAL_SECS + 1));
    poll(&harness, &handle).await;
    assert_eq!(harness.oidc.device_poll_count(), 1);
}

#[tokio::test]
async fn a_denied_login_is_403_and_ends_the_flow() {
    let harness =
        harness(FakeOidcClient::default().with_device_script(vec![Ok(DevicePoll::Denied)]));
    let handle = started(&harness).await;

    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .assert_problem_type(&problem("device-login-denied"));
    assert!(harness.flows.handle_hashes().is_empty());

    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type(&problem("device-login-invalid"));
}

#[tokio::test]
async fn a_flow_left_past_its_lifetime_is_410_and_collected() {
    let harness = harness(FakeOidcClient::default());
    let handle = started(&harness).await;

    harness
        .clock
        .advance(Duration::from_secs(FAKE_DEVICE_EXPIRES_IN_SECS));
    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::GONE)
        .assert_problem_type(&problem("device-login-expired"));
    assert!(harness.flows.handle_hashes().is_empty());
    assert_eq!(harness.oidc.device_poll_count(), 0);
}

#[tokio::test]
async fn an_idp_reporting_expiry_is_410() {
    let harness =
        harness(FakeOidcClient::default().with_device_script(vec![Ok(DevicePoll::Expired)]));
    let handle = started(&harness).await;

    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::GONE)
        .assert_problem_type(&problem("device-login-expired"));
    assert!(harness.flows.handle_hashes().is_empty());
}

#[tokio::test]
async fn an_unknown_handle_is_400() {
    let harness = harness(FakeOidcClient::default());
    poll(&harness, "never-issued")
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type(&problem("device-login-invalid"));
    assert_eq!(harness.oidc.device_poll_count(), 0);
}

#[tokio::test]
async fn an_idp_outage_is_503_and_keeps_the_flow_for_the_next_poll() {
    let harness = harness(FakeOidcClient::default().with_device_script(vec![
        Err(OidcError::Exchange("connection refused".to_owned())),
        Ok(DevicePoll::Complete(identity(json!({})))),
    ]));
    let handle = started(&harness).await;

    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE)
        .assert_problem_type(&problem("oidc-unavailable"));
    assert_eq!(harness.flows.handle_hashes().len(), 1);

    harness
        .clock
        .advance(Duration::from_secs(FAKE_DEVICE_INTERVAL_SECS));
    poll(&harness, &handle).await.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn approval_mints_a_session_the_client_presents_as_the_cookie() {
    let harness = harness(
        FakeOidcClient::default()
            .with_device_script(vec![Ok(DevicePoll::Complete(identity(json!({}))))]),
    );
    let handle = started(&harness).await;

    let response = poll(&harness, &handle).await;
    response.assert_status(StatusCode::OK);
    assert!(
        response.headers("set-cookie").is_empty(),
        "the credential travels in the body; a cookie jar is the browser's"
    );
    let body: Value = response.json();
    let token = body["session_token"].as_str().expect("a session token");
    assert_eq!(
        body["session_expires_in_secs"],
        harness.oidc_config.absolute_ttl_secs()
    );
    assert_eq!(body["user"]["email"], "tv@example.com");
    assert_eq!(body["user"]["is_admin"], false);

    let me = harness
        .client
        .get("/v1/me")
        .cookie(SESSION_COOKIE, token)
        .send()
        .await;
    me.assert_status(StatusCode::OK);
    assert_eq!(me.json::<Value>()["id"], body["user"]["id"]);

    assert!(harness.flows.handle_hashes().is_empty(), "the flow ended");
    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type(&problem("device-login-invalid"));
}

#[tokio::test]
async fn the_admin_claim_is_honoured_as_on_a_browser_login() {
    let harness = harness(FakeOidcClient::default().with_device_script(vec![Ok(
        DevicePoll::Complete(identity(json!({"groups": ["beam-admin"]}))),
    )]));
    let handle = started(&harness).await;

    let body: Value = poll(&harness, &handle).await.json();
    assert_eq!(body["user"]["is_admin"], true);
    let stored = harness
        .users
        .find_by_oidc_identity("https://dex.test", "tv-user")
        .await
        .unwrap()
        .expect("provisioned on first sign-in");
    assert!(stored.is_admin);
}

#[tokio::test]
async fn a_disabled_account_is_refused_and_gets_no_session() {
    let harness = harness(FakeOidcClient::default().with_device_script(vec![
        Ok(DevicePoll::Complete(identity(json!({})))),
        Ok(DevicePoll::Complete(identity(json!({})))),
    ]));
    let first = started(&harness).await;
    let body: Value = poll(&harness, &first).await.json();
    let user_id = body["user"]["id"].as_str().unwrap().to_owned();
    harness
        .users
        .set_disabled(user_id.parse().unwrap(), true)
        .await
        .unwrap();

    let second = started(&harness).await;
    poll(&harness, &second)
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .assert_problem_type(&problem("account-disabled"));

    assert_eq!(
        harness
            .sessions
            .list_for_user(&user_id)
            .await
            .unwrap()
            .len(),
        1,
        "only the session from before the account was disabled"
    );
}

#[tokio::test]
async fn a_device_session_expires_on_the_same_idle_rule_as_a_browser_session() {
    let harness = harness(
        FakeOidcClient::default()
            .with_device_script(vec![Ok(DevicePoll::Complete(identity(json!({}))))]),
    );
    let handle = started(&harness).await;
    let body: Value = poll(&harness, &handle).await.json();
    let token = body["session_token"].as_str().unwrap().to_owned();

    let idle = Duration::from_secs(harness.oidc_config.idle_ttl_secs());
    harness.clock.advance(idle + Duration::from_secs(1));

    harness
        .client
        .get("/v1/me")
        .cookie(SESSION_COOKIE, &token)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

// ─── Same-origin and rate limits ──────────────────────────────────────────────

#[tokio::test]
async fn a_cross_origin_start_is_refused_but_a_native_client_with_no_origin_is_not() {
    let harness = harness(FakeOidcClient::default());

    harness
        .client
        .post("/v1/auth/device")
        .header("Origin", "http://evil.example.com")
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .assert_problem_type(&problem("cross-origin-rejected"));
    assert!(harness.flows.handle_hashes().is_empty());

    // A TV app sends neither Origin nor Referer (NFR-104).
    start(&harness).await.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn polls_spend_their_own_budget_and_are_refused_past_it() {
    let oidc = Arc::new(FakeOidcClient::default());
    let harness = harness_with(oidc, |config| {
        config.rate_limit_device_poll_per_minute = 3;
        config.rate_limit_auth_per_minute = 1_000;
    });
    let handle = started(&harness).await;

    let send = || {
        harness
            .client
            .post("/v1/auth/device/token")
            .peer(PEER.parse().unwrap())
            .json(&json!({ "device_handle": handle }))
            .send()
    };
    for _ in 0..3 {
        assert_eq!(send().await.status(), StatusCode::ACCEPTED);
    }
    send()
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS)
        .assert_problem_type(&problem("rate-limited"));
}

// ─── What the IdP grants, bounded ─────────────────────────────────────────────

#[tokio::test]
async fn a_lifetime_past_the_cap_is_cut_to_it_in_the_answer_and_the_stored_flow() {
    let harness = harness(
        FakeOidcClient::default()
            .with_device_grant(2 * DEVICE_LOGIN_MAX_SECS, FAKE_DEVICE_INTERVAL_SECS),
    );

    let response = start(&harness).await;
    response.assert_status(StatusCode::OK);
    let body: Value = response.json();
    assert_eq!(body["expires_in_secs"], DEVICE_LOGIN_MAX_SECS);
    let handle = body["device_handle"].as_str().unwrap().to_owned();

    // The stored flow ends at the cap too, not at the IdP's lifetime.
    harness
        .clock
        .advance(Duration::from_secs(DEVICE_LOGIN_MAX_SECS - 1));
    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::ACCEPTED);
    harness.clock.advance(Duration::from_secs(1));
    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::GONE)
        .assert_problem_type(&problem("device-login-expired"));
}

#[tokio::test]
async fn an_idp_asking_for_no_interval_is_paced_at_one_second() {
    let harness =
        harness(FakeOidcClient::default().with_device_grant(FAKE_DEVICE_EXPIRES_IN_SECS, 0));

    let response = start(&harness).await;
    response.assert_status(StatusCode::OK);
    let body: Value = response.json();
    assert_eq!(body["interval_secs"], 1);
    let handle = body["device_handle"].as_str().unwrap().to_owned();

    poll(&harness, &handle)
        .await
        .assert_status(StatusCode::ACCEPTED);
    // With no floor an immediate second poll would reach the IdP.
    let again = poll(&harness, &handle).await;
    again.assert_status(StatusCode::ACCEPTED);
    assert_eq!(again.json::<Value>()["status"], "slow_down");
    assert_eq!(harness.oidc.device_poll_count(), 1);
}

// ─── An approval that does not verify ─────────────────────────────────────────

#[tokio::test]
async fn an_id_token_that_does_not_verify_is_400_and_ends_the_flow() {
    for failure in [
        OidcError::MissingIdToken,
        OidcError::ClaimsVerification("audience does not match".to_owned()),
    ] {
        let label = failure.to_string();
        let harness = harness(FakeOidcClient::default().with_device_script(vec![Err(failure)]));
        let handle = started(&harness).await;

        poll(&harness, &handle)
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type(&problem("login-failed"));

        // A retry would verify no better, so the flow is gone and nobody was
        // provisioned.
        assert!(harness.flows.handle_hashes().is_empty(), "{label}");
        assert!(
            harness
                .users
                .find_by_oidc_identity("https://dex.test", "tv-user")
                .await
                .unwrap()
                .is_none(),
            "{label}"
        );
        poll(&harness, &handle)
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type(&problem("device-login-invalid"));
        assert_eq!(harness.oidc.device_poll_count(), 1, "{label}");
    }
}

// ─── Caching ──────────────────────────────────────────────────────────────────

fn assert_no_store(response: &TestResponse, what: &str) {
    assert_eq!(response.header("cache-control"), Some("no-store"), "{what}");
    assert_eq!(response.header("pragma"), Some("no-cache"), "{what}");
}

#[tokio::test]
async fn no_answer_carrying_a_handle_or_a_session_may_be_cached() {
    // RFC 6749 section 5.1 for the credential; the handle is as good as one
    // for as long as the flow is open.
    let approving = harness(
        FakeOidcClient::default()
            .with_device_script(vec![Ok(DevicePoll::Complete(identity(json!({}))))]),
    );
    let waiting = harness(FakeOidcClient::default());

    let start_response = start(&approving).await;
    start_response.assert_status(StatusCode::OK);
    assert_no_store(&start_response, "the start, which carries the handle");

    let handle = start_response.json::<Value>()["device_handle"]
        .as_str()
        .unwrap()
        .to_owned();
    let signed_in = poll(&approving, &handle).await;
    signed_in.assert_status(StatusCode::OK);
    assert_no_store(&signed_in, "the 200, which carries the session");

    let waiting_handle = started(&waiting).await;
    let pending = poll(&waiting, &waiting_handle).await;
    pending.assert_status(StatusCode::ACCEPTED);
    assert_no_store(&pending, "the 202");
}
