# Security Architecture

Status: target architecture. See [ADR-0003](decisions/ADR-0003-oidc-bff-auth.md) and
[ADR-0005](decisions/ADR-0005-sessions-in-postgres.md) for the decisions this document details.

## Summary of the model

`beam-server` authenticates users via OIDC, using the backend-for-frontend (BFF) pattern: the
*server*, not the browser, holds the OIDC client credentials and performs the Authorization Code +
PKCE exchange. The browser never sees an ID token, access token, or refresh token. It holds exactly
one credential — an httpOnly, `SameSite=Lax` session cookie — for everything, including video
playback. There is no OIDC of any kind in the current codebase; today's auth is username/password +
Argon2 + HS256 JWT with a shared `JWT_SECRET`, plus a separate 6-hour "stream token" JWT carried in a
`?token=` query parameter for video playback. All of that is deleted.

## OIDC / BFF flow, end to end

1. **Login initiation.** Browser hits `GET /v1/auth/login`. `beam-server` generates a PKCE code
   verifier/challenge and a CSRF `state` value, stashes them server-side (short-lived, tied to a
   pre-session cookie), and redirects the browser to the configured OIDC issuer's authorization
   endpoint with the challenge and state attached.
2. **IdP authentication.** The user authenticates at the IdP (in dev: Dex, a lightweight IdP with
   static test users, run via `compose.dependencies.yaml`; in prod: whatever OIDC-compliant IdP the
   operator configures — Authelia, Keycloak, Authentik, a hosted provider, etc.). The IdP redirects
   back to `beam-server`'s callback URL with an authorization code.
3. **Callback / token exchange.** `GET /v1/auth/callback` validates `state`, then exchanges the
   authorization code plus the PKCE verifier for tokens directly with the IdP over a server-to-server
   request (using the `openidconnect` crate). The ID token's claims are validated (issuer, audience,
   signature, expiry).
4. **JIT provisioning.** `beam-server` looks up a `users` row by `(oidc_issuer, oidc_subject)`
   (the ID token's `iss`/`sub`). If none exists, one is created — there is no separate registration
   step. `is_admin` is (re-)computed at this point from the configured admin email allowlist, not
   read from a stored flag — see "Admin gating" below.
5. **Session creation.** `beam-server` generates an opaque, high-entropy session token, stores only
   its hash in the new `sessions` row (see `data-model.md`), and sets the session cookie:
   `HttpOnly`, `Secure` (prod), `SameSite=Lax`, scoped to the API's path/domain. The IdP tokens
   themselves are discarded once the session is established — `beam-server` does not need to keep a
   refresh token around, because it never talks to the IdP again on the user's behalf mid-session
   (see NFR notes on token refresh in `docs/requirements` for any future exception to this).
6. **Subsequent requests.** Every request to `beam-server` carries the session cookie automatically
   (browser default behavior for a same-site/same-origin request). Middleware hashes the presented
   token, looks it up in `sessions` by `token_hash`, checks `expires_at`, and attaches the resolved
   `user_id`/`is_admin` to the request context. No token ever needs to appear in a URL, a query
   string, or a `<video>` element's `src` attribute — see "Why cookies work for `<video>`" below.
7. **Logout.** `DELETE /v1/auth/session` deletes the corresponding `sessions` row server-side and
   clears the cookie. Deleting the row (not just the cookie) means a stolen cookie value is useless
   after logout, not merely inconvenient to use.

## Session model

Sessions live in Postgres (`sessions` table — see `data-model.md`), not Redis/Valkey, which is
dropped from the stack entirely this push. Key properties:

- **Hashed at rest.** Only `SHA-256(token)` is stored; the raw token exists solely in the browser's
  cookie jar and in transit over TLS. A database dump does not expose usable sessions.
- **Sliding TTL.** `expires_at` extends on activity (subject to a hard maximum lifetime), so an idle
  session expires but an actively-used one does not interrupt a long viewing session.
- **Server-side revocation.** Because the store is a real database the server controls, revoking a
  session (logout, admin-initiated "sign out this user," a future "sign out all devices" action) is
  a single row delete — no reliance on short JWT expiry windows or a separate denylist mechanism.
- **No JWTs for session state.** The session cookie carries only an opaque, unguessable token, not a
  signed/encoded claim bundle. This sidesteps the entire class of JWT-specific pitfalls (algorithm
  confusion, `none` algorithm, key confusion between HS256 and RS256, secret rotation invalidating all
  sessions at once) that the current HS256/shared-secret design is exposed to.

## No tokens in URLs

This is an absolute rule in the target architecture, not a best-effort guideline: no bearer token,
session token, or any other credential ever appears in a URL, query string, or path segment, anywhere
in the API. This directly replaces today's `?token=` stream-token JWT, which — like any token in a
URL — is exposed to server access logs, browser history, `Referer` headers sent to third parties on
subresource requests, and proxy/CDN logs, none of which are considered a safe place for credential
material to end up. Video playback authenticates via the same session cookie as every other request.

## CSRF model

`SameSite=Lax` on the session cookie is the primary defense: it is sent on top-level navigations and
on "safe" cross-site requests (GET), but not attached to cross-site POST/PUT/PATCH/DELETE requests
originating from another site, which is exactly the shape of a CSRF attack. This is backstopped by
explicit `Origin`/`Referer` allowlist validation on every unsafe HTTP method (POST, PUT, PATCH,
DELETE) at the middleware level — a request whose `Origin` doesn't match the configured allowed
origin(s) is rejected before it reaches a handler, independent of cookie behavior. Two layers are used
deliberately: `SameSite` protects against cookie-riding CSRF even from origins an attacker fully
controls; `Origin` validation protects against edge cases where `SameSite` enforcement varies by
browser or is bypassed by same-site-but-untrusted subdomains.

## Admin gating

`is_admin` is resolved from a configured admin **email allowlist**, checked fresh at every login and
written to the `users` row at that time — it is explicitly not treated as durable state that can
silently drift from the source of truth (an operator removing an email from the allowlist takes
effect the next time that user logs in, without a separate admin-revocation action). Admin-only
mutations (library management, re-enrichment triggers, log viewing) are gated by this resolved role
check at the handler/middleware level, applied consistently across every admin-surfaced endpoint.
**Changed from today:** the current GraphQL stack has at least one library-mutation path where
admin-gating is inconsistently applied relative to the rest of the admin surface; the REST rewrite
fixes this as a byproduct of routing every admin mutation through one shared gating middleware rather
than resolver-by-resolver checks.

## Read-only media filesystem as a security boundary

`beam-server` (and `beam-index` within it) mounts the media library read-only. This is treated as a
hard architectural invariant, not just an operational convention, specifically because it bounds the
blast radius of any future path-traversal-class bug in file-serving code: even in the worst case where
an attacker tricks the server into resolving a request to an unintended path, the server physically
cannot use that access to modify or delete anything in the media library, only to read it (and even
read access is scoped to the library's configured root paths in practice). The server's own writable
storage — its data/cache directory — is a separate filesystem location from the media library, and
nothing about media playback or indexing ever needs to write into the library tree.

The domain API surface reinforces the same boundary at a different layer: it exposes IDs and domain
primitives (`fileId`, `movieId`, quality labels) to clients, never raw filesystem paths. A client
cannot construct a request that references an arbitrary filesystem location even in principle — every
resource reference is an opaque ID resolved server-side against the catalog. This also keeps the API
contract stable for future non-web clients (mobile, TV) without redesigning it around filesystem
exposure.

## Rate limiting intent

Auth endpoints (`/v1/auth/login`, `/v1/auth/callback`) and any endpoint that resolves a
user-controlled identifier against the session store are intended targets for request-rate limiting
(per-IP and/or per-session) to blunt credential-stuffing-adjacent and session-enumeration attempts.
This push establishes the middleware seam for rate limiting as part of the auth request path; the
specific limits and backing store (in-process vs. shared) are an operational tuning concern outside
this document's scope.

## Threat model notes

**Why a cookie is safe to use for the `<video>` tag specifically.** A `<video src="...">` (or the
underlying Range-request fetches a player library issues) is a subresource request, not a script
context — the browser attaches cookies to it the same way it does for an `<img>` tag, without ever
exposing the cookie's value to page JavaScript (the cookie is `HttpOnly`). Because Beam's deployment
model is always same-site in dev (frontend and backend on different ports of the same host,
`localhost`) and same-origin in prod (both served through one reverse-proxy origin), the cookie is
reliably attached to these media requests without needing a workaround like a query-string token —
which is precisely the workaround today's `?token=` scheme exists to provide, and precisely why that
workaround is no longer necessary once the deployment topology is same-site/same-origin by
convention. A cross-site embed of Beam's `<video>` element on a third-party page would *not* receive
the cookie under `SameSite=Lax` for the initial navigation-adjacent case, which is an accepted
consequence: Beam is not designed to allow its media to be hotlinked/embedded cross-site, and this
cookie behavior enforces that rather than working around it.

**Why no server-side transcoding also narrows the attack surface.** Deleting the ffmpeg CLI
shell-out (`Command::new("ffmpeg")`) from the request path removes an entire class of
command-injection and resource-exhaustion risk that comes from invoking an external process per
playback request with attacker-influenceable inputs (subtitle content, filenames). What remains is a
plain byte-range file server, which has a much smaller and better-understood attack surface.

**Accepted residual risk: unproxied image URLs.** Poster/backdrop URLs are stored and served as
direct TMDB/AniList CDN links rather than proxied through `beam-server`. This means a user's browser
makes a direct request to a third-party CDN when rendering a poster, which is a minor
information-leakage tradeoff (the CDN sees the user's IP; TMDB/AniList are treated as low-risk
third parties). This is a documented, deliberate tradeoff for this push, not an oversight — a
server-side image proxy remains a candidate future addition (see `docs/requirements/product.md`'s
out-of-scope list).
