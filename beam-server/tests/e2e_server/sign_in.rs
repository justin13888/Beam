//! Signing in: the RFC 8628 device grant a native client uses (#151, #215),
//! and the browser's Authorization Code + PKCE flow, both through Dex's real
//! login form. Admin rights come from the IdP's `groups` claim alone (FR-106).

use std::time::Duration;

use beam_entity::session;
use beam_test_support::e2e::StackOptions;
use beam_test_support::e2e::client::api;
use sea_orm::EntityTrait;

use crate::common::{self, ADMIN_ACCOUNT, VIEWER, assert_refused};

/// The device grant end to end: the server hands out a code, says
/// `authorization_pending` until the user approves at Dex, then mints the
/// same opaque session a browser gets -- and the handle is spent once used.
#[tokio::test]
async fn the_device_grant_signs_a_native_client_in() {
    let stack = common::start().await;
    let anonymous = stack.anonymous();

    let started = anonymous
        .start_device_login()
        .await
        .expect("start a device sign-in")
        .into_inner();
    assert!(!started.user_code.is_empty(), "there is a code to show");
    assert!(
        started
            .verification_uri
            .starts_with(&beam_test_support::e2e::idp::issuer()),
        "the user approves at the identity provider: {}",
        started.verification_uri
    );
    let poll = api::types::DeviceLoginPoll {
        device_handle: started.device_handle.clone(),
    };

    let pending = anonymous
        .poll_device_login(&poll)
        .await
        .expect("poll before approval")
        .into_inner();
    let api::PollDeviceLoginResponse::Status202(pending) = pending else {
        panic!("an unapproved sign-in is still waiting, not {pending:?}");
    };
    assert_eq!(
        pending.status,
        api::types::DeviceLoginWait::AuthorizationPending
    );

    stack
        .browser()
        .approve_device(
            started
                .verification_uri_complete
                .as_deref()
                .expect("a complete verification URI"),
            ADMIN_ACCOUNT,
        )
        .await;

    // The interval the server asked for, which a client must keep to.
    tokio::time::sleep(Duration::from_secs(
        u64::try_from(pending.interval_secs).expect("a positive interval"),
    ))
    .await;
    let complete = anonymous
        .poll_device_login(&poll)
        .await
        .expect("poll after approval")
        .into_inner();
    let api::PollDeviceLoginResponse::Status200(complete) = complete else {
        panic!("an approved sign-in completes, not {complete:?}");
    };
    assert!(complete.user.is_admin, "admin@ is in the beam-admin group");
    assert_eq!(complete.user.email.as_deref(), Some(ADMIN_ACCOUNT.email));

    let signed_in = stack.client(&complete.session_token);
    let me = signed_in
        .get_current_user()
        .await
        .expect("GET /v1/me with the minted session")
        .into_inner();
    assert_eq!(me.id, complete.user.id);
    assert!(me.is_admin);

    let sessions = session::Entity::find()
        .all(stack.db())
        .await
        .expect("read the sessions table");
    assert_eq!(sessions.len(), 1, "exactly one session was minted");
    assert_eq!(sessions[0].user_id, me.id);
    assert_ne!(
        sessions[0].token_hash, complete.session_token,
        "the token itself is never stored"
    );

    let reused = anonymous.poll_device_login(&poll).await;
    let status = common::status_of(&reused.expect_err("a spent handle is refused"));
    assert!(
        matches!(status, Some(400 | 410)),
        "a spent handle cannot mint a second session (answered {status:?})"
    );

    stack.finish().await;
}

/// The browser flow: login redirects to Dex, the callback checks the state
/// cookie and sets the session cookie, and the IdP's claim -- nothing else --
/// decides who is an administrator.
#[tokio::test]
async fn the_browser_flow_signs_in_and_only_the_idp_makes_an_administrator() {
    // Dex's static client registers exactly this callback.
    let stack = common::start_with(StackOptions::new().port(8000)).await;

    let admin_session = stack.sign_in_with_browser(ADMIN_ACCOUNT).await;
    let admin = stack.client(&admin_session);
    let me = admin
        .get_current_user()
        .await
        .expect("GET /v1/me as admin@")
        .into_inner();
    assert!(me.is_admin);
    admin
        .list_admin_users(None)
        .await
        .expect("an administrator lists users");

    let viewer_session = stack.sign_in_with_browser(VIEWER).await;
    let viewer = stack.client(&viewer_session);
    let me = viewer
        .get_current_user()
        .await
        .expect("GET /v1/me as user@")
        .into_inner();
    assert!(!me.is_admin, "user@ is in no group");
    assert_refused(
        "a viewer listing users",
        403,
        viewer.list_admin_users(None).await,
    );
    assert_refused(
        "a viewer creating a library",
        403,
        viewer
            .create_library(&api::types::CreateLibraryRequest {
                name: "Films".to_owned(),
                root_path: ".".to_owned(),
            })
            .await,
    );

    // `POST /v1/logout` reads the session as an optional cookie parameter
    // rather than through the `BeamSession` scheme, so the generated client
    // sends it only when asked to.
    viewer
        .logout(api::LogoutParams {
            beam_session: Some(viewer_session.clone()),
            beam_oidc_state: None,
        })
        .await
        .expect("sign out");
    assert_refused(
        "GET /v1/me after signing out",
        401,
        viewer.get_current_user().await,
    );
    admin
        .get_current_user()
        .await
        .expect("another user's session is untouched");

    // The generated client refuses to send a secured call with no session at
    // all, so the server's own answer to one is asked for raw.
    let anonymous = common::raw_get(stack.base_url(), "/v1/me", None, None).await;
    assert_eq!(anonymous.status().as_u16(), 401, "no session, no user");

    stack.finish().await;
}
