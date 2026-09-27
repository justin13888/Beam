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
use crate::utils::models::{CreateUser, User};
use crate::utils::oidc::OidcIdentity;
use crate::utils::oidc_config::OidcRuntimeConfig;
use crate::utils::repository::UserRepository;
use crate::utils::session_store::{SessionData, SessionStore};

/// Who is signing in from where, as the session record describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientContext {
    /// The `User-Agent`, hashed into the session's device fingerprint.
    pub user_agent: Option<String>,
    /// The client address, as the deployment's proxy reports it.
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

/// The session's device fingerprint: the SHA-256 of the `User-Agent`.
#[must_use]
pub fn device_hash(user_agent: Option<&str>) -> String {
    // Session rows are compared by equality against this string, so it is
    // produced by the crate's one encoder rather than a local `{:x}`.
    crate::utils::hex::encode_lower(&Sha256::digest(user_agent.unwrap_or("").as_bytes()))
}

/// The client address, as the deployment's proxy reports it: the first entry
/// of `X-Forwarded-For`, else `X-Real-IP`, else `unknown`.
#[must_use]
pub fn client_ip(forwarded_for: Option<&str>, real_ip: Option<&str>) -> String {
    if let Some(first) = forwarded_for.and_then(|value| value.split(',').next()) {
        let first = first.trim();
        if !first.is_empty() {
            return first.to_owned();
        }
    }
    real_ip.map_or_else(|| "unknown".to_owned(), str::to_owned)
}

/// Picks a display name when the IdP doesn't release a `name` claim: the
/// local part of the email if one is available, else a subject-derived
/// placeholder. Real IdPs (including Dex) send `name`, so this is a rare
/// fallback, not the common case.
#[must_use]
pub fn derive_display_name(name: Option<&str>, email: Option<&str>, subject: &str) -> String {
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
    use crate::utils::repository::in_memory::InMemoryUserRepository;
    use crate::utils::session_store::in_memory::InMemorySessionStore;

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

    #[test]
    fn the_forwarded_chain_yields_its_first_entry() {
        assert_eq!(
            client_ip(Some("203.0.113.7, 70.41.3.18"), Some("10.0.0.1")),
            "203.0.113.7"
        );
    }

    #[test]
    fn the_real_ip_header_is_the_fallback() {
        assert_eq!(client_ip(None, Some("10.0.0.1")), "10.0.0.1");
        assert_eq!(client_ip(Some(" "), Some("10.0.0.1")), "10.0.0.1");
        assert_eq!(client_ip(None, None), "unknown");
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
}
