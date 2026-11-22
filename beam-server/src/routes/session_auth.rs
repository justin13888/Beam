//! The cookie session scheme that both enforces and documents authentication.
//!
//! Auth is the `beam_session` cookie set by the OIDC login flow (see ADR-0003),
//! looked up via `SessionStore::get` and sliding the idle expiry forward on
//! activity. Taking [`SessionAuth`] in a handler signature is what puts the
//! requirement in the OpenAPI document: there is no second place to declare it,
//! so the enforcement and the description cannot disagree.
//!
//! The four delivery operations also accept a [`MediaGrant`] (ADR-0022), a
//! signed URL for one file that a cast receiver or external player can fetch
//! without the cookie. They take [`DeliveryAuth`]; nothing else does.

use beam_auth::utils::session_store::get_and_touch;
use kynos::error::rejection::AuthRejection;
use kynos::prelude::*;
use kynos::security::{
    Authenticates, Authenticator, Scope, Unscoped,
    auth::{Auth, Scoped, Scopes},
    requirement::{AnyOf, Either2},
};
use uuid::Uuid;

use crate::metrics::session_store_failed;
use crate::state::AppState;

/// The cookie the OIDC login flow sets. The only session credential Beam
/// issues; a [`MediaGrant`] is a capability for one file, not a session.
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

/// What a media grant authenticates: one file, on behalf of one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantedFile {
    /// The user the grant was issued to, whose access policy still applies.
    pub user_id: Uuid,
    /// The one file the grant may fetch.
    pub file_id: Uuid,
}

/// A signed, file-scoped media grant in the `grant` query parameter
/// (ADR-0022), for a client that holds no session cookie -- a cast receiver
/// or an external player handed a URL.
///
/// Accepted by the four delivery operations alone, through [`DeliveryAuth`];
/// every other operation names only [`SessionCookie`], so a grant opens no
/// part of the API. Verifying one reads no store, so it declares no 503.
#[derive(SecurityScheme)]
#[security(
    api_key(in = "query", name = "grant"),
    name = "MediaGrant",
    credential = GrantedFile,
    description = "Short-lived signature over one file and one user, issued by \
                   `POST /v1/files/{file_id}/grant` (ADR-0022). Fetches that \
                   file's bytes and nothing else, under the user's access policy."
)]
pub struct MediaGrant;

/// The four delivery operations' requirement: the session cookie, or a media
/// grant for the file the path names.
///
/// The cookie is tried first, so a browser that has both is the session it
/// signed in as. Which file a grant names is checked by the operation, which
/// alone knows the path.
pub type DeliveryAuth = Auth<AnyOf<(SessionCookie, MediaGrant)>>;

/// The caller of a delivery operation, whichever scheme admitted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryCaller {
    /// A signed-in session, which may fetch any file its policy shows.
    Session(Uuid),
    /// A media grant, which may fetch only the file it names.
    Grant(GrantedFile),
}

impl DeliveryCaller {
    /// Who the caller acts for: whose access policy applies.
    pub fn user_id(self) -> Uuid {
        match self {
            Self::Session(user_id) => user_id,
            Self::Grant(GrantedFile { user_id, .. }) => user_id,
        }
    }

    /// Whether the caller may ask for `file_id` at all, before any policy: a
    /// grant names one file and is no credential for another.
    pub fn may_request(self, file_id: Uuid) -> bool {
        match self {
            Self::Session(_) => true,
            Self::Grant(granted) => granted.file_id == file_id,
        }
    }
}

impl From<&DeliveryAuth> for DeliveryCaller {
    fn from(auth: &DeliveryAuth) -> Self {
        match &auth.0 {
            Either2::First(session) => Self::Session(session.user_id),
            Either2::Second(granted) => Self::Grant(*granted),
        }
    }
}

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
            session_store_failed();
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

/// Verifies a media grant's signature and expiry against the process's key
/// and clock.
///
/// Zero-sized for the reason [`SessionAuthenticator`] is.
pub struct MediaGrantAuthenticator;

static MEDIA_GRANT_AUTHENTICATOR: MediaGrantAuthenticator = MediaGrantAuthenticator;

impl Authenticates<MediaGrant> for AppState {
    type Authenticator = MediaGrantAuthenticator;

    fn authenticator(&self) -> &Self::Authenticator {
        &MEDIA_GRANT_AUTHENTICATOR
    }
}

impl Authenticator<MediaGrant, AppState> for MediaGrantAuthenticator {
    /// A grant this process signed and whose expiry has not passed on the
    /// injected clock; anything else is a 401, whichever check refused it.
    ///
    /// The reason is logged at `debug`, never the grant: a grant read from a
    /// log can be presented again until it expires.
    async fn authenticate(
        &self,
        presented: kynos::security::carrier::ApiKey,
        context: &AppState,
    ) -> Result<GrantedFile, AuthRejection> {
        let claims = context
            .media_grants()
            .verify(presented.as_str(), context.clock().now())
            .map_err(|refused| {
                tracing::debug!(%refused, "media grant refused");
                AuthRejection::unauthenticated()
            })?;

        Ok(GrantedFile {
            user_id: claims.user_id,
            file_id: claims.file_id,
        })
    }

    /// A grant carries no scope, and no operation demands one of it.
    async fn authorize(
        &self,
        _credential: &GrantedFile,
        scopes: &[Unscoped],
        _context: &AppState,
    ) -> Result<(), AuthRejection> {
        match scopes.first() {
            None => Ok(()),
            Some(never) => match *never {},
        }
    }
}
