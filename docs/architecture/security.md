# Security Architecture

`beam-server` authenticates users via OIDC in the backend-for-frontend (BFF) pattern: the *server*
holds the OIDC client credentials and performs the Authorization Code + PKCE exchange; the browser
never sees an ID, access, or refresh token. The browser holds exactly one credential — the
`beam_session` httpOnly, `SameSite=Lax` cookie — for everything, including video playback. A native
client with no browser signs in by the device authorization grant, which the server also runs
against the IdP itself, and holds the same opaque credential (see
[below](#device-authorization-grant) and [ADR-0017](decisions/ADR-0017-device-authorization-grant.md)). See
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
   Every server-to-IdP request (discovery, JWKS, token exchange) goes through `OidcHttpClient`
   (`beam-auth/src/utils/oidc.rs`): redirects are never followed, TLS is verified against the
   system trust store, and requests are bounded (10 s to connect, 30 s in total) so an IdP that
   stops answering cannot stall startup discovery or a login.
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

## Device authorization grant

For a client with no browser (a TV), the server runs RFC 8628 against the IdP on the client's
behalf:

1. **Start.** `POST /v1/auth/device` calls the IdP's device authorization endpoint (read from
   discovery; absent → `501 device-login-unsupported`), stores the IdP's device code in
   `device_auths` keyed by the SHA-256 of a fresh 256-bit **device handle**, and returns the handle,
   the user code and the verification URI. The device code never leaves the server, so the client
   can only ever turn an approval into a Beam session — never into IdP tokens.
2. **Approve.** The user opens the verification URI on another device and signs in at the IdP.
3. **Poll.** `POST /v1/auth/device/token` claims the flow with a conditional `UPDATE` on
   `next_poll_at`: a poll inside the interval is answered `slow_down` by Beam without contacting the
   IdP, and the interval grows. A claimed poll makes exactly one token-endpoint request through
   `OidcHttpClient`. Both device requests authenticate as the code exchange does, with the one
   method `BEAM_OIDC_CLIENT_AUTH_METHOD` names -- `client_secret_basic` (the default, with
   `client_id` repeated in the form) or `client_secret_post` -- and a refusal is never retried with
   the other. Discovery's `token_endpoint_auth_methods_supported` is not consulted: it lists what
   the IdP can accept, not the method Beam's client is registered with (ADR-0017 D151-9). The start and every `200`/`202`
   poll answer carry `Cache-Control: no-store` and `Pragma: no-cache` (RFC 6749 section 5.1). On
   approval the ID token is verified (signature, issuer, audience, expiry — there is no nonce in
   this grant), the flow row is deleted
   with `DELETE ... RETURNING` (so one approval mints one session), and steps 4–5 of the browser
   flow run unchanged through the shared `complete_login`. The session value is returned in the
   body; the client presents it as the `beam_session` cookie.

**Phishing (RFC 8628 section 5.4).** Whoever starts a flow receives the session its user code
approves, so an attacker can start a flow and try to talk a victim into entering the code. Beam caps
a flow's lifetime at 30 minutes, makes every flow single-use, and relies on the IdP's approval
screen naming the client; the operator documentation tells users never to enter a code they did not
start. The residual risk is inherent to the grant.

**Handles are hashed at rest** like session tokens: a dump of `device_auths` cannot be replayed as a
poll.

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
  neither header pass (NFR-104): browsers always send `Origin` on a non-GET request, so its
  absence marks a non-browser client — a native app signing in by the device grant sends neither —
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
server-side against the catalog. The AdminAuth-gated operational surface is a sanctioned exemption
(NFR-108): admin events, the admin event stream and the admin log carry the configured root path so
that the operator can fix it. Nothing there is a resource reference — no request path is ever
resolved from a client-supplied string.

File listings open to every signed-in user (`getLibraryFiles`, `GET /v1/libraries/{id}/files`)
carry each file's path relative to its library root, for display only — never the absolute path,
and never something a request resolves. The absolute path the delivery routes open stays in a
server-internal type (`LocatedFile`) that cannot be serialized into a response.

The indexer also *parses* files it did not write: the Kodi `.nfo` files beside the media (FR-219).
Anyone who can drop a file into a library can hand Beam one, so an NFO is treated as hostile
input. It is opened read-only, only when it is a regular file (a symbolic link is never followed:
it is refused at the `lstat`, and the open itself follows no link beneath the library root, so a
link swapped in between the two -- for the NFO or for a folder above it -- fails to open; it also
carries `O_NONBLOCK`, so a FIFO swapped in opens at once and is refused as not a regular file
instead of blocking the scan),
and at most 1 MiB of it is read; bytes that are not UTF-8 are refused rather than guessed at; a
document type declaration is refused before parsing, so no entity is ever expanded (the billion
laughs); and the XML parser (`roxmltree`, which resolves no external resources) is capped at
10 000 nodes. A rejected NFO is logged and ignored: the file is classified by its path. Subtitle
files are only stat-ed and their names read; their contents are never opened by the indexer.
The walk never follows a link, and every later read of a library file opens it beneath the root
with no link followed at any component: NFO reads, all file delivery, and the indexer's stat, hash
and probe of a video (issue #238). The indexer opens a video once
(`beam_index::library_file::LibraryFile::open`) and takes everything it records from that handle:
its size, modification time and identity from the handle's `fstat`, its content hash from the
handle's bytes, and its streams from FFmpeg reading the same handle through a custom I/O context
(`StreamIo`), so FFmpeg never opens the path itself. A subtitle's size and modification time on a
watcher event are read the same way. A link swapped in at the file or at any folder above it
between the walk and those reads fails to open (`ELOOP`), and the path is treated as missing, as a
link the walk saw is -- nothing is recorded from the file it would have led to.

Delivery reads library files too, and a file can change between the scan that recorded it and the
request that reads it. Every file the server serves — a video on `/stream` and `/download`, a
subtitle file as stored or as WebVTT (FR-512) — is opened through the same helper as an NFO
(`beam_index::library_file::open_regular_file`), which takes the library root and the path
beneath it and follows no symbolic link anywhere below the root. On Linux the kernel resolves the
path from the open root with `openat2(RESOLVE_NO_SYMLINKS | RESOLVE_BENEATH)`; on other Unix, and
on a Linux kernel without `openat2`, it is walked from the root one `openat` at a time, each folder
with `O_NOFOLLOW | O_DIRECTORY` and the file with `O_NOFOLLOW | O_NONBLOCK`. Only plain names are
accepted beneath the root (no `..`, no absolute path), and the open is refused unless the handle
`fstat`s as a regular file. The root itself is opened as configured: it is the administrator's
choice of folder, and one that is itself a link is followed. So a file -- or any folder above it --
replaced by a symbolic link to something outside the library (`/proc/self/environ`, a secrets
file), or a file replaced by a FIFO or by a device such as `/dev/zero`, answers
`#source-file-missing`, as a deleted file does, and is never read. Length,
modification time and every byte served come from that one handle, never from a second lookup of
the path. A subtitle is treated as hostile input as an NFO is: the WebVTT rendition reads at most
8 MiB of it, checked against the handle's size and again against the bytes read, and converts it in
time linear in its length whatever markup it holds, so a crafted file cannot hold a worker for
longer than an honest file of the same size.

## Operational hardening

- Startup logs redact secrets: `ServerConfig` has a hand-written `Debug` impl that redacts
  credential fields, and destructures all fields so adding a config field without classifying it is
  a compile error.
- Request handlers return `500` instead of panicking when expected injected state is missing —
  a wiring bug degrades one request, not the process.
- Rate limiting: in-process token buckets (see `beam-server/src/routes/rate_limit.rs`) guard the auth
  endpoints (`/v1/auth/login`, `/v1/auth/callback`, `/v1/auth/device`), device-login polls
  (`/v1/auth/device/token`, their own class) and the browse/search endpoint (`GET /v1/media`),
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

**Artwork does not leave the deployment.** Poster and backdrop URLs are Beam's own
`/v1/artwork` paths: the server fetches each image from the provider once and serves it from its
cache, so no viewer's client contacts TMDB or AniList and neither CDN can observe who is browsing
what ([ADR-0015](decisions/ADR-0015-artwork-served-by-beam.md), NFR-501). This replaces the
residual risk [ADR-0008](decisions/ADR-0008-image-cdn-direct.md) accepted.

**The artwork endpoint is not an open proxy.** The only URL it will fetch is one enrichment itself
wrote onto a row; a client sends a title id and two enum variants, never a URL, so there is no
allowlist to maintain and no bypass to find. The outbound fetch additionally refuses anything that
is not `https`, will not follow a redirect off `https`, refuses an over-large body before reading
it, accepts only image content types, and carries no Beam credential (NFR-502) — the client is
built with no cookie jar, so there is nowhere for the session cookie to be attached from.

**Why a cookie works for an `<img>` tag.** The same reason it works for `<video>` above: artwork is
a subresource request, and every supported deployment is same-site, so `SameSite=Lax` attaches the
session cookie without it ever reaching page JavaScript.
