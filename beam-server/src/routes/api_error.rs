//! Shared v1 REST conventions: one machine-readable error shape, and the
//! cookie session scheme that both enforces and documents authentication.
//!
//! Before the Kynos migration this module held one of *four* error enums
//! rendering *three* different body shapes (issue #123). Kynos renders every
//! failure -- its own extractor rejections included -- as an RFC 9457 problem
//! document and offers no hook to render a second envelope, so the four
//! collapse into the family below and a client gets one type it can
//! deserialise for any status.
//!
//! Auth is the `beam_session` cookie set by the OIDC login flow (see ADR-0003),
//! looked up via `SessionStore::get` and sliding the idle expiry forward on
//! activity. Taking [`SessionAuth`] in a handler signature is what puts the
//! requirement in the OpenAPI document: there is no second place to declare it,
//! so the enforcement and the description cannot disagree.

use std::sync::Arc;

use beam_auth::utils::session_store::get_and_touch;
use kynos::error::rejection::AuthRejection;
use kynos::prelude::*;
use kynos::security::{
    Authenticates, Authenticator,
    auth::{Auth, Scoped, Scopes},
};

use crate::state::AppState;

/// The cookie the OIDC login flow sets. The only credential Beam issues.
pub const SESSION_COOKIE: &str = "beam_session";

/// The base every `type` URI shares.
///
/// Published rather than internal: the whole point of a stable identifier is
/// that a client branches on it, so it belongs to the contract.
pub const ERROR_BASE: &str = "https://beam.justinchung.net/reference/errors/";

// ── Errors ───────────────────────────────────────────────────────────────────
//
// Not one enum but a small family, and the split is deliberate. Kynos derives
// an operation's `responses` from its return type, so a single shared error
// union would make every endpoint advertise every status Beam can produce --
// `GET /v1/genres` would claim a 416 it cannot reach, and a generated client
// turns that into dead retry logic. Each operation names the narrowest type
// that covers what it can actually answer with.
//
// The `type` URI is written out per variant rather than derived from the
// variant name, so the same failure carries the same stable code no matter
// which enum it reaches a client through. That code is what issue #123 asked
// for; the message stays human-facing and non-contractual.
//
// 401 and 403 are deliberately absent from all of these: they arrive from
// `SessionAuth` and `AdminAuth` respectively, which is what makes taking the
// extractor and documenting the requirement one act. The exception is
// `DeliveryError::Forbidden`, which is an authorization decision about a *file*
// rather than about the caller.

/// A read whose only failure is infrastructural.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum InternalError {
    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// A read of one resource that may not exist.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum LookupError {
    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/not-found",
        title = "Not found"
    )]
    NotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// A write against a resource that may not exist, with a body that may not
/// validate.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum MutationError {
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/bad-request",
        title = "Bad request"
    )]
    BadRequest(String),

    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/not-found",
        title = "Not found"
    )]
    NotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// Byte-range file delivery.
///
/// Three of the statuses these operations answer with are not here, and each
/// absence is deliberate. The 401 and 403 arrive from `SessionAuth` and
/// `EnforceSameOrigin`. The 416 is declared on [`MediaDelivery`], because
/// `Served::deliver` resolves an unsatisfiable range into a problem document
/// and hands it back as an `Ok` delivery -- the handler never sees an error to
/// convert. Variants for all three existed here and nothing constructed them.
///
/// Issue #123 recorded that the Salvo implementation reported a *forbidden*
/// file as 401 on `/v1/files/{id}/stream` and `/download`. That collapse is
/// gone, and so is the case it described: a file resolving outside its library
/// root is refused at registration (`services::library`), not at delivery, so
/// there is no 403 for this type to carry.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum DeliveryError {
    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/not-found",
        title = "Not found"
    )]
    NotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/internal",
        title = "Internal server error"
    )]
    Internal(String),
}

impl From<InternalError> for LookupError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
    }
}

impl From<InternalError> for MutationError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
    }
}

impl From<LookupError> for MutationError {
    fn from(e: LookupError) -> Self {
        match e {
            LookupError::NotFound(m) => Self::NotFound(m),
            LookupError::Internal(m) => Self::Internal(m),
        }
    }
}

// ── Authentication ───────────────────────────────────────────────────────────

/// The caller's identity, resolved from the `beam_session` cookie.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub user_id: String,
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
    description = "Opaque session cookie issued by the OIDC login flow (ADR-0003). \
                   Hashed at rest; never recoverable from the API."
)]
pub struct SessionCookie;

/// The scope an admin-gated operation requires.
///
/// Beam has exactly one privilege level above "signed in", so this is one
/// scope rather than a scope language.
pub struct Admin;

impl Scopes for Admin {
    const SCOPES: &'static [&'static str] = &["admin"];
}

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
    /// Note the one behaviour change the migration forces: a *session store
    /// failure* used to surface as 500 and now surfaces as 401.
    /// `AuthRejection` has only `Unauthenticated` and `Forbidden`, so a
    /// verifier has no way to say "I could not tell". The error is logged at
    /// `error` level so an outage is still visible in operations; see the
    /// upstream note in `docs/architecture/kynos-migration-readiness.md`.
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
            AuthRejection::unauthenticated()
        })?
        .ok_or_else(AuthRejection::unauthenticated)?;

        Ok(AuthenticatedUser {
            user_id: session.user_id,
        })
    }

    /// Beam's only scope is `admin`, checked against the user row rather than
    /// against anything carried in the credential -- a session outlives a
    /// change to the flag, and the row is the truth.
    async fn authorize(
        &self,
        credential: &AuthenticatedUser,
        scopes: &'static [&'static str],
        context: &AppState,
    ) -> Result<(), AuthRejection> {
        if !scopes.contains(&"admin") {
            return Ok(());
        }

        let user_id = uuid::Uuid::parse_str(&credential.user_id).map_err(|_| {
            tracing::error!(user_id = %credential.user_id, "session carries an unparseable user id");
            AuthRejection::unauthenticated()
        })?;

        let user = context
            .services
            .user_repo
            .find_by_id(user_id)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "user lookup failed while authorizing");
                AuthRejection::unauthenticated()
            })?
            .ok_or_else(AuthRejection::unauthenticated)?;

        if user.is_admin {
            Ok(())
        } else {
            Err(AuthRejection::Forbidden)
        }
    }
}

// ── Dependency injection ─────────────────────────────────────────────────────

/// The auth dependencies the OIDC handlers reach for individually.
///
/// Written out rather than derived: `AppState` keeps its fields behind
/// `AppStateInner`, and `#[derive(Provider)]` needs public fields on the
/// context itself. Each one mirrors an `affix_state::inject` hoop the Salvo
/// router used to mount, with the difference that asking for something absent
/// is now a compile error rather than a 500.
macro_rules! provides_service {
    ($($ty:ty => $field:ident),+ $(,)?) => {
        $(
            impl kynos::di::Provides<$ty> for AppState {
                fn provide(&self) -> $ty {
                    self.services.$field.clone()
                }
            }
        )+
    };
}

provides_service! {
    Arc<dyn beam_auth::utils::repository::UserRepository> => user_repo,
    Arc<dyn beam_auth::utils::session_store::SessionStore> => session_store,
    Arc<dyn beam_auth::utils::oidc::OidcClient> => oidc_client,
    Arc<dyn beam_auth::utils::pending_auth_store::PendingAuthStore> => pending_auth_store,
}

impl kynos::di::Provides<beam_auth::utils::oidc_config::OidcRuntimeConfig> for AppState {
    fn provide(&self) -> beam_auth::utils::oidc_config::OidcRuntimeConfig {
        self.services.oidc_config.clone()
    }
}
