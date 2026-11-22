//! Subcutaneous tests for `GET /v1/libraries/changes` (issue #454): the real
//! router, handler and access service, over the notification service the
//! indexer's watch consumer publishes `library_changed` to.

use std::collections::BTreeSet;

use beam_auth::utils::session_store::SessionData;
use beam_domain::models::access::{AccessPolicy, LibraryAccess};
use kynos::http::StatusCode;
use kynos::test::TestClient;
use uuid::Uuid;

use crate::models::LibraryChanges;
use crate::routes::test_support::{make_app_state, served_client, signed_in};
use crate::state::AppState;

/// A session for a new user, and the user.
async fn viewer(state: &AppState) -> (String, Uuid) {
    let user_id = Uuid::new_v4();
    let token = state
        .services
        .session_store
        .create(
            &SessionData {
                user_id: user_id.to_string(),
                device_hash: "test-device".to_owned(),
                ip: "127.0.0.1".to_owned(),
                created_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
            },
            86_400,
            86_400,
        )
        .await
        .expect("the in-memory session store issues a session");
    (token, user_id)
}

async fn poll(client: &TestClient<AppState>, token: &str, since: Option<&str>) -> LibraryChanges {
    let path = match since {
        Some(since) => format!("/v1/libraries/changes?since={since}"),
        None => "/v1/libraries/changes".to_owned(),
    };
    client
        .get(&path)
        .cookie("beam_session", token)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
}

/// A client's first poll is a reset that hands it a cursor; after the
/// watcher announces a change, the next poll names the library once, and the
/// one after that names nothing.
#[tokio::test]
async fn a_poll_after_a_library_changed_names_that_library_once() {
    let state = make_app_state();
    let client = served_client(state.clone());
    let token = signed_in(&state).await;
    let (films, shows) = (Uuid::from_u128(1), Uuid::from_u128(2));

    let first = poll(&client, &token, None).await;
    assert!(first.reset, "no cursor yet: refetch everything");
    assert!(first.library_ids.is_empty());

    let notification = &state.services.notification;
    notification.library_changed(films);
    notification.library_changed(shows);
    notification.library_changed(films);
    let second = poll(&client, &token, Some(&first.cursor)).await;
    assert!(!second.reset);
    assert_eq!(sorted(second.library_ids), sorted(vec![films, shows]));
    assert_ne!(second.cursor, first.cursor);

    let third = poll(&client, &token, Some(&second.cursor)).await;
    assert!(!third.reset);
    assert!(third.library_ids.is_empty(), "nothing changed since");
    assert_eq!(third.cursor, second.cursor);
}

/// A viewer restricted to some libraries never learns that another changed:
/// a library id is all the feed carries, and a hidden one is left out.
#[tokio::test]
async fn a_restricted_viewer_is_told_only_of_the_libraries_they_may_see() {
    let state = make_app_state();
    let client = served_client(state.clone());
    let (kids, films) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let (open, _) = viewer(&state).await;
    let (restricted, restricted_id) = viewer(&state).await;
    state
        .services
        .access
        .replace(
            restricted_id,
            &AccessPolicy {
                libraries: LibraryAccess::Only(BTreeSet::from([kids])),
                max_rating: None,
            },
        )
        .await
        .expect("store the policy");
    let cursor = poll(&client, &open, None).await.cursor;

    state.services.notification.library_changed(kids);
    state.services.notification.library_changed(films);

    let seen_open = poll(&client, &open, Some(&cursor)).await;
    assert_eq!(sorted(seen_open.library_ids), sorted(vec![kids, films]));
    let seen_restricted = poll(&client, &restricted, Some(&cursor)).await;
    assert_eq!(seen_restricted.library_ids, vec![kids]);
    assert_eq!(
        seen_restricted.cursor, seen_open.cursor,
        "the restricted viewer still moves past the hidden change"
    );
}

/// A cursor this server did not issue -- malformed, or from before a
/// restart -- is a reset rather than a refusal, so a client always recovers.
#[tokio::test]
async fn an_unknown_cursor_is_a_reset() {
    let state = make_app_state();
    let client = served_client(state.clone());
    let token = signed_in(&state).await;
    state
        .services
        .notification
        .library_changed(Uuid::from_u128(1));

    let before_restart = format!("{}.0", Uuid::from_u128(99));
    for since in ["garbage", before_restart.as_str()] {
        let answer = poll(&client, &token, Some(since)).await;
        assert!(answer.reset, "{since:?} is a reset");
        assert!(answer.library_ids.is_empty());
        let next = poll(&client, &token, Some(&answer.cursor)).await;
        assert!(!next.reset, "the cursor a reset hands out is a real one");
    }
}

/// Like every library read, the feed is for signed-in users only.
#[tokio::test]
async fn polling_without_a_session_is_401() {
    let client = served_client(make_app_state());
    client
        .get("/v1/libraries/changes")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
