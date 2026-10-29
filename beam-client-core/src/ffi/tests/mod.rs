use super::*;
use crate::catalog::BrowseQuery;
use crate::error::StorageError;
use crate::ports::kv::{FailureMode, InMemoryKeyValueStore};
use crate::progress::QueuedProgress;
use crate::transport::CannedBackend;

mod admin;
mod catalog;
mod playback;
mod progress;
mod telemetry;

/// The library the library and scan tests name.
const LIBRARY_ID: &str = "00000000-0000-0000-0000-00000000000a";

/// Everything here exercises the façade without a network. That is most of
/// it: the registry, the session state machine, trust decisions, and the
/// configuration handed to the platform player are all local, and they are
/// the parts a mistake in would be invisible until a device ran them.
fn client() -> Arc<BeamClient> {
    BeamClient::new(Arc::new(InMemoryKeyValueStore::new()))
}

async fn client_with_server() -> (Arc<BeamClient>, String) {
    let client = client();
    let summary = client
        .add_server("https://beam.test".to_owned(), Some("Home".to_owned()))
        .await
        .expect("adding a server is offline");
    let id = summary.id.clone();
    (client, id)
}

#[tokio::test]
async fn a_new_client_knows_no_servers() {
    let client = client();
    assert!(client.list_servers().expect("listable").is_empty());
    assert!(matches!(
        client.playback_config("f1".to_owned()),
        Err(BeamError::NoActiveServer)
    ));
}

#[tokio::test]
async fn adding_a_server_makes_it_active() {
    let (client, id) = client_with_server().await;

    let servers = client.list_servers().expect("listable");
    assert_eq!(servers.len(), 1);
    assert!(servers[0].is_active);
    assert_eq!(servers[0].id, id);
    assert_eq!(servers[0].display_name, "Home");
}

#[tokio::test]
async fn adding_the_same_server_twice_is_idempotent() {
    // Typing the same address again means "go there", not "fail" -- and
    // certainly not "register it twice".
    let (client, id) = client_with_server().await;

    let again = client
        .add_server("https://beam.test/".to_owned(), None)
        .await
        .expect("idempotent");

    assert_eq!(again.id, id);
    assert_eq!(client.list_servers().expect("listable").len(), 1);
    assert_eq!(
        again.display_name, "Home",
        "re-adding must not discard the name the user gave it"
    );
}

#[tokio::test]
async fn an_unusable_address_is_rejected_before_anything_is_stored() {
    let client = client();

    assert!(matches!(
        client.add_server("not a url".to_owned(), None).await,
        Err(BeamError::InvalidServerUrl { .. })
    ));
    assert!(client.list_servers().expect("listable").is_empty());
}

#[tokio::test]
async fn a_registry_survives_a_restart() {
    // The whole point of persisting it: a cold start must not send the
    // viewer back to the address field.
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let first = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    first
        .add_server("https://beam.test".to_owned(), Some("Home".to_owned()))
        .await
        .expect("added");

    let second = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    let restored = second.restore().await.expect("restored");

    assert_eq!(restored.len(), 1);
    assert!(restored[0].is_active, "the active choice must survive too");
}

#[tokio::test]
async fn a_restored_session_is_usable_without_a_request() {
    // A cold start lands signed in, as the identity the server confirmed
    // at sign-in, and the cookie is ready for the player -- all before
    // this client has sent anything. Its transport is the real one,
    // pointed at a host that does not resolve, so a request would fail.
    let (_, client, id) = restarted_after_sign_in(ADMIN_ME, "opaque").await;

    let SessionState::Authenticated { user } = client.session_state(id).expect("known") else {
        panic!("a stored session restores signed in");
    };
    assert_eq!(user.display_name, "Ada");
    assert!(
        user.is_admin,
        "the role the server confirmed survives the restart"
    );
    assert_eq!(
        client
            .server_http_config()
            .expect("the cookie is registered")
            .headers
            .get("Cookie")
            .map(String::as_str),
        Some("beam_session=opaque")
    );
}

/// A cookie stored without the identity beside it cannot say who is
/// signed in, so it does not restore a session.
#[tokio::test]
async fn a_cookie_stored_without_an_identity_restores_signed_out() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let first = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let summary = first
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added");
    storage
        .put_secret(session_key(&summary.id), "opaque".to_owned())
        .await
        .expect("stored");

    let second = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    second.restore().await.expect("restored");

    assert_eq!(
        second.session_state(summary.id).expect("known"),
        SessionState::LoggedOut
    );
    assert!(
        matches!(second.server_http_config(), Err(BeamError::Unauthenticated)),
        "and the cookie is not registered"
    );
}

/// Everything a `401` on a restored cookie must withdraw: the state, the
/// client's credential, the player's copy, and both stored halves --
/// leaving any behind sends the refused cookie again, or restores it on
/// the next cold start.
#[tokio::test]
async fn a_401_on_a_restored_cookie_withdraws_it_everywhere() {
    let (storage, client, id) = restarted_after_sign_in(ME, "opaque").await;
    let backend = Arc::new(CannedBackend::answering(
        401,
        "application/problem+json",
        r#"{"type":"about:blank","status":401,"detail":"session expired"}"#,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    assert_eq!(
        client.media_sources(MEDIA_ID.to_owned()).await,
        Err(BeamError::SessionExpired)
    );
    assert_eq!(
        client.session_state(id.clone()).expect("known"),
        SessionState::Expired
    );
    let sent_before = backend.recorded().len();
    assert_eq!(
        client.media_sources(MEDIA_ID.to_owned()).await,
        Err(BeamError::SessionExpired)
    );
    assert_eq!(
        backend.recorded().len(),
        sent_before,
        "the refused cookie is not sent again"
    );
    assert!(
        matches!(client.server_http_config(), Err(BeamError::Unauthenticated)),
        "nor handed to the player"
    );
    assert!(!storage.has_secret(&session_key(&id)));
    assert!(!storage.has_secret(&identity_key(&id)));

    let restarted = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    restarted.restore().await.expect("restored");
    assert_eq!(
        restarted.session_state(id).expect("known"),
        SessionState::LoggedOut,
        "nor restored by the next cold start"
    );
}

/// The user signs out while the sign-in's `/v1/me` is in flight. The
/// confirmation arrives for a sign-in nobody is waiting on, and must
/// leave the device signed out -- now, and after a restart.
#[tokio::test]
async fn a_sign_in_confirmed_after_a_sign_out_stays_signed_out() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let id = client
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added")
        .id;
    let backend = Arc::new(SignsOutDuringMe {
        client: Arc::downgrade(&client),
        server_id: id.clone(),
    });
    client
        .use_transport(&id, backend as Arc<dyn crate::api::HttpBackend>)
        .expect("the server is registered");

    assert_eq!(
        client.complete_login(id.clone(), "late".to_owned()).await,
        Err(BeamError::Unauthenticated)
    );
    assert_eq!(
        client.session_state(id.clone()).expect("known"),
        SessionState::LoggedOut
    );
    assert!(!storage.has_secret(&session_key(&id)));
    assert!(!storage.has_secret(&identity_key(&id)));
}

/// The same late confirmation over a keystore that refuses the withdrawal.
/// The cookie and identity this sign-in wrote stay on disk, so the session
/// is marked abandoned: the next `restore` drops it rather than signing
/// the device back in as the user who signed out.
#[tokio::test]
async fn a_late_sign_in_the_keystore_will_not_withdraw_is_not_restored() {
    let inner = Arc::new(InMemoryKeyValueStore::new());
    let storage = Arc::new(RefusesStoredSecretRemoval {
        inner: Arc::clone(&inner),
    });
    let client = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    let id = client
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added")
        .id;
    let backend = Arc::new(SignsOutDuringMe {
        client: Arc::downgrade(&client),
        server_id: id.clone(),
    });
    client
        .use_transport(&id, backend as Arc<dyn crate::api::HttpBackend>)
        .expect("the server is registered");

    assert_eq!(
        client.complete_login(id.clone(), "late".to_owned()).await,
        Err(BeamError::Unauthenticated),
        "a refused delete does not change the answer"
    );
    assert!(
        inner.has_secret(&session_key(&id)) && inner.has_secret(&identity_key(&id)),
        "the deletes really did fail, which is what the mark is for"
    );

    client
        .restore()
        .await
        .expect("the registry itself is still readable");

    assert_eq!(
        client.session_state(id.clone()).expect("known"),
        SessionState::LoggedOut,
        "the stranded session is not restored"
    );
    assert!(
        matches!(client.server_http_config(), Err(BeamError::Unauthenticated)),
        "nor its cookie handed to the player"
    );
}

/// A keystore that refuses to delete a secret it holds. Deleting an absent
/// key still succeeds, so the sign-out that races the sign-in -- which runs
/// before anything is stored -- goes through.
#[derive(Debug)]
struct RefusesStoredSecretRemoval {
    inner: Arc<InMemoryKeyValueStore>,
}

#[async_trait::async_trait]
impl KeyValueStore for RefusesStoredSecretRemoval {
    async fn get(&self, key: String) -> Result<Option<String>, StorageError> {
        self.inner.get(key).await
    }

    async fn put(&self, key: String, value: String) -> Result<(), StorageError> {
        self.inner.put(key, value).await
    }

    async fn remove(&self, key: String) -> Result<(), StorageError> {
        self.inner.remove(key).await
    }

    async fn list_keys(&self, prefix: String) -> Result<Vec<String>, StorageError> {
        self.inner.list_keys(prefix).await
    }

    async fn get_secret(&self, key: String) -> Result<Option<String>, StorageError> {
        self.inner.get_secret(key).await
    }

    async fn put_secret(&self, key: String, value: String) -> Result<(), StorageError> {
        self.inner.put_secret(key, value).await
    }

    async fn remove_secret(&self, key: String) -> Result<(), StorageError> {
        if self.inner.has_secret(&key) {
            return Err(StorageError::Unavailable {
                detail: "keystore locked".to_owned(),
            });
        }
        self.inner.remove_secret(key).await
    }
}

/// A transport that, asked for `/v1/me`, signs the user out before it
/// answers -- the sign-out racing a sign-in, made deterministic.
#[derive(Debug)]
struct SignsOutDuringMe {
    client: std::sync::Weak<BeamClient>,
    server_id: String,
}

impl crate::api::HttpBackend for SignsOutDuringMe {
    fn execute(&self, request: reqwest::Request) -> crate::api::ExecuteFuture<'_> {
        let is_me = request.url().path() == "/v1/me";
        Box::pin(async move {
            let (status, body) = if is_me {
                let client = self.client.upgrade().expect("the client outlives its call");
                client
                    .logout(self.server_id.clone())
                    .await
                    .expect("signing out is local");
                (200, ME)
            } else {
                (204, "")
            };
            let response = ::http::Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(body.to_owned())
                .expect("a canned response is well-formed");
            Ok(reqwest::Response::from(response))
        })
    }
}

#[tokio::test]
async fn a_server_with_no_stored_cookie_restores_as_logged_out() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let first = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let summary = first
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added");

    let second = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    second.restore().await.expect("restored");

    assert!(matches!(
        second.session_state(summary.id).expect("known"),
        SessionState::LoggedOut
    ));
}

#[tokio::test]
async fn removing_a_server_forgets_it_and_its_secret() {
    // Signed in and with a queued resume point, so every key the server
    // owns exists before the removal -- the removal is what clears them.
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
    client
        .use_transport(&id, Arc::new(CannedBackend::unreachable()))
        .expect("the server is registered");
    client
        .report_progress(FILE_A.to_owned(), 30.0, None, true)
        .await
        .expect("queued");
    for key in [
        format!("servers/{id}"),
        format!("progress_queue/{id}"),
        ACTIVE_SERVER_KEY.to_owned(),
    ] {
        assert!(storage.has_plain(&key), "{key} exists before the removal");
    }
    assert!(storage.has_secret(&session_key(&id)));
    assert!(storage.has_secret(&identity_key(&id)));

    client.remove_server(id.clone()).await.expect("removed");

    assert!(client.list_servers().expect("listable").is_empty());
    assert!(matches!(
        client.session_state(id.clone()),
        Err(BeamError::UnknownServer { .. })
    ));
    for key in [
        format!("servers/{id}"),
        format!("progress_queue/{id}"),
        ACTIVE_SERVER_KEY.to_owned(),
    ] {
        assert!(!storage.has_plain(&key), "{key} outlived its server");
    }
    assert!(!storage.has_secret(&session_key(&id)), "the cookie leaked");
    assert!(!storage.has_secret(&identity_key(&id)));
    assert_eq!(
        storage
            .get(SERVER_INDEX_KEY.to_owned())
            .await
            .expect("readable"),
        Some("[]".to_owned()),
        "the index no longer names it"
    );
}

#[tokio::test]
async fn selecting_an_unknown_server_is_an_error() {
    let client = client();
    assert!(matches!(
        client.select_server("nope".to_owned()).await,
        Err(BeamError::UnknownServer { .. })
    ));
}

#[tokio::test]
async fn the_login_url_is_built_from_the_server_origin() {
    let (client, id) = client_with_server().await;

    let url = client.login_url(id).expect("built");

    assert!(url.starts_with("https://beam.test/v1/auth/login"));
    assert!(
        url.contains("redirect="),
        "the server only accepts a same-origin relative redirect"
    );
}

#[tokio::test]
async fn trusting_a_certificate_records_it_and_survives_a_restart() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let summary = client
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added");

    client
        .trust_certificate(summary.id.clone(), "AA:BB:CC".to_owned())
        .await
        .expect("trusted");

    assert_eq!(
        client
            .trusted_certificates(summary.id.clone())
            .expect("known"),
        vec!["AA:BB:CC".to_owned()]
    );

    // A trust decision the user made once must not be forgotten on restart,
    // or they would be asked again on every cold start.
    let restarted = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    restarted.restore().await.expect("restored");
    assert_eq!(
        restarted.trusted_certificates(summary.id).expect("known"),
        vec!["AA:BB:CC".to_owned()]
    );
}

#[tokio::test]
async fn trusting_the_same_certificate_twice_does_not_duplicate_it() {
    let (client, id) = client_with_server().await;

    client
        .trust_certificate(id.clone(), "AA:BB".to_owned())
        .await
        .expect("trusted");
    client
        .trust_certificate(id.clone(), "AA:BB".to_owned())
        .await
        .expect("trusted again");

    assert_eq!(client.trusted_certificates(id).expect("known").len(), 1);
}

#[tokio::test]
async fn forgetting_certificates_withdraws_every_one() {
    let (client, id) = client_with_server().await;
    client
        .trust_certificate(id.clone(), "AA:BB".to_owned())
        .await
        .expect("trusted");

    client
        .forget_certificates(id.clone())
        .await
        .expect("forgotten");

    assert!(client.trusted_certificates(id).expect("known").is_empty());
}

#[tokio::test]
async fn trusting_a_certificate_for_an_unknown_server_is_an_error() {
    let client = client();
    assert!(matches!(
        client
            .trust_certificate("nope".to_owned(), "AA:BB".to_owned())
            .await,
        Err(BeamError::UnknownServer { .. })
    ));
}

#[tokio::test]
async fn logging_out_clears_the_session_but_keeps_the_server() {
    // Signing out is not forgetting the address; the viewer will sign back
    // in to the same place.
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
    assert!(matches!(
        client.session_state(id.clone()).expect("known"),
        SessionState::Authenticated { .. }
    ));

    client.logout(id.clone()).await.expect("logged out");

    assert!(matches!(
        client.session_state(id.clone()).expect("still known"),
        SessionState::LoggedOut
    ));
    assert!(!storage.has_secret(&session_key(&id)));
    assert_eq!(client.list_servers().expect("listable").len(), 1);
    assert!(storage.has_plain(&format!("servers/{id}")));
}

/// Signing out with no signal still signs out on this device: the revoke
/// is best-effort, and a sign-out button that failed offline would leave
/// a lost phone signed in.
#[tokio::test]
async fn logout_with_an_unreachable_server_still_signs_out_locally() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
    let backend = Arc::new(CannedBackend::unreachable());
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    client
        .logout(id.clone())
        .await
        .expect("an unreachable revoke is not an error");

    assert_eq!(backend.recorded().len(), 1, "the revoke was attempted");
    assert_eq!(backend.recorded()[0].url.path(), "/v1/logout");
    assert!(matches!(
        client.session_state(id.clone()).expect("known"),
        SessionState::LoggedOut
    ));
    assert!(!storage.has_secret(&session_key(&id)));
    assert!(!storage.has_secret(&identity_key(&id)));
    assert_eq!(
        client.server_http_config(),
        Err(BeamError::Unauthenticated),
        "the player no longer gets the cookie either"
    );
}

/// The title the transport tests ask about; what it names never matters.
const MEDIA_ID: &str = "00000000-0000-0000-0000-000000000007";

/// What `GET /v1/me` answers for the signed-in user in these tests.
const ME: &str =
    r#"{"id":"00000000-0000-0000-0000-0000000000a1","display_name":"Ada","is_admin":false}"#;
/// [`ME`], for a user the server says administers it.
const ADMIN_ME: &str =
    r#"{"id":"00000000-0000-0000-0000-0000000000a1","display_name":"Ada","is_admin":true}"#;

/// What `GET /v1/media/{id}/sources` answers for a title with no file.
const NO_SOURCES: &str = r#"{"items":[],"page_info":{"has_next_page":false,"has_previous_page":false,"start_cursor":null,"end_cursor":null}}"#;

/// A signed-in server answering each request with the next of `answers`.
async fn client_answering_in_turn(
    answers: Vec<(u16, &'static str, &'static str)>,
) -> (Arc<BeamClient>, Arc<CannedBackend>) {
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering_in_turn(answers));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    (client, backend)
}

/// Sign in to a registered server against a canned `/v1/me`, through the
/// production path: `complete_login` registers the cookie with the client
/// and verifies it with the one secured request the flow makes.
///
/// Returns the backend too, so a test can read what reached the wire.
async fn signed_in_client() -> (Arc<BeamClient>, String, Arc<CannedBackend>) {
    signed_in_client_over(Arc::new(InMemoryKeyValueStore::new())).await
}

/// `signed_in_client`, over a storage the test keeps a handle to.
async fn signed_in_client_over(
    storage: Arc<InMemoryKeyValueStore>,
) -> (Arc<BeamClient>, String, Arc<CannedBackend>) {
    let client = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    let id = client
        .add_server("https://beam.test".to_owned(), Some("Home".to_owned()))
        .await
        .expect("adding a server is offline")
        .id;
    let backend = Arc::new(CannedBackend::answering(200, "application/json", ME));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    client
        .complete_login(id.clone(), "opaque-value".to_owned())
        .await
        .expect("the canned /v1/me confirms the cookie");
    (client, id, backend)
}

/// A cold start on a device that signed in with `cookie`, the server
/// answering `/v1/me` with `me`: the session a real sign-in stored, read
/// back by a fresh client over the same storage.
async fn restarted_after_sign_in(
    me: &'static str,
    cookie: &str,
) -> (Arc<InMemoryKeyValueStore>, Arc<BeamClient>, String) {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let first = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let id = first
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("adding a server is offline")
        .id;
    first
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(200, "application/json", me)),
        )
        .expect("the server is registered");
    first
        .complete_login(id.clone(), cookie.to_owned())
        .await
        .expect("the canned /v1/me confirms the cookie");

    let second = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    second.restore().await.expect("restored");
    (storage, second, id)
}

/// The `Cookie` header values one recorded request carried.
fn cookies_of(request: &crate::transport::RecordedRequest) -> Vec<&str> {
    request
        .headers
        .get_all(reqwest::header::COOKIE)
        .iter()
        .map(|value| value.to_str().expect("a cookie header is ASCII"))
        .collect()
}

/// A signed-in server that answers every further request with one canned
/// response.
///
/// Signed in, because every operation these tests make is one the spec
/// secures with `BeamSession`, and the generated client refuses to build
/// such a request without a registered credential -- so a logged-out
/// call would never reach the canned answer at all.
async fn client_answering(status: u16, body: &'static str) -> (Arc<BeamClient>, String) {
    let (client, id, _) = signed_in_client().await;
    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(
                status,
                "application/problem+json",
                body,
            )),
        )
        .expect("the server is registered");
    (client, id)
}

/// The whole reason the credential lives on the client: a secured call
/// from a signed-in device has to reach the server carrying the cookie --
/// once, as the spec's `BeamSession` scheme spells it.
#[tokio::test]
async fn a_signed_in_call_carries_the_session_cookie_exactly_once() {
    let (client, _, backend) = signed_in_client().await;

    // The canned body is a `CurrentUser`, so this decodes to nothing
    // useful; what matters is that the request left at all, and how.
    let _ = client.media_sources(MEDIA_ID.to_owned()).await;

    let recorded = backend.recorded();
    assert_eq!(recorded.len(), 2, "the /v1/me of the sign-in, then this");
    let sources = &recorded[1];
    assert_eq!(sources.method, reqwest::Method::GET);
    assert_eq!(sources.url.path(), format!("/v1/media/{MEDIA_ID}/sources"));
    assert_eq!(cookies_of(sources), vec!["beam_session=opaque-value"]);
    assert_eq!(
        cookies_of(&recorded[0]),
        vec!["beam_session=opaque-value"],
        "the sign-in's own /v1/me is a secured call too"
    );
}

/// Signing out revokes the session on the server, not only on the device:
/// the `POST /v1/logout` carries the cookie it is ending, read before the
/// transition withdraws it. Without it the server answers 204 and leaves
/// the session valid until it expires.
#[tokio::test]
async fn logout_sends_the_session_it_is_ending() {
    let (client, id, backend) = signed_in_client().await;
    let sent_before = backend.recorded().len();

    client.logout(id.clone()).await.expect("logged out");

    let recorded = backend.recorded();
    assert_eq!(recorded.len(), sent_before + 1, "exactly the revoke");
    let revoke = recorded.last().expect("the revoke was sent");
    assert_eq!(revoke.method, reqwest::Method::POST);
    assert_eq!(revoke.url.path(), "/v1/logout");
    assert_eq!(cookies_of(revoke), vec!["beam_session=opaque-value"]);
    assert_eq!(
        client.session_state(id).expect("registered"),
        SessionState::LoggedOut,
        "and the device is signed out"
    );
}

/// Signing out withdraws the credential, and the generated client then
/// refuses the request before it is sent: nothing reaches the transport,
/// and what the caller gets is "sign in" -- not a malformed request, and
/// not something permanent, because signing in cures it.
#[tokio::test]
async fn after_logout_a_secured_call_never_reaches_the_transport() {
    let (client, id, backend) = signed_in_client().await;
    client.logout(id).await.expect("logged out");
    let sent_before = backend.recorded().len();

    let error = client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect_err("no credential, no request");

    assert_eq!(error, BeamError::Unauthenticated);
    assert!(
        !error.is_permanent(),
        "a resume point must wait for the sign-in"
    );
    assert_eq!(
        backend.recorded().len(),
        sent_before,
        "the refusal happens at construction, not on the wire"
    );
}

/// The twin of the logout case: once the server has rejected the session,
/// the next secured call is refused at construction too, and it says
/// "your session expired" rather than "sign in" -- the state the 401 left
/// behind is what tells the two apart.
#[tokio::test]
async fn after_an_expiry_a_secured_call_is_reported_as_expired_not_malformed() {
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering(
        401,
        "application/problem+json",
        r#"{"type":"about:blank","status":401}"#,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    let _ = client.media_sources(MEDIA_ID.to_owned()).await;
    let sent_before = backend.recorded().len();

    let error = client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect_err("no credential, no request");

    assert_eq!(error, BeamError::SessionExpired);
    assert!(!error.is_permanent(), "{error:?}");
    assert_eq!(backend.recorded().len(), sent_before);
}

/// A cookie found in secret storage at startup is registered with the
/// client, not merely remembered: the first request after a cold start
/// carries it.
#[tokio::test]
async fn a_restored_cookie_is_sent_with_the_first_secured_call() {
    let (_, second, id) = restarted_after_sign_in(ME, "restored-value").await;
    let backend = Arc::new(CannedBackend::answering(
        200,
        "application/json",
        NO_SOURCES,
    ));
    second
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    second
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect("an empty list decodes");

    let recorded = backend.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        cookies_of(&recorded[0]),
        vec!["beam_session=restored-value"]
    );
}

/// Everything the machine's `UnauthorizedObserved` transition promises, in
/// one place: the state, the client's credential, the player's copy, and
/// the stored one. Leaving any behind means a cookie the server has
/// already refused is sent again -- by the next call, by Media3, or by the
/// next cold start.
#[tokio::test]
async fn a_401_mid_session_withdraws_the_credential_everywhere() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
    let secret_key = format!("session/{id}");
    assert!(storage.has_secret(&secret_key), "signing in stored it");
    let backend = Arc::new(CannedBackend::answering(
        401,
        "application/problem+json",
        r#"{"type":"about:blank","status":401,"detail":"session expired"}"#,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let error = client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect_err("the canned 401 fails the call");

    assert_eq!(error, BeamError::SessionExpired);
    assert!(matches!(
        client.session_state(id.clone()).expect("known"),
        SessionState::Expired
    ));
    let sent_before = backend.recorded().len();
    let _ = client.media_sources(MEDIA_ID.to_owned()).await;
    assert_eq!(
        backend.recorded().len(),
        sent_before,
        "the rejected cookie is not sent again"
    );
    assert!(
        matches!(client.server_http_config(), Err(BeamError::Unauthenticated)),
        "nor handed to the player"
    );
    assert!(
        !storage.has_secret(&secret_key),
        "nor kept for the next cold start"
    );
}

/// The three paths that decide the credential but were covered by no test
/// of their own. Each answers one question: is the cookie on the client
/// afterwards -- observed as whether a secured call leaves at all.
///
/// A cookie the server rejected at sign-in must not stay registered, or
/// every later call would send it and fail in a way the user cannot see.
#[tokio::test]
async fn a_rejected_login_leaves_no_credential_behind() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let id = client
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added")
        .id;
    // 403 rather than 401, so that only `complete_login`'s own failure
    // branch withdraws the cookie -- a 401 would have the expiry path do
    // it too, and this test would not know which one did.
    let backend = Arc::new(CannedBackend::answering(
        403,
        "application/problem+json",
        r#"{"type":"about:blank","status":403,"detail":"disabled"}"#,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let error = client
        .complete_login(id.clone(), "refused-value".to_owned())
        .await
        .expect_err("the canned 403 rejects the cookie");

    assert!(matches!(error, BeamError::Forbidden { .. }), "{error:?}");
    assert!(matches!(
        client.session_state(id.clone()).expect("known"),
        SessionState::LoggedOut
    ));
    let sent_before = backend.recorded().len();
    assert_eq!(
        client.media_sources(MEDIA_ID.to_owned()).await,
        Err(BeamError::Unauthenticated)
    );
    assert_eq!(
        backend.recorded().len(),
        sent_before,
        "the refused cookie is not on the client"
    );
    assert!(!storage.has_secret(&format!("session/{id}")));
}

const DEVICE_START: &str = r#"{"device_handle":"handle-1","user_code":"BCDF-GHJK",
    "verification_uri":"https://idp.test/device",
    "verification_uri_complete":"https://idp.test/device?user_code=BCDF-GHJK",
    "expires_in_secs":600,"interval_secs":5}"#;

const DEVICE_APPROVED: &str = r#"{"session_token":"issued-value","session_expires_in_secs":5184000,
    "user":{"id":"00000000-0000-0000-0000-0000000000a1","display_name":"Ada","is_admin":false}}"#;

/// A registered, signed-out server over `backend`.
async fn device_client(
    backend: CannedBackend,
) -> (
    Arc<BeamClient>,
    String,
    Arc<CannedBackend>,
    Arc<InMemoryKeyValueStore>,
) {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let id = client
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added")
        .id;
    let backend = Arc::new(backend);
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    (client, id, backend, storage)
}

/// Starting needs no session: a device with no browser has none yet.
#[tokio::test]
async fn a_device_login_starts_signed_out_and_carries_the_prompt_through() {
    let (client, id, backend, _) = device_client(CannedBackend::answering(
        200,
        "application/json",
        DEVICE_START,
    ))
    .await;

    let prompt = client
        .start_device_login(id.clone())
        .await
        .expect("the canned start is a prompt");

    assert_eq!(
        prompt,
        DeviceLoginPrompt {
            device_handle: "handle-1".to_owned(),
            user_code: "BCDF-GHJK".to_owned(),
            verification_uri: "https://idp.test/device".to_owned(),
            verification_uri_complete: Some(
                "https://idp.test/device?user_code=BCDF-GHJK".to_owned()
            ),
            expires_in_secs: 600,
            interval_secs: 5,
        }
    );
    let recorded = backend.recorded();
    assert_eq!(recorded[0].method, reqwest::Method::POST);
    assert_eq!(recorded[0].url.path(), "/v1/auth/device");
    assert!(
        recorded[0].headers.get(reqwest::header::ORIGIN).is_none(),
        "a native client sends no Origin, which the server's CSRF check allows"
    );
}

#[tokio::test]
async fn a_pending_poll_is_waiting_and_says_whether_to_slow_down() {
    for (body, slow_down) in [
        (
            r#"{"status":"authorization_pending","interval_secs":5}"#,
            false,
        ),
        (r#"{"status":"slow_down","interval_secs":10}"#, true),
    ] {
        let (client, id, _, storage) =
            device_client(CannedBackend::answering(202, "application/json", body)).await;

        let step = client
            .poll_device_login(id.clone(), "handle-1".to_owned())
            .await
            .expect("a 202 is a step, not an error");

        assert_eq!(
            step,
            DeviceLoginStep::Waiting {
                interval_secs: if slow_down { 10 } else { 5 },
                slow_down,
            }
        );
        assert!(!storage.has_secret(&format!("session/{id}")));
    }
}

/// An approval becomes this server's session exactly as a lifted cookie
/// does: installed, confirmed, persisted -- and then sent.
#[tokio::test]
async fn an_approved_device_login_installs_and_persists_the_issued_credential() {
    let (client, id, backend, storage) = device_client(CannedBackend::answering_in_turn(vec![
        (200, "application/json", DEVICE_APPROVED),
        (200, "application/json", ME),
    ]))
    .await;

    let step = client
        .poll_device_login(id.clone(), "handle-1".to_owned())
        .await
        .expect("approved");

    let DeviceLoginStep::SignedIn { user } = step else {
        panic!("expected a signed-in step, got {step:?}");
    };
    assert_eq!(user.id, "00000000-0000-0000-0000-0000000000a1");
    assert!(matches!(
        client.session_state(id.clone()).expect("known"),
        SessionState::Authenticated { .. }
    ));
    assert!(
        storage.has_secret(&format!("session/{id}")),
        "the issued credential survives a restart"
    );

    let recorded = backend.recorded();
    assert_eq!(recorded[0].url.path(), "/v1/auth/device/token");
    assert!(
        cookies_of(&recorded[0]).is_empty(),
        "the poll itself carries no session"
    );
    assert_eq!(recorded[1].url.path(), "/v1/me");
    assert_eq!(cookies_of(&recorded[1]), vec!["beam_session=issued-value"]);
}

#[tokio::test]
async fn a_refused_device_login_is_forbidden_and_stores_nothing() {
    let (client, id, _, storage) = device_client(CannedBackend::answering(
        403,
        "application/problem+json",
        r#"{"type":"https://beam.justinchung.net/reference/errors/#device-login-denied","status":403,"detail":"refused"}"#,
    ))
    .await;

    let error = client
        .poll_device_login(id.clone(), "handle-1".to_owned())
        .await
        .expect_err("a denial ends the flow");

    assert!(
        matches!(&error, BeamError::Forbidden { code, .. } if code.ends_with("#device-login-denied")),
        "{error:?}"
    );
    assert!(matches!(
        client.session_state(id.clone()).expect("known"),
        SessionState::LoggedOut
    ));
    assert!(!storage.has_secret(&format!("session/{id}")));
}

/// The fallback cue: a server whose identity provider has no device
/// grant says so with a 501 that no retry will change.
#[tokio::test]
async fn an_unsupported_device_login_is_a_permanent_501() {
    let (client, id, _, _) = device_client(CannedBackend::answering(
        501,
        "application/problem+json",
        r#"{"type":"https://beam.justinchung.net/reference/errors/#device-login-unsupported","status":501,"detail":"no grant"}"#,
    ))
    .await;

    let error = client
        .start_device_login(id)
        .await
        .expect_err("unsupported");

    assert!(
        matches!(
            &error,
            BeamError::Server { status: 501, retryable: false, code, .. }
                if code.ends_with("#device-login-unsupported")
        ),
        "{error:?}"
    );
}

/// Signing out everywhere ends this device's session too, credential
/// included.
#[tokio::test]
async fn logging_out_everywhere_withdraws_this_devices_credential() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
    let backend = Arc::new(CannedBackend::answering(204, "text/plain", ""));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    client
        .logout_everywhere()
        .await
        .expect("the canned 204 is the server's yes");

    assert_eq!(backend.recorded().len(), 1, "the logout-all itself");
    assert_eq!(
        client.media_sources(MEDIA_ID.to_owned()).await,
        Err(BeamError::Unauthenticated)
    );
    assert_eq!(
        backend.recorded().len(),
        1,
        "nothing else reaches the transport without a credential"
    );
    assert!(!storage.has_secret(&format!("session/{id}")));
    assert!(matches!(
        client.session_state(id).expect("known"),
        SessionState::LoggedOut
    ));
}

/// Withdrawing a certificate is a trust decision and says nothing about
/// who is signed in. The rebuild `forget_certificates` performs went
/// through `install`, which derives the state from the cookie alone, so a
/// signed-in server came back `Expired` -- and the viewer was told their
/// session had expired for having forgotten a certificate.
#[tokio::test]
async fn forgetting_certificates_keeps_the_viewer_signed_in() {
    let (client, id, _) = signed_in_client().await;
    client
        .trust_certificate(id.clone(), "AA:BB".to_owned())
        .await
        .expect("trusted");
    let before = client.session_state(id.clone()).expect("known");
    assert!(
        matches!(before, SessionState::Authenticated { .. }),
        "{before:?}"
    );

    client
        .forget_certificates(id.clone())
        .await
        .expect("forgotten");

    assert_eq!(
        client.session_state(id).expect("known"),
        before,
        "forgetting a certificate is not signing out"
    );
}

/// Withdrawing trust rebuilds the client; the session must come with it.
/// A viewer who forgets a certificate has not signed out.
#[tokio::test]
async fn forgetting_certificates_keeps_the_session_on_the_rebuilt_client() {
    let (client, id, _) = signed_in_client().await;
    client
        .trust_certificate(id.clone(), "AA:BB".to_owned())
        .await
        .expect("trusted");

    client
        .forget_certificates(id.clone())
        .await
        .expect("forgotten");

    // `install` put a real transport under the rebuilt client; the canned
    // one goes back in the way production swaps nothing else.
    let backend = Arc::new(CannedBackend::answering(
        200,
        "application/json",
        NO_SOURCES,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect("an empty list decodes");

    let recorded = backend.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(cookies_of(&recorded[0]), vec!["beam_session=opaque-value"]);
    assert!(client.trusted_certificates(id).expect("known").is_empty());
}

/// A certificate to drive the machine into `Untrusted` with. Never
/// verified against anything; the state is what the test is about.
fn certificate() -> crate::trust::CertificateDetails {
    crate::trust::CertificateDetails {
        sha256_fingerprint: "AA:BB".to_owned(),
        spki_sha256_base64: "c3BraQ==".to_owned(),
        subject: "CN=beam.test".to_owned(),
        issuer: "CN=beam.test".to_owned(),
        not_before_unix: 0,
        not_after_unix: i64::MAX,
        subject_alt_names: vec!["beam.test".to_owned()],
        serial_hex: "01".to_owned(),
        is_self_signed: true,
        is_expired: false,
    }
}

/// A 401 observed outside `Authenticated` must withdraw nothing and delete
/// nothing.
///
/// The machine emits no effects for one -- its own comment says a late 401
/// "must not clobber a sign-in already in progress" -- and every other
/// state falls through to that. `map_error` used to withdraw the
/// credential and `fail` to delete the stored cookie on *any* 401, read
/// off a flag rather than off the transition, so a late answer to a
/// cancelled request destroyed whatever session had replaced it.
#[tokio::test]
async fn a_401_outside_an_authenticated_session_withdraws_nothing() {
    for reached_by in [
        SessionEvent::LoginStarted,
        SessionEvent::LogoutRequested,
        SessionEvent::UnauthorizedObserved,
        SessionEvent::CertificateRejected(Box::new(certificate())),
    ] {
        let storage = Arc::new(InMemoryKeyValueStore::new());
        let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
        // Applied without performing the effects, so the state moves on
        // while the credential stays in place -- the shape a 401 racing a
        // sign-in or a sign-out actually has.
        let _effects = client
            .apply(&id, reached_by)
            .expect("the server is registered");
        let state = client.session_state(id.clone()).expect("known");

        let backend = Arc::new(CannedBackend::answering(
            401,
            "application/problem+json",
            r#"{"type":"about:blank","status":401}"#,
        ));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let error = client
            .media_sources(MEDIA_ID.to_owned())
            .await
            .expect_err("the canned 401 fails the call");

        assert_eq!(
            error,
            BeamError::Unauthenticated,
            "a 401 the machine declined to act on is not an expiry, from {state:?}"
        );
        assert_eq!(
            client.session_state(id.clone()).expect("known"),
            state,
            "the state is left where it was"
        );
        assert!(
            storage.has_secret(&format!("session/{id}")),
            "the stored cookie survives, from {state:?}"
        );
        assert!(
            client.server_http_config().is_ok(),
            "and the player keeps its copy, from {state:?}"
        );
    }
}

/// The keystore refusing the delete an expiry asks for is swallowed -- the
/// caller's answer is still the expired session -- but the cookie is then
/// still on the device. Restoring it would hand the client back the exact
/// credential the server has just rejected, so the server is marked and
/// its stored cookie is not registered again.
#[tokio::test]
async fn a_cookie_that_could_not_be_deleted_is_not_registered_again() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
    let secret_key = format!("session/{id}");
    assert!(storage.has_secret(&secret_key), "signing in stored it");

    let backend = Arc::new(CannedBackend::answering(
        401,
        "application/problem+json",
        r#"{"type":"about:blank","status":401}"#,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    // A locked keystore: reads still answer, writes and deletes do not.
    storage.set_failure(FailureMode::FailWrites);

    let error = client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect_err("the canned 401 fails the call");

    assert_eq!(
        error,
        BeamError::SessionExpired,
        "swallowing the delete must not change the caller's answer"
    );
    assert!(
        storage.has_secret(&secret_key),
        "the delete really did fail, which is what the mark is for"
    );

    client
        .restore()
        .await
        .expect("the registry itself is still readable");

    assert!(matches!(
        client.session_state(id).expect("known"),
        SessionState::LoggedOut
    ));
    assert_eq!(
        client.media_sources(MEDIA_ID.to_owned()).await,
        Err(BeamError::Unauthenticated),
        "the rejected cookie is not back on the client"
    );
    assert!(
        matches!(client.server_http_config(), Err(BeamError::Unauthenticated)),
        "nor handed to the player"
    );
}

/// A 401 whose failure is dropped -- a request that never passed through
/// [`BeamClient::send`] -- leaves the middleware's flag set with nothing
/// to consume it.
///
/// The flag belongs to the session that provoked it. Left standing across
/// a sign-in it was spent on the next unrelated failure, expiring a
/// credential the server had never rejected and deleting it from the
/// platform keystore, which is not recoverable.
#[tokio::test]
async fn a_401_no_call_consumed_cannot_expire_the_session_that_replaced_it() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;

    let rejecting = Arc::new(CannedBackend::answering(
        401,
        "application/problem+json",
        r#"{"type":"about:blank","status":401}"#,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&rejecting) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    // A request made around `send`, so nothing hands its 401 to the
    // session machine -- the shape a dropped failure has.
    let generated = client.client_for(&id).expect("the server is registered");
    let dropped = TransportFailure::capture(
        generated.get_media_detail(parse_uuid("media_id", MEDIA_ID).expect("a UUID")),
    )
    .await;
    assert!(
        dropped.is_err(),
        "the 401 is dropped, which is what leaves the flag unconsumed"
    );

    // The user signs in again: a new cookie, which no server has rejected.
    let confirming = Arc::new(CannedBackend::answering(200, "application/json", ME));
    client
        .use_transport(
            &id,
            Arc::clone(&confirming) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    client
        .complete_login(id.clone(), "second-value".to_owned())
        .await
        .expect("the canned /v1/me confirms the new cookie");

    // Then something entirely unrelated fails under it.
    let missing = Arc::new(CannedBackend::answering(
        404,
        "application/problem+json",
        r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404,"detail":"no such title"}"#,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&missing) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let error = client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect_err("the canned 404 fails the call");

    assert!(matches!(error, BeamError::NotFound { .. }), "{error:?}");
    assert!(matches!(
        client.session_state(id.clone()).expect("known"),
        SessionState::Authenticated { .. }
    ));
    assert!(
        storage.has_secret(&format!("session/{id}")),
        "the second session is still on the device"
    );
    assert!(client.server_http_config().is_ok());
}

/// A problem `type` the contract does not list for the status is lost,
/// status and all.
///
/// This pins a gap, not a preference: the generated client decodes the
/// `type` as a closed set and reports anything else as a decode failure
/// that carries no status, so a code the server adds after a client ships
/// turns a documented 404 into a malformed response (getkono/spargen#268).
/// Expected to fail when that is fixed -- the failure is the reminder to
/// classify this as the 404 it is.
#[tokio::test]
async fn a_problem_type_the_contract_does_not_list_loses_its_status() {
    let (client, _) = client_answering(
        404,
        r#"{"type":"https://beam.justinchung.net/reference/errors/#source-file-missing","status":404}"#,
    )
    .await;

    let error = client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect_err("the canned 404 fails the call");

    assert!(matches!(error, BeamError::Protocol { .. }), "{error:?}");
}

/// A 429 whose body is not Beam's `rate-limited` problem -- a reverse
/// proxy's own limiter answering in HTML -- is lost the same way.
///
/// `browse_media` declares its 429 as that problem, so the generated
/// client decodes the body, fails, and reports a decode failure with no
/// status: the caller sees a malformed response rather than
/// `RateLimited`, and loses `Retry-After` (getkono/spargen#268). Expected
/// to fail when that is fixed.
#[tokio::test]
async fn a_429_a_proxy_wrote_is_not_recognised_as_rate_limiting() {
    let (client, id, _) = signed_in_client().await;
    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(
                429,
                "text/html",
                "<html><body>Too Many Requests</body></html>",
            )),
        )
        .expect("the server is registered");

    let error = client
        .browse_media(BrowseQuery::default())
        .await
        .expect_err("the canned 429 fails the call");

    assert!(matches!(error, BeamError::Protocol { .. }), "{error:?}");
}

/// The document the middleware captured is the one the error carries.
///
/// Two 404s a viewer must be told about differently is what issue #123
/// opened with; that only works if the `type` survives the trip from the
/// scope into `classify`.
#[tokio::test]
async fn a_failed_call_carries_its_own_problem_document_into_the_error() {
    let (client, _) = client_answering(
        404,
        r#"{"type":"https://beam.justinchung.net/reference/errors/#media-not-found","status":404,"detail":"Media not found"}"#,
    )
    .await;

    let error = client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect_err("the canned 404 fails the call");

    assert_eq!(
        error,
        BeamError::NotFound {
            detail: "Media not found".to_owned(),
            code: "https://beam.justinchung.net/reference/errors/#media-not-found".to_owned(),
        }
    );
}

fn failure(
    kind: FailureKind,
    problem: Option<crate::transport::ProblemDetail>,
) -> TransportFailure {
    TransportFailure {
        kind,
        retry_after_secs: None,
        message: "the transport's own words".to_owned(),
        problem,
    }
}

/// The shapes a failure can take that classify from the response alone,
/// with and without a document.
///
/// Only an answered request has a document to classify from; the others
/// never saw a response, so a document on them would be a bug upstream and
/// is ignored rather than trusted. `NoCredential` is the one kind that is
/// read against the session instead, and has its own test below.
#[tokio::test]
async fn map_error_classifies_from_the_status_and_the_document_it_was_given() {
    let (client, id) = client_with_server().await;
    let document = crate::transport::ProblemDetail {
        type_uri: "https://beam.justinchung.net/reference/errors/#media-not-found".to_owned(),
        detail: Some("media 7 not found".to_owned()),
        pointers: Vec::new(),
    };

    assert_eq!(
        client.map_error(
            &id,
            &failure(FailureKind::Answered(404), Some(document.clone()))
        ),
        BeamError::NotFound {
            detail: "media 7 not found".to_owned(),
            code: "https://beam.justinchung.net/reference/errors/#media-not-found".to_owned(),
        }
    );
    assert!(
        matches!(
            client.map_error(&id, &failure(FailureKind::Answered(404), None)),
            BeamError::NotFound { code, .. } if code == ABOUT_BLANK
        ),
        "no document means the status is the whole story"
    );

    for problem in [None, Some(document.clone())] {
        assert_eq!(
            client.map_error(&id, &failure(FailureKind::Unreachable, problem.clone())),
            BeamError::Network {
                detail: "the transport's own words".to_owned(),
                retryable: true,
            }
        );
        assert_eq!(
            client.map_error(&id, &failure(FailureKind::Malformed, problem.clone())),
            BeamError::Protocol {
                detail: "the transport's own words".to_owned(),
            }
        );
        assert_eq!(
            client.map_error(&id, &failure(FailureKind::Unbuildable, problem)),
            BeamError::Protocol {
                detail: "the transport's own words".to_owned(),
            }
        );
    }
}

/// A refused credential is read against the session -- which of the two
/// unauthenticated answers to give -- while a request that genuinely could
/// not be built is read against nothing at all. The two used to be one
/// kind told apart by whether a cookie happened to be registered.
#[tokio::test]
async fn a_refused_credential_is_read_against_the_session_and_a_failed_build_is_not() {
    let (logged_out, id) = client_with_server().await;
    assert_eq!(
        logged_out.map_error(&id, &failure(FailureKind::NoCredential, None)),
        BeamError::Unauthenticated
    );

    let (signed_in, id, _) = signed_in_client().await;
    signed_in
        .apply(&id, SessionEvent::UnauthorizedObserved)
        .expect("registered");
    assert_eq!(
        signed_in.map_error(&id, &failure(FailureKind::NoCredential, None)),
        BeamError::SessionExpired,
        "a device whose session the server rejected keeps its work in progress"
    );
}

/// The credential is read when a request is built, not baked into the
/// client at construction. Every fan-out holds a client clone taken before
/// the call it is part of, so a clone that went on carrying a withdrawn
/// cookie would send it after the sign-out that withdrew it.
#[tokio::test]
async fn a_client_taken_before_a_sign_out_stops_carrying_the_cookie() {
    let (client, id, backend) = signed_in_client().await;
    let held = client.client_for(&id).expect("the server is registered");

    client.logout(id).await.expect("logged out");
    let sent_before = backend.recorded().len();

    let refused = held
        .get_current_user()
        .await
        .expect_err("no cookie, no request");

    assert!(
        matches!(refused, crate::api::Error::RequestConstruction(_)),
        "{refused:?}"
    );
    assert_eq!(
        backend.recorded().len(),
        sent_before,
        "the withdrawn cookie must not reach the wire on a client handed out earlier"
    );
}

#[tokio::test]
async fn a_storage_failure_when_adding_a_server_is_reported_not_swallowed() {
    // A server that appears to be added but was never written would come
    // back missing on the next start, with nothing having said so.
    let storage = Arc::new(InMemoryKeyValueStore::new());
    storage.set_failure(FailureMode::FailWrites);
    let client = BeamClient::new(storage as Arc<dyn KeyValueStore>);

    assert!(matches!(
        client
            .add_server("https://beam.test".to_owned(), None)
            .await,
        Err(BeamError::Storage { .. })
    ));
}

// -- characterization: several servers -------------------------------

const JSON: &str = "application/json";
const PROBLEM: &str = "application/problem+json";

/// Two files the progress tests report on; what they name never matters.
const FILE_A: &str = "a0000000-0000-4000-8000-00000000000a";
const FILE_B: &str = "b0000000-0000-4000-8000-00000000000b";

const NO_PAGES: &str = r#""page_info":{"has_next_page":false,"has_previous_page":false}"#;

/// Register `base_url` on `client` and sign in to it with `cookie`,
/// through the production path, against a canned `/v1/me`.
async fn sign_in_to(client: &BeamClient, base_url: &str, cookie: &str) -> String {
    let id = client
        .add_server(base_url.to_owned(), None)
        .await
        .expect("adding a server is offline")
        .id;
    client
        .use_transport(&id, Arc::new(CannedBackend::answering(200, JSON, ME)))
        .expect("the server is registered");
    client
        .complete_login(id.clone(), cookie.to_owned())
        .await
        .expect("the canned /v1/me confirms the cookie");
    id
}

/// Point `server_id` at a backend giving these answers in turn, and hand
/// it back so the test can read what reached it.
fn answer_with(
    client: &BeamClient,
    server_id: &str,
    answers: Vec<crate::transport::CannedAnswer>,
) -> Arc<CannedBackend> {
    let backend = Arc::new(CannedBackend::saying(answers));
    client
        .use_transport(
            server_id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    backend
}

fn genres_answering(name: &'static str) -> crate::transport::CannedAnswer {
    let body: &'static str =
        Box::leak(format!(r#"{{"items":["{name}"],{NO_PAGES}}}"#).into_boxed_str());
    crate::transport::CannedAnswer::respond(200, JSON, body)
}

/// Each server has its own transport and its own cookie, and selecting
/// one is what decides which a call uses. Every earlier test registered
/// one server, so a call reaching the wrong server -- or carrying the
/// other server's cookie, which is a credential leak across origins --
/// could not have failed any of them.
#[tokio::test]
async fn switching_servers_routes_calls_and_cookies_to_the_selected_one() {
    let client = client();
    let home = sign_in_to(&client, "https://home.beam.test", "home-cookie").await;
    let cabin = sign_in_to(&client, "https://cabin.beam.test", "cabin-cookie").await;
    let home_backend = answer_with(&client, &home, vec![genres_answering("Home")]);
    let cabin_backend = answer_with(&client, &cabin, vec![genres_answering("Cabin")]);

    // The server added last is the active one.
    assert_eq!(
        client.genres().await.expect("answered"),
        vec!["Cabin".to_owned()]
    );
    client.select_server(home.clone()).await.expect("known");
    assert_eq!(
        client.genres().await.expect("answered"),
        vec!["Home".to_owned()]
    );

    let to_home = home_backend.recorded();
    let to_cabin = cabin_backend.recorded();
    assert_eq!((to_home.len(), to_cabin.len()), (1, 1));
    assert_eq!(to_home[0].url.host_str(), Some("home.beam.test"));
    assert_eq!(cookies_of(&to_home[0]), vec!["beam_session=home-cookie"]);
    assert_eq!(to_cabin[0].url.host_str(), Some("cabin.beam.test"));
    assert_eq!(cookies_of(&to_cabin[0]), vec!["beam_session=cabin-cookie"]);

    let player = client.server_http_config().expect("signed in");
    assert_eq!(player.base_url, "https://home.beam.test");
    assert_eq!(
        player.headers.get("Cookie").map(String::as_str),
        Some("beam_session=home-cookie"),
        "the player is handed the selected server's cookie"
    );
    let active: Vec<String> = client
        .list_servers()
        .expect("listable")
        .into_iter()
        .filter(|server| server.is_active)
        .map(|server| server.id)
        .collect();
    assert_eq!(active, vec![home]);
}

#[tokio::test]
async fn removing_an_inactive_server_keeps_the_active_one() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let client = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let home = sign_in_to(&client, "https://home.beam.test", "home-cookie").await;
    let cabin = sign_in_to(&client, "https://cabin.beam.test", "cabin-cookie").await;

    client.remove_server(home.clone()).await.expect("removed");

    let servers = client.list_servers().expect("listable");
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].id, cabin);
    assert!(servers[0].is_active, "the active server stays active");
    assert!(matches!(
        servers[0].state,
        SessionState::Authenticated { .. }
    ));
    assert!(storage.has_secret(&session_key(&cabin)));
    assert!(!storage.has_secret(&session_key(&home)));

    // And it is still the active, signed-in server after a cold start.
    let restarted = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    let restored = restarted.restore().await.expect("restored");
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].id, cabin);
    assert!(restored[0].is_active);
    assert!(matches!(
        restored[0].state,
        SessionState::Authenticated { .. }
    ));
}

// -- characterization: restore over a damaged store ------------------

#[tokio::test]
async fn a_store_that_cannot_be_read_fails_the_restore() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>)
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added");
    storage.set_failure(FailureMode::FailReads);

    let restarted = BeamClient::new(storage as Arc<dyn KeyValueStore>);

    assert!(matches!(
        restarted.restore().await,
        Err(BeamError::Storage { .. })
    ));
    assert!(restarted.list_servers().expect("listable").is_empty());
}

#[tokio::test]
async fn a_corrupt_index_restores_no_servers_rather_than_failing() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>)
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added");
    storage
        .put(SERVER_INDEX_KEY.to_owned(), "{not json".to_owned())
        .await
        .expect("writable");

    let restarted = BeamClient::new(storage as Arc<dyn KeyValueStore>);

    assert_eq!(restarted.restore().await.expect("not an error"), Vec::new());
}

#[tokio::test]
async fn a_corrupt_record_is_skipped_and_the_other_servers_restore() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let first = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let home = first
        .add_server("https://home.beam.test".to_owned(), None)
        .await
        .expect("added")
        .id;
    let cabin = first
        .add_server("https://cabin.beam.test".to_owned(), None)
        .await
        .expect("added")
        .id;
    storage
        .put(format!("servers/{home}"), "{not json".to_owned())
        .await
        .expect("writable");

    let restarted = BeamClient::new(storage as Arc<dyn KeyValueStore>);
    let restored = restarted.restore().await.expect("not an error");

    let ids: Vec<&str> = restored.iter().map(|server| server.id.as_str()).collect();
    assert_eq!(ids, vec![cabin.as_str()]);
    assert!(restored[0].is_active);
}

// -- characterization: progress through the façade -------------------

/// A signed-in client over a clock the test moves and a storage it reads.
async fn clocked_client() -> (
    Arc<BeamClient>,
    String,
    Arc<InMemoryKeyValueStore>,
    Arc<crate::clock::TestClock>,
) {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let clock = Arc::new(crate::clock::TestClock::new(1_000_000));
    let client = BeamClient::over(
        Arc::clone(&storage) as Arc<dyn KeyValueStore>,
        Arc::clone(&clock) as Arc<dyn Clock>,
    );
    let id = sign_in_to(&client, "https://beam.test", "opaque-value").await;
    (client, id, storage, clock)
}

/// The progress queue as persisted, oldest first.
async fn queued(storage: &InMemoryKeyValueStore, server_id: &str) -> Vec<QueuedProgress> {
    storage
        .get(format!("progress_queue/{server_id}"))
        .await
        .expect("readable")
        .map(|raw| serde_json::from_str(&raw).expect("the queue decodes"))
        .unwrap_or_default()
}

fn json_body(request: &crate::transport::RecordedRequest) -> serde_json::Value {
    serde_json::from_slice(&request.body).expect("a JSON body")
}

#[tokio::test]
async fn a_malformed_success_body_is_protocol() {
    let (client, id, _storage, _clock) = clocked_client().await;
    answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::respond(
            200,
            JSON,
            r#"{"items":"not a list"}"#,
        )],
    );

    let error = client.genres().await.expect_err("does not decode");

    assert!(matches!(error, BeamError::Protocol { .. }), "{error:?}");
    assert!(error.is_permanent(), "the same body fails the same way");
    assert!(matches!(
        client.session_state(id).expect("known"),
        SessionState::Authenticated { .. }
    ));
}

/// A backend that performs a real TLS handshake, in memory, against a
/// server presenting a self-signed certificate -- through the verifier the
/// façade built for this server -- and reports the failed connection.
#[derive(Debug)]
struct SelfSignedServer {
    trust: Arc<crate::tls::TrustDecision>,
    host: &'static str,
    /// What the transport reports once the handshake has failed.
    refused: CannedBackend,
}

impl SelfSignedServer {
    fn handshake(&self) {
        use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};

        let certified =
            rcgen::generate_simple_self_signed(vec![self.host.to_owned()]).expect("generate");
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certified.signing_key.serialize_der(),
        ));
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![certified.cert.der().clone()], key)
        .expect("a server config");
        let client_config =
            crate::tls::client_config(Arc::clone(&self.trust)).expect("a client config");

        let mut server = rustls::ServerConnection::new(Arc::new(server_config)).expect("a server");
        let mut client = rustls::ClientConnection::new(
            Arc::new(client_config),
            ServerName::try_from(self.host).expect("a name"),
        )
        .expect("a client");
        for _ in 0..8 {
            let mut bytes = Vec::new();
            client.write_tls(&mut bytes).expect("writable");
            server.read_tls(&mut bytes.as_slice()).expect("readable");
            server
                .process_new_packets()
                .expect("the server accepts the hello");
            let mut bytes = Vec::new();
            server.write_tls(&mut bytes).expect("writable");
            client.read_tls(&mut bytes.as_slice()).expect("readable");
            if client.process_new_packets().is_err() {
                return;
            }
        }
        panic!("the handshake was expected to be refused");
    }
}

impl crate::api::HttpBackend for SelfSignedServer {
    fn execute(&self, request: reqwest::Request) -> crate::api::ExecuteFuture<'_> {
        self.handshake();
        self.refused.execute(request)
    }
}

/// A certificate nobody vouches for surfaces as `UntrustedCertificate`,
/// carrying what the prompt shows -- not as the generic network failure
/// the handshake looks like from the transport. The verifier is the real
/// one the façade built for the server.
#[tokio::test]
async fn a_rejected_handshake_surfaces_untrusted_certificate_with_details() {
    crate::tls::install_crypto_provider();
    let (client, id, _storage, _clock) = clocked_client().await;
    let trust = client.trust_for(&id).expect("registered");
    client
        .use_transport(
            &id,
            Arc::new(SelfSignedServer {
                trust,
                host: "beam.test",
                refused: CannedBackend::unreachable(),
            }),
        )
        .expect("the server is registered");

    let error = client.genres().await.expect_err("refused");

    let BeamError::UntrustedCertificate { host, details } = error else {
        panic!("expected an untrusted certificate, got {error:?}");
    };
    assert_eq!(host, "beam.test");
    assert!(details.is_self_signed);
    assert!(details.subject_alt_names.contains(&"beam.test".to_owned()));
    assert_eq!(
        details.sha256_fingerprint.split(':').count(),
        32,
        "a whole SHA-256 digest, for the user to compare"
    );
    assert!(
        client
            .trust_for(&id)
            .expect("registered")
            .take_rejection()
            .is_none(),
        "the rejection is consumed by the error that reported it"
    );
}

// -- characterization: the remaining operations -----------------------

/// A signed-in client whose server gives `body` with `status` to the next
/// call, and the backend that records it.
async fn one_answer(status: u16, body: &'static str) -> (Arc<BeamClient>, Arc<CannedBackend>) {
    let (client, id, _storage, _clock) = clocked_client().await;
    let backend = answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::respond(status, JSON, body)],
    );
    (client, backend)
}

/// The one request a [`one_answer`] backend received.
fn the_request(backend: &CannedBackend) -> crate::transport::RecordedRequest {
    let recorded = backend.recorded();
    assert_eq!(recorded.len(), 1, "exactly one request: {recorded:?}");
    recorded[0].clone()
}

fn query_of(request: &crate::transport::RecordedRequest) -> HashMap<String, String> {
    request.url.query_pairs().into_owned().collect()
}

const LIBRARY: &str = r#"{"id":"00000000-0000-0000-0000-00000000000a","name":"Films",
    "description":"Feature films","file_count":42,"last_scan_file_count":40,
    "last_scan_started_at":"2026-01-01T00:00:00Z",
    "last_scan_finished_at":"2026-01-01T00:10:00Z"}"#;

#[tokio::test]
async fn health_is_the_servers_own_report() {
    let (client, backend) = one_answer(
        200,
        r#"{"status":"degraded","version":"1.2.3","uptime_secs":77,
            "checked_at":"2026-01-01T00:00:00Z",
            "checks":{"database":{"status":"error","detail":"timed out"}}}"#,
    )
    .await;

    let health = client.health().await.expect("answered");

    assert_eq!(
        health,
        crate::catalog::ServerHealth {
            status: "degraded".to_owned(),
            version: "1.2.3".to_owned(),
            uptime_secs: 77,
            database: "error: timed out".to_owned(),
        }
    );
    assert_eq!(the_request(&backend).url.path(), "/v1/health");
}

#[tokio::test]
async fn revoking_a_session_deletes_it_by_id() {
    const SESSION_ID: &str = "5e5e5e5e-0000-4000-8000-000000000001";
    let (client, backend) = one_answer(204, "").await;

    client
        .revoke_session(SESSION_ID.to_owned())
        .await
        .expect("revoked");

    let request = the_request(&backend);
    assert_eq!(request.method, reqwest::Method::DELETE);
    assert_eq!(request.url.path(), format!("/v1/sessions/{SESSION_ID}"));
    assert!(matches!(
        client.revoke_session("this one".to_owned()).await,
        Err(BeamError::BadRequest { .. })
    ));
    assert_eq!(backend.recorded().len(), 1, "a malformed id is never sent");
}
