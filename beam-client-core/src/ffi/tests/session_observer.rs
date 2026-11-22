//! The session observer: every change to the server list reaches it exactly
//! once, and nothing that leaves the list as it was reaches it at all.

use super::*;
use std::sync::Mutex;

/// Every list the core announced, in order.
#[derive(Default)]
struct Recorded(Mutex<Vec<Vec<ServerSummary>>>);

impl Recorded {
    fn lists(&self) -> Vec<Vec<ServerSummary>> {
        self.0.lock().expect("recorded lock").clone()
    }
}

impl SessionObserver for Recorded {
    fn on_sessions_changed(&self, servers: Vec<ServerSummary>) {
        self.0.lock().expect("recorded lock").push(servers);
    }
}

/// An observer registered on `client` from this point on.
fn observing(client: &BeamClient) -> Arc<Recorded> {
    let recorded = Arc::new(Recorded::default());
    client.set_session_observer(Arc::clone(&recorded) as Arc<dyn SessionObserver>);
    recorded
}

/// The one list `recorded` was told, which must be what the client lists now.
fn the_only_announcement(client: &BeamClient, recorded: &Recorded) -> ServerSummary {
    let lists = recorded.lists();
    assert_eq!(lists.len(), 1, "exactly one change is announced: {lists:?}");
    assert_eq!(
        lists[0],
        client.list_servers().expect("listable"),
        "the observer is told the list as it now stands"
    );
    let [server] = lists[0].as_slice() else {
        panic!("one server is registered: {lists:?}");
    };
    server.clone()
}

const EXPIRED: (u16, &str, &str) = (
    401,
    "application/problem+json",
    r#"{"type":"about:blank","status":401,"detail":"session expired"}"#,
);

/// A `401` on any call is the session expiring under the app: announced
/// once, with the server signed out, however many calls fail after it.
#[tokio::test]
async fn a_401_announces_the_server_signed_out_exactly_once() {
    let (client, id, _) = signed_in_client().await;
    let recorded = observing(&client);
    let (status, content_type, body) = EXPIRED;
    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(status, content_type, body)),
        )
        .expect("the server is registered");

    for _ in 0..3 {
        let _ = client.media_sources(MEDIA_ID.to_owned()).await;
    }

    let server = the_only_announcement(&client, &recorded);
    assert_eq!(server.id, id);
    assert_eq!(server.state, SessionState::Expired);
}

/// A drain names a refused session instead of throwing it, so the platform
/// has no error to react to: the announcement is how the app learns the
/// server signed out, and a second drain does not repeat it.
#[tokio::test]
async fn a_drain_that_meets_a_401_announces_the_server_signed_out_once() {
    let (client, id, _) = signed_in_client().await;
    answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::Unreachable],
    );
    assert!(matches!(
        client
            .report_progress(FILE_A.to_owned(), 30.0, None, true)
            .await
            .expect("an outcome"),
        crate::progress::ProgressOutcome::Queued { .. }
    ));
    let recorded = observing(&client);
    let (status, content_type, body) = EXPIRED;
    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(status, content_type, body)),
        )
        .expect("the server is registered");

    let drain = client.drain_progress().await.expect("an expiry is named");
    assert_eq!(drain.expired_server_ids, vec![id.clone()]);
    client.drain_progress().await.expect("drained");

    let server = the_only_announcement(&client, &recorded);
    assert_eq!(server.id, id);
    assert_eq!(server.state, SessionState::Expired);
}

#[tokio::test]
async fn a_completed_sign_in_is_announced_once() {
    let (client, id) = client_with_server().await;
    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(200, "application/json", ME)),
        )
        .expect("the server is registered");
    let recorded = observing(&client);

    client
        .complete_login(id.clone(), "opaque-value".to_owned())
        .await
        .expect("the canned /v1/me confirms the cookie");

    let server = the_only_announcement(&client, &recorded);
    let SessionState::Authenticated { user } = server.state else {
        panic!("announced signed in: {server:?}");
    };
    assert_eq!(user.display_name, "Ada");
}

#[tokio::test]
async fn a_sign_out_is_announced_once() {
    let (client, id, _) = signed_in_client().await;
    let recorded = observing(&client);

    client.logout(id).await.expect("signed out");

    assert_eq!(
        the_only_announcement(&client, &recorded).state,
        SessionState::LoggedOut
    );
}

/// The state moved before the keystore refused: the device is signed out,
/// and the observer has to hear it even though the call failed.
#[tokio::test]
async fn a_sign_out_the_keystore_refuses_is_still_announced() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
    let recorded = observing(&client);
    storage.set_failure(FailureMode::FailWrites);

    assert!(matches!(
        client.logout(id).await,
        Err(BeamError::Storage { .. })
    ));

    assert_eq!(
        the_only_announcement(&client, &recorded).state,
        SessionState::LoggedOut
    );
}

/// A sign-in the server rejects from signed out ends where it began, so
/// there is nothing to announce -- not the `AwaitingLogin` it passed
/// through, and not a second copy of the list the observer already has.
#[tokio::test]
async fn a_rejected_sign_in_from_signed_out_announces_nothing() {
    let (client, id) = client_with_server().await;
    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(
                403,
                "application/problem+json",
                r#"{"type":"about:blank","status":403,"detail":"disabled"}"#,
            )),
        )
        .expect("the server is registered");
    let recorded = observing(&client);
    let before = client.list_servers().expect("listable");
    client.select_server(id.clone()).await.expect("selected");
    assert_eq!(
        recorded.lists(),
        vec![before],
        "the observer is told the list once to start from"
    );

    let _ = client
        .complete_login(id, "refused-value".to_owned())
        .await
        .expect_err("the canned 403 rejects the cookie");

    assert_eq!(recorded.lists().len(), 1, "{:?}", recorded.lists());
}

/// A sign-in that stops partway -- here, at storing the confirmed cookie --
/// still leaves the observer holding the list as it stands.
#[tokio::test]
async fn a_sign_in_that_fails_to_store_announces_where_it_stopped() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let id = client
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added")
        .id;
    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(200, "application/json", ME)),
        )
        .expect("the server is registered");
    let recorded = observing(&client);
    storage.set_failure(FailureMode::FailWrites);

    assert!(matches!(
        client.complete_login(id, "opaque-value".to_owned()).await,
        Err(BeamError::Storage { .. })
    ));

    the_only_announcement(&client, &recorded);
}

/// Adding, re-adding, selecting and removing servers: each change once,
/// and the idempotent re-add -- which changes nothing -- not at all.
#[tokio::test]
async fn server_additions_selections_and_removals_are_each_announced() {
    let client = client();
    let recorded = observing(&client);

    let home = client
        .add_server("https://beam.test".to_owned(), Some("Home".to_owned()))
        .await
        .expect("added");
    client
        .add_server("https://beam.test/".to_owned(), None)
        .await
        .expect("idempotent");
    client
        .add_server("https://cabin.test".to_owned(), Some("Cabin".to_owned()))
        .await
        .expect("added");
    client
        .select_server(home.id.clone())
        .await
        .expect("selected");
    client
        .remove_server(home.id.clone())
        .await
        .expect("removed");

    let announced: Vec<Vec<(String, bool)>> = recorded
        .lists()
        .into_iter()
        .map(|servers| {
            servers
                .into_iter()
                .map(|server| (server.display_name, server.is_active))
                .collect()
        })
        .collect();
    let entry = |name: &str, active: bool| (name.to_owned(), active);
    assert_eq!(
        announced,
        vec![
            vec![entry("Home", true)],
            vec![entry("Cabin", true), entry("Home", false)],
            vec![entry("Cabin", false), entry("Home", true)],
            vec![entry("Cabin", false)],
        ]
    );
}

/// The server is installed before the registry write the keystore refuses,
/// so it is in the list all the same: the observer hears that list once.
#[tokio::test]
async fn an_addition_the_storage_refuses_is_still_announced() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let recorded = observing(&client);
    storage.set_failure(FailureMode::FailWrites);

    assert!(matches!(
        client
            .add_server("https://beam.test".to_owned(), Some("Home".to_owned()))
            .await,
        Err(BeamError::Storage { .. })
    ));

    let server = the_only_announcement(&client, &recorded);
    assert_eq!(server.display_name, "Home");
    assert_eq!(server.state, SessionState::LoggedOut);
}

/// The server leaves the in-memory registry before the write the keystore
/// refuses, so the list changed: the observer hears it once, without it.
#[tokio::test]
async fn a_removal_the_storage_refuses_is_still_announced() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let home = client
        .add_server("https://beam.test".to_owned(), Some("Home".to_owned()))
        .await
        .expect("added")
        .id;
    let cabin = client
        .add_server("https://cabin.test".to_owned(), Some("Cabin".to_owned()))
        .await
        .expect("added")
        .id;
    let recorded = observing(&client);
    storage.set_failure(FailureMode::FailWrites);

    assert!(matches!(
        client.remove_server(home).await,
        Err(BeamError::Storage { .. })
    ));

    let server = the_only_announcement(&client, &recorded);
    assert_eq!(server.id, cabin);
}

/// A cold start announces what it read, and registering announces nothing
/// by itself: the app reads the list it starts from.
#[tokio::test]
async fn a_restore_announces_what_it_read() {
    let (storage, _, id) = restarted_after_sign_in(ME, "opaque").await;
    let client = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    let recorded = observing(&client);
    assert!(recorded.lists().is_empty(), "registering announces nothing");

    let restored = client.restore().await.expect("restored");

    assert_eq!(recorded.lists(), vec![restored]);
    let server = the_only_announcement(&client, &recorded);
    assert_eq!(server.id, id);
    assert!(matches!(server.state, SessionState::Authenticated { .. }));
}
