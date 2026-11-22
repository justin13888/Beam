//! What the account and session events leave in the process log (NFR-403):
//! signing out, signing out everywhere, revoking a session, provisioning an
//! account on its first sign-in, and the identity provider changing an
//! account's admin status. Each names the account; none carries a session
//! token.
//!
//! The same harness as `auth_tests.rs`, whose fixtures and log capture these
//! share: the real handlers over a `FakeOidcClient` and the in-memory stores.

use std::sync::Arc;

use beam_auth::utils::oidc::fake::FakeOidcClient;
use beam_auth::utils::repository::UserRepository;
use beam_auth::utils::repository::in_memory::InMemoryUserRepository;
use kynos::http::StatusCode;
use serde_json::{Value, json};

use super::auth_tests::tests::{
    CapturedLogs, Harness, identity, identity_with_claims, login_and_get_session_cookie,
    make_harness, make_harness_with_repo,
};
use crate::routes::session_auth::SESSION_COOKIE;

/// The one captured line that carries `message`.
fn line_with<'a>(logged: &'a str, message: &str) -> &'a str {
    let mut lines = logged.lines().filter(|line| line.contains(message));
    let line = lines
        .next()
        .unwrap_or_else(|| panic!("no {message:?} event in:\n{logged}"));
    assert!(
        lines.next().is_none(),
        "exactly one {message:?} event expected in:\n{logged}"
    );
    line
}

/// The id of the default `(https://dex.test, subj-1)` account.
async fn account_id(harness: &Harness) -> String {
    harness
        .user_repo
        .find_by_oidc_identity("https://dex.test", "subj-1")
        .await
        .unwrap()
        .expect("provisioned")
        .id
        .to_string()
}

/// Finding (b) of #346: signing out of one session, out of every session, and
/// revoking a session by its listing id are each an `info` event naming the
/// account -- and no session token reaches the log.
#[tokio::test]
async fn signing_out_and_revoking_a_session_are_logged_with_the_account_and_no_token() {
    let logs = CapturedLogs::default();
    let _guard = logs.install();
    let harness = make_harness(FakeOidcClient::with_identity(identity(
        "out@example.com",
        true,
    )));
    let first = login_and_get_session_cookie(&harness, "out@example.com").await;
    let second = login_and_get_session_cookie(&harness, "out@example.com").await;
    let third = login_and_get_session_cookie(&harness, "out@example.com").await;
    let user_id = account_id(&harness).await;

    harness
        .client
        .post("/v1/logout")
        .cookie(SESSION_COOKIE, &second)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    // `first` and `third` remain.
    harness
        .client
        .post("/v1/logout-all")
        .cookie(SESSION_COOKIE, &first)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let fourth = login_and_get_session_cookie(&harness, "out@example.com").await;
    let listed: Value = harness
        .client
        .get("/v1/sessions")
        .cookie(SESSION_COOKIE, &fourth)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let session_id = listed["items"][0]["id"]
        .as_str()
        .expect("the one session left")
        .to_owned();
    harness
        .client
        .delete(&format!("/v1/sessions/{session_id}"))
        .cookie(SESSION_COOKIE, &fourth)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let logged = logs.text();
    let one = line_with(&logged, "signed out of one session");
    assert!(one.contains("INFO") && one.contains(&user_id), "{one}");
    let every = line_with(&logged, "signed out of every session");
    assert!(
        every.contains("INFO") && every.contains(&user_id) && every.contains("revoked=2"),
        "{every}"
    );
    let revoked = line_with(&logged, "revoked a session");
    assert!(
        revoked.contains("INFO") && revoked.contains(&user_id) && revoked.contains(&session_id),
        "{revoked}"
    );
    for token in [&first, &second, &third, &fourth] {
        assert!(
            !logged.contains(token.as_str()),
            "a session token reached the log"
        );
    }
}

/// An account provisioned on its first sign-in, and the identity provider's
/// admin claim later changing that account's admin status, are each an
/// `info` event naming the account and the admin status it now has. A sign-in
/// that changes neither logs neither.
#[tokio::test]
async fn provisioning_an_account_and_the_idp_changing_its_admin_status_are_logged() {
    let logs = CapturedLogs::default();
    let _guard = logs.install();
    let user_repo = Arc::new(InMemoryUserRepository::default());
    let as_admin = make_harness_with_repo(
        FakeOidcClient::with_identity(identity_with_claims(json!({ "groups": ["beam-admin"] }))),
        user_repo.clone(),
    );
    let first = login_and_get_session_cookie(&as_admin, "user@example.com").await;
    let user_id = account_id(&as_admin).await;

    let provisioned = "provisioned an account on its first sign-in";
    let changed = "the identity provider's admin claim changed this account's admin status";
    let logged = logs.text();
    let line = line_with(&logged, provisioned);
    assert!(
        line.contains("INFO") && line.contains(&user_id) && line.contains("is_admin=true"),
        "{line}"
    );

    // The same claim again: the account exists and its admin status stands.
    let again = login_and_get_session_cookie(&as_admin, "user@example.com").await;
    assert!(!logs.text().contains(changed), "{}", logs.text());

    let as_user = make_harness_with_repo(
        FakeOidcClient::with_identity(identity_with_claims(json!({}))),
        user_repo.clone(),
    );
    let demoted = login_and_get_session_cookie(&as_user, "user@example.com").await;

    let logged = logs.text();
    line_with(&logged, provisioned);
    let line = line_with(&logged, changed);
    assert!(
        line.contains("INFO") && line.contains(&user_id) && line.contains("is_admin=false"),
        "{line}"
    );
    for token in [&first, &again, &demoted] {
        assert!(
            !logged.contains(token.as_str()),
            "a session token reached the log"
        );
    }
}
