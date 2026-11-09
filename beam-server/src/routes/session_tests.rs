//! Subcutaneous tests for signing out and for the session list:
//! `POST /v1/logout`, `POST /v1/logout/all`, `GET /v1/sessions` and
//! `DELETE /v1/sessions/{id}`.
//!
//! The same harness as `auth_tests.rs`, whose fixtures these share: the real
//! handlers over a `FakeOidcClient` and the in-memory stores (NFR-201).

use std::sync::Arc;

use beam_auth::utils::oidc::fake::FakeOidcClient;
use beam_auth::utils::repository::in_memory::InMemoryUserRepository;
use beam_auth::utils::session_store::in_memory::{InMemorySessionStore, SessionStoreOperation};
use kynos::http::StatusCode;
use kynos::test::TestResponse;
use serde_json::Value;

use super::auth_tests::tests::{
    Harness, STATE_COOKIE, do_login, get_me, harness_failing, identity, identity_with, live_cookie,
    login_and_get_session_cookie, make_harness, make_harness_behind, make_harness_sharing_store,
    test_config,
};
use crate::routes::session_auth::SESSION_COOKIE;

/// Whether the response tells the browser to drop the session cookie.
///
/// A clearing cookie is emitted as `beam_session=; ...; Max-Age=0` rather
/// than a live one, so this reads the raw field -- which is what a browser
/// acts on and therefore what this asserts.
fn clears_session_cookie(response: &TestResponse) -> bool {
    response
        .headers("set-cookie")
        .iter()
        .any(|c| c.starts_with("beam_session=;"))
}

#[tokio::test]
async fn logout_deletes_session_so_me_then_fails() {
    let harness = make_harness(FakeOidcClient::with_identity(identity(
        "logout@example.com",
        true,
    )));
    let session_cookie = login_and_get_session_cookie(&harness, "logout@example.com").await;

    let response = harness
        .client
        .post("/v1/logout")
        .cookie(SESSION_COOKIE, &session_cookie)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        clears_session_cookie(&response),
        "logging out must also clear the cookie"
    );

    assert_eq!(
        get_me(&harness, &session_cookie).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// FR-109: logout used to discard a store failure, clear the cookie and
/// answer 204 -- the client believed it had signed out while the session
/// stayed valid on the server. It is a 500 now, and the cookie stays so the
/// client can retry.
#[tokio::test]
async fn a_session_store_failure_on_logout_is_500_and_keeps_the_cookie() {
    let harness = harness_failing("stuck@example.com", SessionStoreOperation::Delete);
    let session_cookie = login_and_get_session_cookie(&harness, "stuck@example.com").await;

    let response = harness
        .client
        .post("/v1/logout")
        .cookie(SESSION_COOKIE, &session_cookie)
        .send()
        .await;

    response
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
    assert!(
        response.headers("set-cookie").is_empty(),
        "a logout that revoked nothing must not touch the cookie: {:?}",
        response.headers("set-cookie")
    );
    assert_eq!(
        get_me(&harness, &session_cookie).await.status(),
        StatusCode::OK,
        "the session the failed logout left is still valid, as the 500 says"
    );
}

/// NFR-108 over the session routes: whichever store call fails, the 500's
/// `detail` is the fixed sentence and never the store's own message.
#[tokio::test]
async fn a_session_store_failure_never_echoes_its_cause() {
    for (operation, method, path) in [
        (SessionStoreOperation::Delete, "POST", "/v1/logout"),
        (
            SessionStoreOperation::DeleteAllForUser,
            "POST",
            "/v1/logout-all",
        ),
        (SessionStoreOperation::ListForUser, "GET", "/v1/sessions"),
    ] {
        let harness = harness_failing("echo@example.com", operation);
        let session_cookie = login_and_get_session_cookie(&harness, "echo@example.com").await;
        let request = match method {
            "POST" => harness.client.post(path),
            _ => harness.client.get(path),
        };
        let response = request.cookie(SESSION_COOKIE, &session_cookie).send().await;

        response
            .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
        let body = response.json::<Value>();
        assert_eq!(
            body["detail"],
            crate::routes::api_error::INTERNAL_DETAIL,
            "{path}"
        );
        assert!(
            !body.to_string().contains("injected failure"),
            "{path}: the store's message reached the wire: {body}"
        );
    }
}

/// The session ids `GET /v1/sessions` reports for the holder of `cookie`.
async fn list_session_ids(harness: &Harness, cookie: &str) -> Vec<String> {
    let response = harness
        .client
        .get("/v1/sessions")
        .cookie(SESSION_COOKIE, cookie)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>();
    assert_eq!(
        body["page_info"]["has_next_page"], false,
        "the whole list is one page"
    );
    body["items"]
        .as_array()
        .expect("a connection's items")
        .iter()
        .map(|session| session["id"].as_str().expect("a session id").to_string())
        .collect()
}

#[tokio::test]
async fn list_and_delete_session_round_trip() {
    let harness = make_harness(FakeOidcClient::with_identity(identity(
        "sessions@example.com",
        true,
    )));
    let session_cookie = login_and_get_session_cookie(&harness, "sessions@example.com").await;

    let ids = list_session_ids(&harness, &session_cookie).await;
    assert_eq!(ids.len(), 1);

    let response = harness
        .client
        .delete(&format!("/v1/sessions/{}", ids[0]))
        .cookie(SESSION_COOKIE, &session_cookie)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // The session used to make this very request was just revoked.
    assert_eq!(
        get_me(&harness, &session_cookie).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// An id naming no session is `404 #session-not-found`, and the caller
/// stays signed in: it is the id that failed, not their credential.
#[tokio::test]
async fn delete_unknown_session_id_returns_404() {
    let harness = make_harness(FakeOidcClient::with_identity(identity(
        "sessions2@example.com",
        true,
    )));
    let session_cookie = login_and_get_session_cookie(&harness, "sessions2@example.com").await;

    harness
        .client
        .delete("/v1/sessions/00000000-0000-0000-0000-000000000000")
        .cookie(SESSION_COOKIE, &session_cookie)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#session-not-found");
    assert_eq!(
        get_me(&harness, &session_cookie).await.status(),
        StatusCode::OK
    );
}

/// A well-formed id nobody owns is a 404; a malformed one is a 400.
///
/// It used to be a 500, because the handler flattened every
/// `SessionError` into `Internal`. The path id is typed `Uuid` now, so the
/// `Path` extractor answers the 400 before the store is reached at all.
#[tokio::test]
async fn delete_malformed_session_id_returns_400() {
    let harness = make_harness(FakeOidcClient::with_identity(identity(
        "sessions3@example.com",
        true,
    )));
    let session_cookie = login_and_get_session_cookie(&harness, "sessions3@example.com").await;

    harness
        .client
        .delete("/v1/sessions/not-a-uuid")
        .cookie(SESSION_COOKIE, &session_cookie)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

/// The store keeps epoch seconds; the list reports them as the instants
/// they are. Sign-in and last use are two hours apart, so a swapped or
/// mis-scaled conversion cannot pass.
#[tokio::test]
async fn listed_sessions_report_when_they_were_signed_in_and_last_used() {
    let signed_in_at =
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a representable instant");
    let clock = Arc::new(beam_domain::services::TestClock::starting_at(signed_in_at));
    let harness = make_harness_sharing_store(
        FakeOidcClient::with_identity(identity("instants@example.com", true)),
        Arc::new(InMemoryUserRepository::default()),
        Arc::new(InMemorySessionStore::new(clock.clone())),
        test_config(),
    );
    let session_cookie = login_and_get_session_cookie(&harness, "instants@example.com").await;

    // Past the touch throttle, so this request slides `last_active`.
    clock.advance(std::time::Duration::from_secs(2 * 3600));
    assert_eq!(
        get_me(&harness, &session_cookie).await.status(),
        StatusCode::OK
    );

    let response = harness
        .client
        .get("/v1/sessions")
        .cookie(SESSION_COOKIE, &session_cookie)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>();
    let sessions = body["items"].as_array().expect("a connection's items");
    assert_eq!(sessions.len(), 1);
    let instant = |field: &str| {
        sessions[0][field]
            .as_str()
            .unwrap_or_else(|| panic!("{field} is a date-time string"))
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap_or_else(|_| panic!("{field} parses as RFC 3339"))
    };
    assert_eq!(instant("created_at"), signed_in_at);
    assert_eq!(
        instant("last_active_at"),
        signed_in_at + chrono::Duration::hours(2)
    );
}

/// Signs in through a callback that arrives from the socket `peer`
/// carrying every forwarding header a client could forge, and returns the
/// address `GET /v1/sessions` then reports for that session.
async fn session_ip_after_callback_from(harness: &Harness, peer: &str) -> String {
    let (state_cookie, _) = do_login(harness, None).await;
    let begin = harness.oidc_client.last_begin().unwrap();
    let response = harness
        .client
        .get(&format!(
            "/v1/auth/callback?state={}&code=fake-code",
            begin.state
        ))
        .cookie(STATE_COOKIE, &state_cookie)
        .peer(peer.parse().expect("a socket address"))
        .header("X-Forwarded-For", "198.51.100.4")
        .header("X-Real-IP", "192.0.2.99")
        .header("User-Agent", "BeamTV/1.0")
        .send()
        .await;
    response.assert_status(StatusCode::FOUND);
    let session_cookie = live_cookie(&response, SESSION_COOKIE)
        .expect("a session was minted")
        .to_owned();

    let listed = harness
        .client
        .get("/v1/sessions")
        .cookie(SESSION_COOKIE, &session_cookie)
        .send()
        .await;
    assert_eq!(listed.status(), StatusCode::OK);
    let body = listed.json::<Value>();
    let sessions = body["items"].as_array().expect("a connection's items");
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0]["device_hash"],
        beam_auth::utils::login::device_hash(Some("BeamTV/1.0")),
        "the User-Agent still reaches the session's device fingerprint"
    );
    sessions[0]["ip"]
        .as_str()
        .expect("the session's address is a string")
        .to_owned()
}

/// Issue #200: with no proxy trusted -- the default -- the session records
/// the socket peer, and neither `X-Forwarded-For` nor `X-Real-IP` the
/// client wrote changes it.
#[tokio::test]
async fn a_session_records_the_socket_peer_when_no_proxy_is_trusted() {
    let harness = make_harness(FakeOidcClient::with_identity(identity(
        "peer@example.com",
        true,
    )));
    assert_eq!(
        session_ip_after_callback_from(&harness, "203.0.113.7:41000").await,
        "203.0.113.7"
    );
}

/// Behind a proxy listed in `BEAM_TRUSTED_PROXIES`, the session records the
/// client that proxy appended to `X-Forwarded-For`; a peer outside the
/// list is still recorded as itself.
#[tokio::test]
async fn a_session_records_the_client_a_trusted_proxy_names() {
    let behind_proxy = || {
        make_harness_behind(
            FakeOidcClient::with_identity(identity("proxied@example.com", true)),
            Arc::new(InMemoryUserRepository::default()),
            Arc::new(InMemorySessionStore::default()),
            test_config(),
            |server_config| server_config.trusted_proxies = vec!["10.0.0.0/8".to_owned()],
        )
    };

    assert_eq!(
        session_ip_after_callback_from(&behind_proxy(), "10.1.2.3:443").await,
        "198.51.100.4",
        "the listed proxy names the client"
    );
    assert_eq!(
        session_ip_after_callback_from(&behind_proxy(), "203.0.113.7:41000").await,
        "203.0.113.7",
        "a peer outside the list cannot name its own address"
    );
}

/// A stored second no `DateTime` can hold is a corrupt row: reported as
/// an error rather than rendered as some other instant. The in-memory
/// store stamps from a `DateTime` clock, so only the SQL store can hold
/// such a row, and the conversion is exercised directly.
#[test]
fn a_session_with_an_unrepresentable_timestamp_is_an_error() {
    use crate::models::auth::SessionSummary;
    use beam_auth::utils::session_store::SessionData;

    let id = uuid::Uuid::nil();
    let data = |created_at: i64, last_active: i64| SessionData {
        user_id: "00000000-0000-0000-0000-000000000001".to_string(),
        device_hash: "device".to_string(),
        ip: "127.0.0.1".to_string(),
        created_at,
        last_active,
    };

    assert!(SessionSummary::from_store(id, data(i64::MAX, 0)).is_err());
    assert!(SessionSummary::from_store(id, data(0, i64::MAX)).is_err());
    let summary = SessionSummary::from_store(id, data(1_700_000_000, 1_700_007_200))
        .expect("representable seconds convert");
    assert_eq!(summary.created_at.timestamp(), 1_700_000_000);
    assert_eq!(summary.last_active_at.timestamp(), 1_700_007_200);
}

#[tokio::test]
async fn logging_out_everywhere_revokes_every_session_of_that_user_only() {
    // The mutant this pins: replacing the handler body with `Ok(())`
    // returns 204 and leaves every session alive. Asserting the status
    // alone cannot tell the difference.
    let harness = make_harness(FakeOidcClient::with_identity(identity(
        "everywhere@example.com",
        true,
    )));
    let first = login_and_get_session_cookie(&harness, "everywhere@example.com").await;
    let second = login_and_get_session_cookie(&harness, "everywhere@example.com").await;
    assert_ne!(first, second, "two logins are two sessions");

    // A genuinely different user over the *same* store, whose session must
    // survive. Identity is `(issuer, subject)`, so the subject has to
    // differ -- `identity()` pins it, which would make this one account.
    let bystander_harness = make_harness_sharing_store(
        FakeOidcClient::with_identity(identity_with(
            "https://dex.test",
            "subj-bystander",
            Some("bystander@example.com"),
            Some("Bystander"),
        )),
        harness.user_repo.clone(),
        harness.session_store.clone(),
        test_config(),
    );
    let bystander_cookie =
        login_and_get_session_cookie(&bystander_harness, "bystander@example.com").await;

    let response = harness
        .client
        .post("/v1/logout-all")
        .cookie(SESSION_COOKIE, &first)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    for cookie in [&first, &second] {
        assert_eq!(
            get_me(&harness, cookie).await.status(),
            StatusCode::UNAUTHORIZED,
            "every session of the caller must be gone"
        );
    }
    assert_eq!(
        get_me(&bystander_harness, &bystander_cookie).await.status(),
        StatusCode::OK,
        "logging one user out everywhere must not touch anyone else"
    );
}

#[tokio::test]
async fn one_user_cannot_revoke_another_users_session_by_its_id() {
    // The ownership check in `oidc_delete_session` is the only thing
    // standing between a listed session id and a cross-account logout.
    let user_repo = Arc::new(InMemoryUserRepository::default());
    let session_store = Arc::new(InMemorySessionStore::default());

    // Distinct subjects: identity is `(issuer, subject)`, and `identity()`
    // pins the subject, so two calls to it are one account.
    let victim = make_harness_sharing_store(
        FakeOidcClient::with_identity(identity_with(
            "https://dex.test",
            "subj-victim",
            Some("victim@example.com"),
            Some("Victim"),
        )),
        user_repo.clone(),
        session_store.clone(),
        test_config(),
    );
    let attacker = make_harness_sharing_store(
        FakeOidcClient::with_identity(identity_with(
            "https://dex.test",
            "subj-attacker",
            Some("attacker@example.com"),
            Some("Attacker"),
        )),
        user_repo.clone(),
        session_store.clone(),
        test_config(),
    );

    let victim_cookie = login_and_get_session_cookie(&victim, "victim@example.com").await;
    let attacker_cookie = login_and_get_session_cookie(&attacker, "attacker@example.com").await;
    assert_ne!(victim_cookie, attacker_cookie);

    let victim_sessions = list_session_ids(&victim, &victim_cookie).await;
    assert_eq!(
        victim_sessions.len(),
        1,
        "the victim sees only their own session"
    );

    let response = attacker
        .client
        .delete(&format!("/v1/sessions/{}", victim_sessions[0]))
        .cookie(SESSION_COOKIE, &attacker_cookie)
        .send()
        .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another user's session id must not be revocable, and must not be \
         distinguishable from one that does not exist"
    );

    assert_eq!(
        get_me(&victim, &victim_cookie).await.status(),
        StatusCode::OK,
        "the victim is still signed in"
    );
}

#[tokio::test]
async fn revoking_a_session_other_than_the_current_one_leaves_the_cookie_alone() {
    // Whether the caller's own cookie is cleared is decided by re-reading
    // their token after the revocation. Inverting that would sign the
    // caller out every time they revoked one of their *other* devices.
    let harness = make_harness(FakeOidcClient::with_identity(identity(
        "twodevices@example.com",
        true,
    )));

    // Log in the old device first and read its id while it is the only
    // session, so the id is known rather than guessed at.
    let old_device = login_and_get_session_cookie(&harness, "twodevices@example.com").await;
    let ids = list_session_ids(&harness, &old_device).await;
    assert_eq!(ids.len(), 1);
    let old_device_id = ids[0].clone();

    let current = login_and_get_session_cookie(&harness, "twodevices@example.com").await;
    assert_ne!(old_device, current);

    let response = harness
        .client
        .delete(&format!("/v1/sessions/{old_device_id}"))
        .cookie(SESSION_COOKIE, &current)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        !clears_session_cookie(&response),
        "revoking another device must not clear the caller's own cookie"
    );

    // The caller is still signed in; the other device is not.
    assert_eq!(
        get_me(&harness, &current).await.status(),
        StatusCode::OK,
        "the caller keeps the session they were holding"
    );
    assert_eq!(
        get_me(&harness, &old_device).await.status(),
        StatusCode::UNAUTHORIZED,
        "the revoked device is signed out"
    );
}

#[tokio::test]
async fn revoking_the_session_in_use_clears_the_callers_cookie() {
    // The other half of the same condition: when the id *is* the caller's
    // own session, leaving a live cookie behind for a dead session makes
    // every later request a confusing 401.
    let harness = make_harness(FakeOidcClient::with_identity(identity(
        "selfrevoke@example.com",
        true,
    )));
    let cookie = login_and_get_session_cookie(&harness, "selfrevoke@example.com").await;

    let ids = list_session_ids(&harness, &cookie).await;
    let own_id = ids[0].clone();

    let response = harness
        .client
        .delete(&format!("/v1/sessions/{own_id}"))
        .cookie(SESSION_COOKIE, &cookie)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        clears_session_cookie(&response),
        "revoking the session in use must clear its cookie"
    );
}
