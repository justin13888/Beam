//! What happens once an IdP has vouched for somebody, however it was asked.
//!
//! Beam has two ways to reach a verified [`OidcIdentity`]: the browser's
//! Authorization Code + PKCE round-trip and a native client's device
//! authorization grant (ADR-0017). Everything after that point -- admin
//! from the configured claim, JIT provisioning, the disabled-account gate,
//! the profile refresh, and the session row -- is one function, so the two
//! logins cannot drift apart on who may sign in or for how long.

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::utils::admin_claim::admin_claim_matches;
use crate::utils::device_auth_store::{Claim, DeviceAuthError, DeviceAuthStore, hash_handle};
use crate::utils::models::{CreateUser, User};
use crate::utils::oidc::{DevicePoll, OidcClient, OidcError, OidcIdentity};
use crate::utils::oidc_config::OidcRuntimeConfig;
use crate::utils::repository::UserRepository;
use crate::utils::session_store::{SessionData, SessionStore};

/// Who is signing in from where, as the session record describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientContext {
    /// The `User-Agent`, hashed into the session's device fingerprint.
    pub user_agent: Option<String>,
    /// The client address, as the transport resolved it: the socket peer, or
    /// the client a trusted proxy names. Resolving it is the HTTP adapter's
    /// job, so this crate never reads a forwarding header itself.
    pub ip: String,
}

/// A session minted for a verified identity.
#[derive(Debug, Clone)]
pub struct MintedSession {
    /// The plaintext opaque credential. Only its hash is stored.
    pub token: String,
    /// The user as of this login: profile and admin already refreshed.
    pub user: User,
    /// The session's hard lifetime, in seconds.
    pub absolute_ttl_secs: u64,
}

#[derive(Debug, Error)]
pub enum LoginError {
    /// The identity verified; the local account is disabled (issue #85).
    #[error("This account has been disabled. Contact an administrator.")]
    AccountDisabled,
    #[error("{0}")]
    Internal(String),
}

/// Signs `identity` in: evaluates admin, provisions or refreshes the user,
/// refuses a disabled account, and mints a session.
///
/// # Errors
///
/// [`LoginError::AccountDisabled`] for a disabled account -- no session is
/// minted and no field of the account is touched. [`LoginError::Internal`]
/// when the user repository or the session store fails.
pub async fn complete_login(
    identity: &OidcIdentity,
    client: &ClientContext,
    user_repo: &dyn UserRepository,
    session_store: &dyn SessionStore,
    config: &OidcRuntimeConfig,
) -> Result<MintedSession, LoginError> {
    // Admin is derived solely from a configured ID-token claim asserted by the
    // IdP (issue #85): the IdP is the single authority. Recomputed on every
    // login below, so it both grants and revokes -- and with no admin claim
    // configured, `false` here demotes any previously-admin user at next login.
    let is_admin = match config.admin_claim.as_deref() {
        Some(claim_name) => {
            admin_claim_matches(&identity.claims, claim_name, config.admin_value.as_deref())
        }
        None => false,
    };
    let display_name = derive_display_name(
        identity.name.as_deref(),
        identity.email.as_deref(),
        &identity.subject,
    );

    let internal = |e: sea_orm::DbErr| LoginError::Internal(e.to_string());

    let user = match user_repo
        .find_by_oidc_identity(&identity.issuer, &identity.subject)
        .await
        .map_err(internal)?
    {
        Some(existing) => {
            // A disabled account is blocked at the door: no session is minted
            // and no profile/admin fields are touched (issue #85). Only an
            // already-provisioned account can be disabled -- JIT-provisioned
            // new users below are always created enabled.
            if existing.disabled {
                return Err(LoginError::AccountDisabled);
            }
            if existing.is_admin != is_admin {
                user_repo
                    .set_admin(existing.id, is_admin)
                    .await
                    .map_err(internal)?;
            }
            if existing.display_name != display_name || existing.avatar_url != identity.picture {
                user_repo
                    .update_oidc_profile(
                        existing.id,
                        display_name.clone(),
                        identity.picture.clone(),
                    )
                    .await
                    .map_err(internal)?;
            }
            User {
                is_admin,
                display_name,
                avatar_url: identity.picture.clone(),
                ..existing
            }
        }
        None => user_repo
            .create(CreateUser {
                oidc_issuer: identity.issuer.clone(),
                oidc_subject: identity.subject.clone(),
                email: identity.email.clone(),
                display_name,
                avatar_url: identity.picture.clone(),
                is_admin,
            })
            .await
            .map_err(|e| LoginError::Internal(format!("Failed to provision user: {e}")))?,
    };

    let idle_ttl_secs = config.idle_ttl_secs();
    let absolute_ttl_secs = config.absolute_ttl_secs();
    // The store's clock, not the wall clock: every expiry the store enforces
    // is measured against it, so the stamps it is handed must be too.
    let now = session_store.now().timestamp();

    let session_data = SessionData {
        user_id: user.id.to_string(),
        device_hash: device_hash(client.user_agent.as_deref()),
        ip: client.ip.clone(),
        created_at: now,
        last_active: now,
    };

    let token = session_store
        .create(&session_data, idle_ttl_secs, absolute_ttl_secs)
        .await
        .map_err(|e| LoginError::Internal(e.to_string()))?;

    Ok(MintedSession {
        token,
        user,
        absolute_ttl_secs,
    })
}

/// What one poll of a device login (RFC 8628) came to, when it did not fail.
#[derive(Debug, Clone)]
pub enum DevicePollOutcome {
    /// The user has not approved yet. Poll again after `interval_secs`.
    AuthorizationPending { interval_secs: u32 },
    /// The poll came too soon, by Beam's own pacing or the IdP's: the
    /// interval has grown to `interval_secs`.
    SlowDown { interval_secs: u32 },
    /// The user approved; the flow is over and this session was minted.
    SignedIn(MintedSession),
}

/// Why a device-login poll ended without a session or a "keep waiting".
///
/// Each message is the one the transport reports, so an adapter maps a
/// variant to its status and passes the `Display` text through.
#[derive(Debug, Error)]
pub enum DevicePollError {
    /// No open device login has this handle: it was never issued, or the
    /// flow already ended -- including by a concurrent poll that won the
    /// approval.
    #[error("Unknown or already-finished device login")]
    Invalid,
    /// The flow outlived its lifetime, by Beam's clock or the IdP's. The
    /// flow is ended.
    #[error("The device login expired before it was approved")]
    Expired,
    /// The user refused at the IdP. The flow is ended.
    #[error("The sign-in was refused at the identity provider")]
    Denied,
    /// The identity verified; the local account is disabled (issue #85).
    #[error("{}", LoginError::AccountDisabled)]
    AccountDisabled,
    /// The IdP's answer did not verify, and will not on a retry. The flow is
    /// ended.
    #[error("Login failed: {0}")]
    LoginFailed(OidcError),
    /// The IdP could not be reached, or refused Beam itself. The flow is kept:
    /// polling again may succeed.
    #[error("OIDC login unavailable: {0}")]
    IdpUnavailable(OidcError),
    /// A store failed.
    #[error("{0}")]
    Internal(String),
}

impl From<DeviceAuthError> for DevicePollError {
    fn from(err: DeviceAuthError) -> Self {
        Self::Internal(err.to_string())
    }
}

/// Polls the device login `device_handle` names, once.
///
/// Reaches the IdP at most once, and not at all when the poll comes sooner
/// than the flow's interval allows -- that answers
/// [`DevicePollOutcome::SlowDown`] and grows the interval, as RFC 8628
/// section 3.5 has the IdP do. On approval the flow is ended *before* the
/// session is minted, which is what makes an approval single-use, and the
/// session is minted by [`complete_login`] exactly as a browser login's is.
///
/// # Errors
///
/// See [`DevicePollError`]: every variant but `IdpUnavailable` and
/// `Internal` means the flow is over.
pub async fn poll_device_login(
    device_handle: &str,
    client: &ClientContext,
    oidc_client: &dyn OidcClient,
    device_auth_store: &dyn DeviceAuthStore,
    user_repo: &dyn UserRepository,
    session_store: &dyn SessionStore,
    config: &OidcRuntimeConfig,
) -> Result<DevicePollOutcome, DevicePollError> {
    let handle_hash = hash_handle(device_handle);

    let auth = match device_auth_store.claim_poll(&handle_hash).await? {
        Claim::NotFound => return Err(DevicePollError::Invalid),
        Claim::Expired => {
            device_auth_store.consume(&handle_hash).await?;
            return Err(DevicePollError::Expired);
        }
        Claim::TooEarly { interval_secs } => {
            // The IdP is not asked: answering for it is the point.
            let interval_secs = device_auth_store
                .bump_interval(&handle_hash)
                .await?
                .unwrap_or(interval_secs);
            return Ok(DevicePollOutcome::SlowDown { interval_secs });
        }
        Claim::Claimed(auth) => auth,
    };

    let identity = match oidc_client.poll_device_token(&auth.device_code).await {
        Ok(DevicePoll::Pending) => {
            return Ok(DevicePollOutcome::AuthorizationPending {
                interval_secs: auth.interval_secs,
            });
        }
        Ok(DevicePoll::SlowDown) => {
            let interval_secs = device_auth_store
                .bump_interval(&handle_hash)
                .await?
                .unwrap_or(auth.interval_secs);
            return Ok(DevicePollOutcome::SlowDown { interval_secs });
        }
        Ok(DevicePoll::Denied) => {
            device_auth_store.consume(&handle_hash).await?;
            return Err(DevicePollError::Denied);
        }
        Ok(DevicePoll::Expired) => {
            device_auth_store.consume(&handle_hash).await?;
            return Err(DevicePollError::Expired);
        }
        Ok(DevicePoll::Complete(identity)) => identity,
        // The IdP could not be reached, or Beam is misconfigured against it:
        // nothing about this flow is wrong, so it is kept for the next poll.
        Err(
            e @ (OidcError::Discovery(_)
            | OidcError::Exchange(_)
            | OidcError::DeviceFlowUnsupported),
        ) => return Err(DevicePollError::IdpUnavailable(e)),
        // An answer that did not verify will not verify on a retry.
        Err(e @ (OidcError::MissingIdToken | OidcError::ClaimsVerification(_))) => {
            device_auth_store.consume(&handle_hash).await?;
            return Err(DevicePollError::LoginFailed(e));
        }
    };

    // Ending the flow before minting is what makes an approval single-use: of
    // two polls that both saw it approved, only the one that removes the row
    // signs in.
    if device_auth_store.consume(&handle_hash).await?.is_none() {
        return Err(DevicePollError::Invalid);
    }

    let minted = complete_login(&identity, client, user_repo, session_store, config)
        .await
        .map_err(|e| match e {
            LoginError::AccountDisabled => DevicePollError::AccountDisabled,
            LoginError::Internal(message) => DevicePollError::Internal(message),
        })?;
    Ok(DevicePollOutcome::SignedIn(minted))
}

/// The session's device fingerprint: the SHA-256 of the `User-Agent`.
#[must_use]
pub fn device_hash(user_agent: Option<&str>) -> String {
    // Session rows are compared by equality against this string, so it is
    // produced by the workspace's one encoder rather than a local `{:x}`.
    beam_domain::utils::hex::encode_lower(&Sha256::digest(user_agent.unwrap_or("").as_bytes()))
}

/// Picks a display name when the IdP doesn't release a `name` claim: the
/// local part of the email if one is available, else a subject-derived
/// placeholder. Real IdPs (including Dex) send `name`, so this is a rare
/// fallback, not the common case.
#[must_use]
pub(crate) fn derive_display_name(
    name: Option<&str>,
    email: Option<&str>,
    subject: &str,
) -> String {
    if let Some(name) = name
        && !name.is_empty()
    {
        return name.to_owned();
    }
    if let Some(local_part) = email.and_then(|e| e.split('@').next())
        && !local_part.is_empty()
    {
        return local_part.to_owned();
    }
    format!("user-{subject}")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use beam_domain::services::{Clock, TestClock};
    use serde_json::{Value, json};

    use super::*;
    use crate::utils::models::test_utils::create_user;
    use crate::utils::repository::in_memory::{
        FailingUserRepository, InMemoryUserRepository, UserRepositoryOperation,
    };
    use crate::utils::session_store::in_memory::{
        FailingSessionStore, InMemorySessionStore, SessionStoreOperation,
    };

    fn config(admin_claim: Option<&str>) -> OidcRuntimeConfig {
        OidcRuntimeConfig {
            web_url: "http://localhost:5173".to_owned(),
            cookie_secure: false,
            admin_claim: admin_claim.map(str::to_owned),
            admin_value: Some("beam-admin".to_owned()),
            session_idle_days: 14,
            session_max_days: 60,
        }
    }

    fn identity(claims: Value) -> OidcIdentity {
        OidcIdentity {
            issuer: "https://idp.test".to_owned(),
            subject: "subj-1".to_owned(),
            email: Some("ada@example.com".to_owned()),
            email_verified: true,
            name: Some("Ada".to_owned()),
            picture: None,
            claims,
        }
    }

    fn client() -> ClientContext {
        ClientContext {
            user_agent: Some("BeamTV/1.0".to_owned()),
            ip: "203.0.113.7".to_owned(),
        }
    }

    struct World {
        clock: Arc<TestClock>,
        users: InMemoryUserRepository,
        sessions: InMemorySessionStore,
    }

    fn world() -> World {
        let clock = Arc::new(TestClock::new());
        World {
            users: InMemoryUserRepository::default(),
            sessions: InMemorySessionStore::new(clock.clone()),
            clock,
        }
    }

    async fn login(world: &World, claims: Value, admin_claim: Option<&str>) -> MintedSession {
        complete_login(
            &identity(claims),
            &client(),
            &world.users,
            &world.sessions,
            &config(admin_claim),
        )
        .await
        .expect("an enabled identity signs in")
    }

    #[tokio::test]
    async fn a_first_login_provisions_the_user_and_mints_a_session_that_resolves_to_them() {
        let world = world();
        let minted = login(&world, json!({}), None).await;

        let session = world
            .sessions
            .get(&minted.token)
            .await
            .unwrap()
            .expect("the minted token is a live session");
        assert_eq!(session.user_id, minted.user.id.to_string());
        assert_eq!(session.ip, "203.0.113.7");
        assert_eq!(session.device_hash, device_hash(Some("BeamTV/1.0")));
        assert_eq!(session.created_at, world.clock.now().timestamp());
        assert_eq!(minted.absolute_ttl_secs, config(None).absolute_ttl_secs());

        let stored = world
            .users
            .find_by_oidc_identity("https://idp.test", "subj-1")
            .await
            .unwrap()
            .expect("the identity was provisioned");
        assert_eq!(stored.id, minted.user.id);
    }

    #[tokio::test]
    async fn the_admin_claim_grants_and_its_absence_revokes_and_the_returned_user_says_so() {
        let world = world();
        let granted = login(&world, json!({"groups": ["beam-admin"]}), Some("groups")).await;
        assert!(granted.user.is_admin);

        let revoked = login(&world, json!({"groups": []}), Some("groups")).await;
        assert!(
            !revoked.user.is_admin,
            "the user handed back must reflect this login, not the row before it"
        );
        let stored = world
            .users
            .find_by_id(revoked.user.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!stored.is_admin);
    }

    #[tokio::test]
    async fn a_disabled_account_gets_no_session_and_keeps_its_fields() {
        let world = world();
        let first = login(&world, json!({"groups": ["beam-admin"]}), Some("groups")).await;
        world.users.set_disabled(first.user.id, true).await.unwrap();

        let refused = complete_login(
            &identity(json!({"groups": []})),
            &client(),
            &world.users,
            &world.sessions,
            &config(Some("groups")),
        )
        .await;

        assert!(matches!(refused, Err(LoginError::AccountDisabled)));
        let sessions = world
            .sessions
            .list_for_user(&first.user.id.to_string())
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1, "only the session from before disabling");
        assert!(
            world
                .users
                .find_by_id(first.user.id)
                .await
                .unwrap()
                .unwrap()
                .is_admin,
            "a refused login must not demote"
        );
    }

    #[tokio::test]
    async fn the_session_expires_on_the_configured_idle_window() {
        let world = world();
        let minted = login(&world, json!({}), None).await;

        let idle = Duration::from_secs(config(None).idle_ttl_secs());
        world.clock.advance(idle - Duration::from_secs(1));
        assert!(world.sessions.get(&minted.token).await.unwrap().is_some());
        world.clock.advance(Duration::from_secs(2));
        assert!(
            world.sessions.get(&minted.token).await.unwrap().is_none(),
            "an untouched session dies at the idle deadline"
        );
    }

    #[tokio::test]
    async fn a_later_login_refreshes_the_profile_the_idp_now_asserts() {
        // Each IdP-side change on its own: a refresh gated on both changing
        // would miss the far more common case of only one.
        let world = world();
        let first = login(&world, json!({}), None).await;
        let avatar = "https://idp.test/ada.png";

        let steps = [
            (Some("Ada"), Some(avatar)),
            (Some("Ada Lovelace"), Some(avatar)),
            (Some("Ada Lovelace"), None),
        ];
        for (name, picture) in steps {
            let minted = complete_login(
                &OidcIdentity {
                    name: name.map(str::to_owned),
                    picture: picture.map(str::to_owned),
                    ..identity(json!({}))
                },
                &client(),
                &world.users,
                &world.sessions,
                &config(None),
            )
            .await
            .expect("an enabled identity signs in");

            assert_eq!(minted.user.id, first.user.id, "the same account");
            let stored = world
                .users
                .find_by_id(first.user.id)
                .await
                .unwrap()
                .unwrap();
            for (label, user) in [("returned", &minted.user), ("stored", &stored)] {
                assert_eq!(user.display_name, name.unwrap(), "{label}: {name:?}");
                assert_eq!(user.avatar_url.as_deref(), picture, "{label}: {picture:?}");
            }
        }
        assert_eq!(
            world.users.count().await.unwrap(),
            1,
            "a returning identity is never provisioned twice"
        );
    }

    /// The pieces of a world whose user repository fails `failing`, with
    /// `subj-1` already provisioned as a non-admin named `Ada`.
    async fn world_failing(
        failing: UserRepositoryOperation,
    ) -> (FailingUserRepository, InMemorySessionStore, uuid::Uuid) {
        let users = FailingUserRepository::new(InMemoryUserRepository::default(), failing);
        let existing = users
            .inner()
            .create(create_user("https://idp.test", "subj-1"))
            .await
            .unwrap();
        (users, InMemorySessionStore::default(), existing.id)
    }

    #[tokio::test]
    async fn a_user_repository_failure_at_any_step_is_internal_and_mints_no_session() {
        // Each step is reached by an identity that differs from the stored
        // user in exactly what that step writes.
        let cases = [
            (
                UserRepositoryOperation::FindByOidcIdentity,
                json!({}),
                "Ada",
            ),
            (
                UserRepositoryOperation::SetAdmin,
                json!({"groups": ["beam-admin"]}),
                "Ada",
            ),
            (
                UserRepositoryOperation::UpdateOidcProfile,
                json!({}),
                "Ada Lovelace",
            ),
        ];
        for (failing, claims, name) in cases {
            let (users, sessions, existing) = world_failing(failing).await;

            let result = complete_login(
                &OidcIdentity {
                    name: Some(name.to_owned()),
                    ..identity(claims)
                },
                &client(),
                &users,
                &sessions,
                &config(Some("groups")),
            )
            .await;

            match result {
                Err(LoginError::Internal(message)) => assert!(
                    message.contains(&format!("{failing:?}")),
                    "{failing:?}: the cause is carried, got {message:?}"
                ),
                other => panic!("{failing:?}: expected Internal, got {other:?}"),
            }
            assert!(
                sessions
                    .list_for_user(&existing.to_string())
                    .await
                    .unwrap()
                    .is_empty(),
                "{failing:?}: no session for a login that failed"
            );
        }
    }

    #[tokio::test]
    async fn a_failure_provisioning_a_new_user_is_internal_and_mints_no_session() {
        let users = FailingUserRepository::new(
            InMemoryUserRepository::default(),
            UserRepositoryOperation::Create,
        );
        let sessions = InMemorySessionStore::default();

        let result = complete_login(
            &identity(json!({})),
            &client(),
            &users,
            &sessions,
            &config(None),
        )
        .await;

        match result {
            Err(LoginError::Internal(message)) => assert!(
                message.contains("Create"),
                "the cause is carried, got {message:?}"
            ),
            other => panic!("expected Internal, got {other:?}"),
        }
        assert_eq!(users.inner().count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_session_store_failure_is_internal() {
        let users = InMemoryUserRepository::default();
        let sessions = FailingSessionStore::new(
            InMemorySessionStore::default(),
            SessionStoreOperation::Create,
        );

        let result = complete_login(
            &identity(json!({})),
            &client(),
            &users,
            &sessions,
            &config(None),
        )
        .await;

        match result {
            Err(LoginError::Internal(message)) => assert!(
                message.contains("Create"),
                "the cause is carried, got {message:?}"
            ),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn a_name_claim_wins_over_the_email_local_part() {
        assert_eq!(
            derive_display_name(Some("Ada Lovelace"), Some("ada@example.com"), "sub"),
            "Ada Lovelace"
        );
    }

    #[test]
    fn an_absent_name_falls_back_to_the_email_local_part_then_the_subject() {
        assert_eq!(
            derive_display_name(None, Some("ada@example.com"), "sub"),
            "ada"
        );
        assert_eq!(derive_display_name(Some(""), None, "sub"), "user-sub");
        assert_eq!(derive_display_name(None, None, "sub"), "user-sub");
    }

    /// A device store in which another poll of the same flow wins the race:
    /// it ends the flow just after this poll was let through to the IdP.
    #[derive(Debug)]
    struct RacedDeviceFlows(crate::utils::device_auth_store::in_memory::InMemoryDeviceAuthStore);

    #[async_trait::async_trait]
    impl DeviceAuthStore for RacedDeviceFlows {
        async fn create(
            &self,
            auth: &crate::utils::device_auth_store::NewDeviceAuth,
        ) -> Result<(), DeviceAuthError> {
            self.0.create(auth).await
        }
        async fn claim_poll(&self, handle_hash: &str) -> Result<Claim, DeviceAuthError> {
            let claim = self.0.claim_poll(handle_hash).await?;
            self.0.consume(handle_hash).await?;
            Ok(claim)
        }
        async fn bump_interval(&self, handle_hash: &str) -> Result<Option<u32>, DeviceAuthError> {
            self.0.bump_interval(handle_hash).await
        }
        async fn consume(
            &self,
            handle_hash: &str,
        ) -> Result<Option<crate::utils::device_auth_store::DeviceAuth>, DeviceAuthError> {
            self.0.consume(handle_hash).await
        }
    }

    /// Of two polls that both saw the approval, only the one that ends the
    /// flow signs in: the loser is told the flow is over, and nobody is
    /// provisioned or given a session on its account.
    #[tokio::test]
    async fn an_approval_another_poll_already_redeemed_signs_nobody_in() {
        let world = world();
        let flows = RacedDeviceFlows(
            crate::utils::device_auth_store::in_memory::InMemoryDeviceAuthStore::new(
                world.clock.clone(),
            ),
        );
        flows
            .create(&crate::utils::device_auth_store::NewDeviceAuth {
                handle_hash: hash_handle("handle"),
                device_code: "device-code".to_owned(),
                user_code: "BCDF-GHJK".to_owned(),
                verification_uri: "https://idp.test/device".to_owned(),
                verification_uri_complete: None,
                interval_secs: 5,
                expires_in_secs: 600,
            })
            .await
            .unwrap();
        let idp = crate::utils::oidc::fake::FakeOidcClient::default()
            .with_device_script(vec![Ok(DevicePoll::Complete(identity(json!({}))))]);

        let outcome = poll_device_login(
            "handle",
            &client(),
            &idp,
            &flows,
            &world.users,
            &world.sessions,
            &config(None),
        )
        .await;

        assert!(
            matches!(outcome, Err(DevicePollError::Invalid)),
            "{outcome:?}"
        );
        assert_eq!(idp.device_poll_count(), 1, "the IdP was asked once");
        assert_eq!(world.users.count().await.unwrap(), 0);
    }

    #[test]
    fn an_email_with_an_empty_local_part_falls_through_to_the_subject() {
        // `@example.com` splits to an empty local part; naming somebody ""
        // would leave an account nobody can pick out of the admin list.
        assert_eq!(
            derive_display_name(None, Some("@example.com"), "sub"),
            "user-sub"
        );
        assert_eq!(
            derive_display_name(Some(""), Some("@example.com"), "sub"),
            "user-sub"
        );
    }
}
