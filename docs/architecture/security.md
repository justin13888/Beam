# Security Architecture

`beam-server` authenticates users via OIDC in the backend-for-frontend (BFF) pattern: the *server*
holds the OIDC client credentials and performs the Authorization Code + PKCE exchange; the browser
never sees an ID, access, or refresh token. The browser holds exactly one credential — the
`beam_session` httpOnly, `SameSite=Lax` cookie — for everything, including video playback. See
[ADR-0003](decisions/ADR-0003-oidc-bff-auth.md) for why OIDC/BFF and
[ADR-0005](decisions/ADR-0005-sessions-in-postgres.md) for why sessions live in Postgres.

## OIDC / BFF flow

1. **Login.** `GET /v1/auth/login` generates a PKCE verifier/challenge, `state`, and nonce, stores
   them in the single-use `pending_auths` table (see `data-model.md`), and redirects the browser to
   the configured issuer's authorization endpoint.
2. **IdP authentication.** The user authenticates at the IdP (dev: the opt-in Dex behind the
   `dev-idp` compose profile, started by `mise run dev:up`; prod: any OIDC-compliant IdP —
   Keycloak, Authentik, Authelia, a hosted provider). The IdP redirects back with an authorization
   code.
3. **Callback.** `GET /v1/auth/callback` consumes the `pending_auths` row atomically (a `state`
   value is exchangeable at most once), exchanges code + PKCE verifier for tokens server-to-server
   (via the `openidconnect` crate), and validates the ID token's issuer, audience, signature, nonce,
   and expiry.
4. **JIT provisioning.** A `users` row is looked up (or created) by `(oidc_issuer, oidc_subject)` —
   there is no separate registration step. `is_admin` is recomputed here from the allowlist (below).
5. **Session creation.** An opaque, high-entropy token is generated; only its SHA-256 hash is stored
   in `sessions`; the cookie is set `HttpOnly`, `SameSite=Lax`, `Secure` per the resolution rules
   below. The IdP tokens are discarded — `beam-server` never talks to the IdP again mid-session.
6. **Subsequent requests.** Middleware hashes the presented cookie value, looks it up by
   `token_hash`, checks expiry, slides the idle expiry forward, and attaches the resolved user to
   the request. No token ever appears in a URL, query string, or `<video>` `src`.
7. **Logout.** `POST /v1/logout` deletes the session row server-side (so a stolen cookie value is
   useless after logout) and clears the cookie; `POST /v1/logout-all` and
   `DELETE /v1/sessions/{id}` revoke other sessions.

## Session model

- **Hashed at rest:** only `SHA-256(token)` is stored; a database dump does not expose usable
  sessions.
- **Two-tier expiry:** `idle_expires_at` slides forward on activity (`BEAM_SESSION_IDLE_DAYS`,
  default 14) up to the hard `absolute_expires_at` ceiling (`BEAM_SESSION_MAX_DAYS`, default 60),
  which never extends.
- **Server-side revocation:** revoking a session is a row delete — no JWT expiry windows or
  denylists. The cookie carries only an opaque token, never a signed claim bundle, sidestepping the
  JWT pitfall class (algorithm confusion, secret rotation, forgery on key leak) entirely.

## Cookie `Secure` resolution

The cookie's `Secure` flag defaults to whatever `BEAM_SERVER_URL`'s scheme implies (`https://` →
secure). If other configuration implies an HTTPS deployment (`BEAM_WEB_URL` or
`BEAM_EXTRA_ALLOWED_ORIGINS` contain `https://`) while cookies resolve insecure and no explicit
override is set, **startup fails with an error** rather than silently issuing an insecure session
cookie on an HTTPS deployment. Setting `BEAM_COOKIE_SECURE=false` explicitly is the escape hatch
for topologies where the heuristic is wrong (e.g. TLS terminated in front of a plain-HTTP origin);
it downgrades the error to a logged warning.

## CSRF model

Two deliberate layers protect cookie-authenticated state changes:

- `SameSite=Lax` is the primary defense — the browser does not attach the cookie to cross-site
  POST/PUT/PATCH/DELETE requests at all.
- The `/v1` router additionally enforces same-origin on every unsafe method: a request presenting
  an `Origin` (or `Referer`, as fallback) that doesn't match `BEAM_WEB_URL`, `BEAM_SERVER_URL`, or
  `BEAM_EXTRA_ALLOWED_ORIGINS` is rejected with `403` before reaching a handler. Requests with
  neither header pass — legitimate non-browser clients send neither and never carry the cookie,
  while SameSite already stops the browser-based attack.

## Admin gating

`is_admin` is derived **solely** from a claim the IdP asserts in the verified ID token — the IdP is
the single, auditable authority (issue #85). An env-var email allowlist was deliberately removed:
trusting the IdP alone keeps the admin attack surface minimal to audit, with no server-side
side-channel grant to reconcile. `BEAM_OIDC_ADMIN_CLAIM` names the claim (e.g. `groups`) and
`BEAM_OIDC_ADMIN_VALUE` the expected value (boolean `true` when unset; otherwise a string equality
or array-contains match — see `../operations/configuration.md`).

Admin is recomputed and written to the `users` row at **every** login, so it both grants and
revokes: removing the claim at the IdP demotes the user at their next login, and with
`BEAM_OIDC_ADMIN_CLAIM` unset nobody is admin at all. There is intentionally no manual admin toggle —
a later admin UI displays the flag read-only. All admin mutations (library management, re-enrichment,
log viewing, the admin event stream) route through one shared admin-gating check.

## Read-only media filesystem as a security boundary

The media library is mounted read-only, as a hard architectural invariant: even a worst-case
path-traversal bug in file-serving code cannot modify or delete library content. The server's own
writable storage (`BEAM_DATA_DIR`) is a separate location; nothing about playback or indexing writes
into the library tree. The API reinforces the boundary at another layer: clients only ever see
opaque IDs (`fileId`, `movieId`), never filesystem paths — every resource reference is resolved
server-side against the catalog.

## Operational hardening

- Startup logs redact secrets: `ServerConfig` has a hand-written `Debug` impl that redacts
  credential fields, and destructures all fields so adding a config field without classifying it is
  a compile error.
- Request handlers return `500` instead of panicking when expected injected state is missing —
  a wiring bug degrades one request, not the process.
- Rate limiting: in-process token buckets (see `beam-server/src/routes/rate_limit.rs`) guard the auth
  endpoints (`/v1/auth/login`, `/v1/auth/callback`) and the browse/search endpoint (`GET /v1/media`),
  keyed per client IP, returning `429` with a `Retry-After` header when exceeded. Streaming/download
  paths are excluded on purpose. Enforced since [#69](https://github.com/justin13888/beam/issues/69);
  tunable via `BEAM_RATE_LIMIT_*` (see [configuration](../operations/configuration.md)).

## Threat model notes

**Why a cookie works for the `<video>` tag.** Range-request fetches from a player are subresource
requests: the browser attaches the httpOnly cookie without ever exposing it to page JavaScript.
Beam's deployment model is same-site in dev (different ports on `localhost`) and same-origin in prod
(one reverse-proxy origin), so the cookie is reliably attached to media requests — no query-string
token workaround is needed, and none exists. A cross-site embed of Beam's `<video>` on a third-party
page does not receive the cookie under `SameSite=Lax`, which is accepted: Beam's media is not meant
to be hotlinked.

**No server-side transcoding narrows the attack surface.** The playback path is a plain byte-range
file server — no external process invocation with attacker-influenceable inputs. See
[ADR-0004](decisions/ADR-0004-never-transcode.md).

**Accepted residual risk: unproxied image URLs.** Poster/backdrop URLs are direct TMDB/AniList CDN
links, so the viewer's browser (and IP) touches those third-party CDNs — a deliberate tradeoff
recorded in [ADR-0008](decisions/ADR-0008-image-cdn-direct.md). A server-side image proxy is
deferred — tracked in [#70](https://github.com/justin13888/beam/issues/70).
