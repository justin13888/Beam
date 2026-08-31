# Beam Auth

A library crate providing OIDC-backed authentication and session management for `beam-server`.
There is no standalone `beam-auth` binary or container image -- `beam-server` links this crate
and mounts its routes in-process. See [`docs/architecture/components.md`](../docs/architecture/components.md)
and [`docs/architecture/security.md`](../docs/architecture/security.md) for the current
architecture, and run/test `beam-server` per its own README for local development.

## Architecture

`beam-auth` is a library crate, and it has **no web-framework dependency**. Its HTTP adapter moved
to `beam-server/src/routes/auth.rs` with the Kynos migration, because
[ADR-0010](../docs/architecture/decisions/ADR-0010-openapi-3-2-kynos.md) requires the framework to
stay in `beam-server`. What is left here is transport-independent by construction rather than by
convention.

Feature flags:

- **`utils`** -- Core domain types and trait abstractions: `UserRepository`, `SessionStore`,
  `PendingAuthStore`, `OidcClient`, plus their sea-orm-backed (`Sql*`) and `InMemory*` fake
  implementations.
- **`oidc`** (the default) -- real discovery and token exchange via `openidconnect` + `reqwest`.
- **`test-utils`** -- `FakeOidcClient` and the in-memory doubles, with no HTTP client.

`beam-server` links `beam-auth` and calls it in process; there is no network call between them. The
route table below is served by `beam-server`.

## Auth flow

Beam uses the OIDC Authorization Code + PKCE flow with the browser as a pure BFF (backend-for-
frontend) client -- the SPA never sees an ID token or access token, only an opaque session cookie.
See [ADR-0003](../docs/architecture/decisions/ADR-0003-oidc-bff-auth.md) for the full rationale.

| Method | Path | Description |
|---|---|---|
| `GET` | `/v1/auth/login` | Begins the Authorization Code + PKCE flow: redirects to the IdP. Accepts a `redirect` query param (sanitized to a same-origin-relative path) for where to send the browser after a successful login. |
| `GET` | `/v1/auth/callback` | IdP redirects back here with `code`/`state`. Verifies state/nonce/PKCE, exchanges the code, JIT-provisions the user, mints a session, and redirects into the web app. |
| `GET` | `/me` | Returns the current session's user (id, email, display name, avatar, admin status). |
| `POST` | `/logout` | Revokes the current session. |
| `POST` | `/logout-all` | Revokes every session for the current user. |
| `GET` | `/sessions` | Lists the current user's active sessions (id, device hash, IP, timestamps -- never the raw session token). |
| `DELETE` | `/sessions/{id}` | Revokes one specific session by id. |

### Session strategy

- **Session cookie**: a random, opaque, URL-safe token (`beam_session`), set `HttpOnly`,
  `SameSite=Lax`, and `Secure` when the deployment is HTTPS. Only its SHA-256 hash is stored in
  Postgres -- the raw token can't be recovered from a database read.
  See [ADR-0005](../docs/architecture/decisions/ADR-0005-sessions-in-postgres.md).
- **Expiry**: a sliding idle timeout (`BEAM_SESSION_IDLE_DAYS`, default 14) that resets on
  activity, capped by an absolute lifetime (`BEAM_SESSION_MAX_DAYS`, default 60) regardless of
  activity.
- **Pending-auth state**: the in-flight state/nonce/PKCE verifier for a login attempt is stored
  server-side (`PendingAuthStore`, single-use, short TTL) and bound to a `beam_oidc_state` cookie
  -- no secrets round-trip through the client beyond the opaque `state` value.
- **Admin status**: derived solely from a configured ID-token claim (`BEAM_OIDC_ADMIN_CLAIM`,
  matched per `BEAM_OIDC_ADMIN_VALUE`), recomputed on every login — granting **and** revoking — not
  stored as a persistent grant. The IdP is the single authority (issue #85); with no admin claim
  configured, nobody is admin. Claim evaluation is a pure function in `utils/admin_claim.rs`.

## Testing

Zero-dependency: `InMemoryUserRepository`, `InMemorySessionStore`, `InMemoryPendingAuthStore`, and
`FakeOidcClient` (a programmable fake IdP that verifies the state/nonce/PKCE round-trip the same
way a real one would, including rejecting a mismatched verifier/nonce) let the full login/
callback/me/logout/sessions flow be exercised with no real IdP, network, or database. The
HTTP-level round-trip runs in `beam-server` through `kynos::test::TestClient`, over these same
fakes. See the `test-utils` feature.
