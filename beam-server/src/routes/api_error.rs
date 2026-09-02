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
/// that a client branches on it, so it belongs to the contract. The trailing
/// `#` is load-bearing -- each code is a fragment on the one error-reference
/// page, so `type` dereferences to the section describing it. Before that it
/// pointed at `/reference/errors/<code>`, a path that has never existed, so
/// every identifier Beam published resolved to a 404.
///
/// It cannot appear in the attributes below: kynos parses `#[problem(type =
/// ...)]` and `#[problem(base = ...)]` as string literals, not paths. The
/// prefix is therefore written out per variant, and `contract_tests` asserts
/// that every literal starts with this constant.
pub const ERROR_BASE: &str = "https://beam.justinchung.net/reference/errors/#";

// ── Errors ───────────────────────────────────────────────────────────────────
//
// Not one enum but a family, one per distinct set of reachable failures. Kynos
// derives an operation's `responses` from its return type, so a shared union
// makes every endpoint advertise every status any of them can produce -- `GET
// /v1/genres` would claim a 416 it cannot reach, and a generated client turns
// that into dead retry logic.
//
// Sharpening the codes made the split load-bearing rather than merely tidy.
// Kynos carries one response per status and titles it from the *first* variant
// declaring that status, so a `MutationError` holding both `MediaNotFound` and
// `LibraryNotFound` would document `deleteLibrary`'s 404 as "Media not found".
// One enum per operation shape is what keeps each description true.
//
// The `type` URI is written out per variant rather than derived from the
// variant name. Kynos can compose one from `#[problem(base = ...)]` plus the
// variant's name, but then a rename silently changes the published contract
// with no string to diff and nothing for a reviewer to catch -- and the same
// code is deliberately emitted by several enums (`media-not-found` by three,
// `internal` by ten), which an implicit convention would hide.
//
// 401 and 403 are almost always absent here: they arrive from `SessionAuth`
// and `AdminAuth`, which is what makes taking the extractor and documenting the
// requirement one act. Those carry no `type` of their own -- kynos renders
// every rejection as `about:blank`, which is the right reading of RFC 9457
// where the status is the whole story, and a gap only for the admin 403
// (getkono/kynos#105, and the note on `SessionAuthenticator::authorize`).

/// A read whose only failure is infrastructural.
///
/// `GET /v1/genres`, `GET /v1/libraries`, `GET /v1/continue-watching`,
/// `GET /v1/history`, the admin read endpoints, and the three session
/// operations that only ever fail on the store.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum InternalError {
    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `GET /v1/media/{id}`.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum MediaLookupError {
    /// The `{id}` in the path is not a UUID.
    ///
    /// `GET /v1/media/{id}` used to answer this 404, because
    /// `get_media_metadata` returns `Option` and the parse failure was folded
    /// into the miss -- while `/sources` and the refresh route, which share the
    /// same path parameter, answered 400. Three routes, one condition, two
    /// answers. The parse now happens in the handler, before the lookup that
    /// cannot report it.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#invalid-media-id",
        title = "Invalid media id"
    )]
    InvalidMediaId(String),

    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#media-not-found",
        title = "Media not found"
    )]
    MediaNotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `GET /v1/media/{id}/sources`.
///
/// Two 400s share one declared response, and `InvalidMediaId` is written first
/// deliberately: kynos carries one response per status and titles it from the
/// first variant declaring that status, so declaration order is part of the
/// published document. Narrowing `type` per branch is what a `oneOf` of
/// const-constrained problems would express, which kynos cannot emit --
/// getkono/kynos#103. Until it can, the codes below are correct on the wire
/// and absent from the contract, and `taxonomy_tests` is what pins them.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum MediaSourcesError {
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#invalid-media-id",
        title = "Invalid media id"
    )]
    InvalidMediaId(String),

    /// A show has no files of its own; the caller wants an episode id.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#sources-not-available-for-show",
        title = "Sources are not available at the show level"
    )]
    SourcesNotAvailableForShow(String),

    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#media-not-found",
        title = "Media not found"
    )]
    MediaNotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `POST /v1/admin/media/{id}/refresh`.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum MediaRefreshError {
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#invalid-media-id",
        title = "Invalid media id"
    )]
    InvalidMediaId(String),

    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#media-not-found",
        title = "Media not found"
    )]
    MediaNotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// An operation naming one library by id: `GET /v1/libraries/{id}`, its
/// `/files` subresource, and `DELETE /v1/admin/libraries/{id}`.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum LibraryRefError {
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#invalid-library-id",
        title = "Invalid library id"
    )]
    InvalidLibraryId(String),

    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#library-not-found",
        title = "Library not found"
    )]
    LibraryNotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `POST /v1/admin/libraries`. No 404: nothing is looked up by id.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum LibraryCreateError {
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#library-path-not-found",
        title = "Library path not found"
    )]
    PathNotFound(String),

    /// The root escapes the directory Beam is allowed to serve from.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#library-path-outside-root",
        title = "Library path is outside the permitted root"
    )]
    PathOutsideRoot(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `POST /v1/admin/libraries/{id}/scan`.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum LibraryScanError {
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#invalid-library-id",
        title = "Invalid library id"
    )]
    InvalidLibraryId(String),

    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#library-path-not-found",
        title = "Library path not found"
    )]
    PathNotFound(String),

    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#library-not-found",
        title = "Library not found"
    )]
    LibraryNotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `PATCH /v1/admin/users/{id}`.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum AdminUserError {
    /// An administrator cannot lock themselves out.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#cannot-disable-self",
        title = "An administrator cannot disable their own account"
    )]
    CannotDisableSelf(String),

    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#user-not-found",
        title = "User not found"
    )]
    UserNotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// `PUT /v1/files/{file_id}/progress`.
///
/// The path parameter is typed `Uuid`, so a malformed id is Kynos's
/// `PathRejection` rather than anything this type carries.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ProgressError {
    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#file-not-found",
        title = "File not found"
    )]
    FileNotFound(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
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
///
/// `FileNotFound` is written before `SourceFileMissing` because both are 404s
/// sharing one declared response, and "File not found" is the description that
/// covers the pair. They are separate codes because the operator response
/// differs: one means the catalogue never had the file, the other that the
/// catalogue and the disk have diverged.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum DeliveryError {
    /// The `{file_id}` in the path is not a UUID.
    ///
    /// `stream.rs` swallowed this into a 500 by catching every `LibraryError`
    /// from the lookup as an internal fault (issue #123). The path parameter is
    /// typed `String` rather than `Uuid` deliberately -- a `Uuid` would let
    /// Kynos answer the 400 first, but its problem document carries no type
    /// Beam can name.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#invalid-file-id",
        title = "Invalid file id"
    )]
    InvalidFileId(String),

    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#file-not-found",
        title = "File not found"
    )]
    FileNotFound(String),

    /// The catalogue has the file; the path it names is not on disk.
    ///
    /// Its own code because it is the one 404 here an operator can act on: a
    /// bind mount is missing, the library moved, or the file was deleted
    /// outside Beam.
    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#source-file-missing",
        title = "Source file missing from disk"
    )]
    SourceFileMissing(String),

    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

/// What `/v1/artwork/{kind}/{id}/{variant}` can fail with.
///
/// Separate from [`DeliveryError`] even though both deliver bytes through
/// `RuntimeDelivery`, because the conditions do not mean the same thing to a
/// client. `file-not-found` and `source-file-missing` are about an indexed file
/// a viewer asked to play; a title with no poster is neither -- it is the
/// ordinary case a client renders as a placeholder. Reusing the delivery codes
/// would tell a viewer to ask an administrator to rescan the library because a
/// show has no backdrop.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ArtworkError {
    /// The `{id}` in the path is not a UUID.
    ///
    /// The same code the detail route, `/sources` and the admin refresh answer
    /// with, because it is the same condition on the same identifier. This
    /// route used to fold the failed parse into the 404 -- a malformed id and
    /// an unknown one are different things for a caller to fix, and the other
    /// three routes over the same id already told them apart.
    #[error("{0}")]
    #[problem(
        status = 400,
        type = "https://beam.justinchung.net/reference/errors/#invalid-media-id",
        title = "Invalid media id"
    )]
    InvalidId(String),

    /// No image for this title: the row carries no stored URL, the variant does
    /// not apply to that kind, or the provider no longer serves it.
    ///
    /// One code for all three because a client acts identically on each -- it
    /// draws the placeholder. Splitting them would publish a distinction no
    /// caller can use.
    #[error("{0}")]
    #[problem(
        status = 404,
        type = "https://beam.justinchung.net/reference/errors/#artwork-not-found",
        title = "Artwork not found"
    )]
    NotFound(String),

    /// The provider answered unusably: a non-image content type, a body over
    /// the ceiling, an unhappy upstream status, or nothing at all.
    ///
    /// A 502 rather than a 500 because the fault is not Beam's, and `internal`
    /// is documented as meaning that it is. A provider CDN timing out reported
    /// as a Beam 500 is the mislabelling issue #123 was opened on. A stored
    /// URL the fetcher *refuses* is deliberately not here: no request was ever
    /// made, so nothing upstream can have failed -- see [`Self::Internal`].
    #[error("{0}")]
    #[problem(
        status = 502,
        type = "https://beam.justinchung.net/reference/errors/#artwork-upstream-failed",
        title = "Artwork provider failed"
    )]
    UpstreamFailed(String),

    /// Beam's own fault: the title lookup failed, the cached file could not be
    /// read back, or enrichment stored a URL Beam itself will not fetch (not
    /// `https`, or unparseable) -- bad data Beam wrote, not a provider that
    /// misbehaved.
    #[error("{0}")]
    #[problem(
        status = 500,
        type = "https://beam.justinchung.net/reference/errors/#internal",
        title = "Internal server error"
    )]
    Internal(String),
}

// Widening conversions. Each is total and each maps like to like; there is no
// arm here that sends a not-found to a 500 to satisfy the compiler.

impl From<InternalError> for MediaLookupError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
    }
}

impl From<InternalError> for MediaSourcesError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
    }
}

impl From<InternalError> for MediaRefreshError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
    }
}

impl From<InternalError> for LibraryRefError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
    }
}

impl From<InternalError> for LibraryCreateError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
    }
}

impl From<InternalError> for LibraryScanError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
    }
}

impl From<InternalError> for AdminUserError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
    }
}

impl From<InternalError> for ProgressError {
    fn from(e: InternalError) -> Self {
        let InternalError::Internal(m) = e;
        Self::Internal(m)
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
    ///
    /// The `Forbidden` returned below is the one condition on the whole
    /// surface Beam would like to name and cannot. `AuthRejection` carries no
    /// type, so this 403 reaches a client as `about:blank` and is
    /// indistinguishable from the same-origin refusals -- and it is the 403
    /// with the clearest next step for a user, since admin is recalculated
    /// only at sign-in. Filed as getkono/kynos#105; there is no local fix that
    /// would not be a second error path around the extractor that declares it.
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
