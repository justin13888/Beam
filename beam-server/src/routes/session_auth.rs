//! The cookie session scheme that both enforces and documents authentication.
//!
//! Auth is the `beam_session` cookie set by the OIDC login flow (see ADR-0003),
//! looked up via `SessionStore::get` and sliding the idle expiry forward on
//! activity. Taking [`SessionAuth`] in a handler signature is what puts the
//! requirement in the OpenAPI document: there is no second place to declare it,
//! so the enforcement and the description cannot disagree.

use beam_auth::utils::session_store::get_and_touch;
use kynos::error::rejection::AuthRejection;
use kynos::prelude::*;
use kynos::security::{
    Authenticates, Authenticator, Scope,
    auth::{Auth, Scoped, Scopes},
};
use uuid::Uuid;

use crate::state::AppState;

/// The cookie the OIDC login flow sets. The only credential Beam issues.
pub const SESSION_COOKIE: &str = "beam_session";

/// The caller's identity, resolved from the `beam_session` cookie.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    /// The signed-in user's id, parsed once by [`SessionAuthenticator`] so no
    /// handler re-parses the session's stored string.
    pub user_id: Uuid,
}

/// The `beam_session` cookie, as both a runtime check and a documented scheme.
///
/// `components.securitySchemes` had no entry at all before this: the previous
/// implementation enforced the cookie in hand-written helpers that the OpenAPI
/// derivation could not see.
#[derive(SecurityScheme)]
#[security(
    api_key(in = "cookie", name = "beam_session"),
    name = "BeamSession",
    credential = AuthenticatedUser,
    scope = BeamScope,
    unavailable,
    description = "Opaque session cookie issued by the OIDC login flow (ADR-0003). \
                   Hashed at rest; never recoverable from the API."
)]
pub struct SessionCookie;

/// Every scope a [`SessionCookie`] operation can demand.
///
/// Beam has exactly one privilege level above "signed in", so this is one
/// scope rather than a scope language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeamScope {
    /// The caller's user row has `is_admin` set.
    Admin,
}

impl Scope for BeamScope {
    fn name(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
        }
    }
}

/// The scope set an admin-gated operation requires.
pub struct Admin;

impl Scopes for Admin {
    type Scope = BeamScope;
    const REQUIRED: &'static [BeamScope] = &[BeamScope::Admin];
    const FORBIDDEN_TYPE: Option<&'static str> = Some(ADMIN_REQUIRED);
}

/// The `type` of the 403 a signed-in non-administrator gets from an admin
/// operation.
///
/// Named because it is the 403 with the clearest next step -- admin is
/// recalculated only at sign-in -- and without a name it is indistinguishable
/// from the same-origin refusals. Declared through [`Admin`]'s
/// `FORBIDDEN_TYPE` and sent through `AuthRejection::forbidden_as`, which is
/// the pair Kynos reads for the document and the wire respectively.
pub const ADMIN_REQUIRED: &str = "https://beam.justinchung.net/reference/errors/#admin-required";

/// Convenience alias for the fourteen `/v1/admin/*` operations.
pub type AdminAuth = Scoped<SessionCookie, Admin>;

/// Convenience alias for an operation that needs any signed-in caller.
pub type SessionAuth = Auth<SessionCookie>;

/// Verifies the session cookie against the store, and the admin scope against
/// the user row.
///
/// Zero-sized: everything it needs arrives as `context`, which is the
/// [`AppState`] the router was built with. That keeps the set of things Beam
/// can authenticate visible in one place instead of spread across handlers.
pub struct SessionAuthenticator;

static SESSION_AUTHENTICATOR: SessionAuthenticator = SessionAuthenticator;

impl Authenticates<SessionCookie> for AppState {
    type Authenticator = SessionAuthenticator;

    fn authenticator(&self) -> &Self::Authenticator {
        &SESSION_AUTHENTICATOR
    }
}

impl Authenticator<SessionCookie, AppState> for SessionAuthenticator {
    /// Resolves the cookie to a user, sliding the idle expiry forward.
    ///
    /// A *session store failure* is a 503: the authenticator could not tell
    /// whether the cookie is valid, which is neither a 401 -- that would tell
    /// a client to discard a session that may be fine -- nor the user the
    /// cookie named. [`SessionCookie`] declares it (`unavailable`), so every
    /// operation it guards documents the 503. The error is logged at `error`
    /// level so an outage is visible in operations.
    ///
    /// A session whose stored user id is not a UUID is a 401, never a
    /// fallback id: every watched-state row is keyed by this value, so
    /// resolving a malformed one to, say, the nil UUID would pool every
    /// affected user's history and progress into one shared, unowned account.
    /// Neither session store can hold one -- both parse the id before writing
    /// -- so this is a corrupted session, logged at `error` level.
    async fn authenticate(
        &self,
        presented: kynos::security::carrier::ApiKey,
        context: &AppState,
    ) -> Result<AuthenticatedUser, AuthRejection> {
        let idle_ttl_secs = context.config.session_idle_days * 24 * 60 * 60;

        let session = get_and_touch(
            context.services.session_store.as_ref(),
            presented.as_str(),
            idle_ttl_secs,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "session store failed while authenticating");
            AuthRejection::unavailable::<SessionCookie>()
        })?
        .ok_or_else(AuthRejection::unauthenticated)?;

        let user_id = Uuid::parse_str(&session.user_id).map_err(|_| {
            tracing::error!(user_id = %session.user_id, "session carries an unparseable user id");
            AuthRejection::unauthenticated()
        })?;

        Ok(AuthenticatedUser { user_id })
    }

    /// Beam's only scope is `admin`, checked against the user row rather than
    /// against anything carried in the credential -- a session outlives a
    /// change to the flag, and the row is the truth.
    ///
    /// A non-administrator is refused as [`ADMIN_REQUIRED`], which [`Admin`]
    /// declares, so the 403 a client receives is the one the document names.
    async fn authorize(
        &self,
        credential: &AuthenticatedUser,
        scopes: &[BeamScope],
        context: &AppState,
    ) -> Result<(), AuthRejection> {
        if !scopes.contains(&BeamScope::Admin) {
            return Ok(());
        }

        let user = context
            .services
            .user_repo
            .find_by_id(credential.user_id)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "user lookup failed while authorizing");
                AuthRejection::unavailable::<SessionCookie>()
            })?
            .ok_or_else(AuthRejection::unauthenticated)?;

        if user.is_admin {
            Ok(())
        } else {
            Err(AuthRejection::forbidden_as(ADMIN_REQUIRED))
        }
    }
}
