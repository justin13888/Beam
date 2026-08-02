# ADR-0003: OIDC-only authentication via the BFF pattern

## Status

Accepted.

## Context

Beam previously authenticated users with username/password credentials hashed via Argon2, issued
HS256 JWTs signed with a shared `JWT_SECRET` environment variable, and backed sessions with
Redis/Valkey. Video playback additionally rode on a separate 6-hour "stream token" JWT passed via a
`?token=` query parameter, because the primary auth JWT wasn't naturally available to a `<video>`
tag's plain GET request. That design meant Beam owned password storage and verification (a
liability and a compliance/UX burden — password reset, credential-stuffing exposure, no SSO story
for organizations that already have an identity provider), a shared-secret JWT scheme (a single
leaked `JWT_SECRET` compromises every session and forged tokens are hard to detect), and a
token-in-URL pattern that is a well-known credential-leakage vector via logs, browser history, and
`Referer` headers.

## Decision

We replaced username/password entirely with OIDC: `beam-server` performs Authorization Code + PKCE
against a configured OIDC issuer (via the `openidconnect` crate), acting as a confidential client in
the backend-for-frontend pattern — the browser never handles IdP tokens directly. On successful
login, `beam-server` creates a session (opaque token, hashed at rest, stored in a Postgres
`sessions` table — see ADR-0005) and sets a single httpOnly, `SameSite=Lax` cookie as the browser's
only credential, used for every subsequent request including video playback. The `?token=`
query-parameter stream-token scheme was deleted outright. Users are JIT-provisioned keyed by
`(oidc_issuer, oidc_subject)`; admin status is resolved at every login, not read from a stored flag.
(The original email-allowlist mechanism was later replaced — see issue #85 — by deriving admin
solely from a configured ID-token claim, `BEAM_OIDC_ADMIN_CLAIM`; the "recompute every login"
principle here is unchanged.) Dev environments run Dex (a lightweight IdP with static users)
via `compose.dependencies.yaml`.

## Consequences

**Positive:**
- Beam never stores or verifies a password — that responsibility moves entirely to the IdP, which is
  built for it.
- One credential (the session cookie) instead of two (auth JWT + stream-token JWT) simplifies both
  the client and the threat model; no token ever needs to appear in a URL.
- Session revocation is a database row delete, not dependent on JWT expiry windows or a denylist.
- Any OIDC-compliant IdP works — Dex for dev, and operators can point prod at Keycloak, Authentik,
  Authelia, or a hosted identity provider of their choice, opening a path to SSO for organizational
  deployments.

**Negative / accepted cost:**
- Requires an OIDC issuer to be configured and reachable for login to work at all — there is no
  "just create an account" fallback; this raises the setup bar for a brand-new self-hoster, who must
  stand up (or already have) an IdP. Dex ships in the dev compose stack specifically to soften this
  for local development and evaluation.
- The BFF pattern's "cookie authenticates the `<video>` tag" property depends on same-site (dev) or
  same-origin (prod) deployment topology; a deployment that serves the web client and API from
  genuinely cross-site origins would need a different mechanism (this is treated as an unsupported
  topology, not an oversight — see `security.md`).
- Redis/Valkey was fully removed from the stack as a consequence (see ADR-0005), which is a net
  simplification but does remove one component some operators may already have tuned for other
  purposes in their deployment.
