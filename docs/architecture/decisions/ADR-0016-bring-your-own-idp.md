# ADR-0016: Beam is bring-your-own-IdP; no bundled identity provider

## Status

Accepted. Extends [ADR-0003](ADR-0003-oidc-bff-auth.md) and settles
[#149](https://github.com/justin13888/beam/issues/149); does not supersede it.

## Context

ADR-0003 made OIDC the only way to sign in to Beam and recorded the price as an accepted cost: it
"raises the setup bar for a brand-new self-hoster, who must stand up (or already have) an IdP."
#73 then made the bundled Dex opt-in, behind the `dev-idp` Compose profile, and confirmed it as a
development fixture only (FR-110).

#149 asked the separate product question that left open: should Beam ship a *supported* "no
external IdP" deployment mode, with Dex bundled as the credential store, for self-hosters who will
never run a general-purpose identity provider? Technically it is a small step. Dex's builtin
connector with `enablePasswordDB` and `staticPasswords` authenticates bcrypt-hashed users and speaks
OIDC, so `beam-server` would not change at all.

#149 named a blocker: FR-106 derives admin solely from an IdP-asserted ID-token claim, and Dex's
builtin connector could not emit `groups` for static users
([dexidp/dex#1080](https://github.com/dexidp/dex/issues/1080),
[#3958](https://github.com/dexidp/dex/issues/3958)). **That blocker no longer holds.**
[dexidp/dex#4456](https://github.com/dexidp/dex/pull/4456) added `groups` and `preferredUsername`
to `staticPasswords`, released in Dex v2.45.0; #1080 was closed as completed on 2026-02-23, and
`compose.dependencies.yaml` already pins v2.45.1. The dev fixture at the time still pointed
`BEAM_OIDC_ADMIN_CLAIM` at `email_verified`, making every dev user an admin, but that was by then a
fixture choice rather than a Dex limitation (since changed, see the follow-up below).

So the question has to be decided on what remains. #149 listed that too.

## Decision

**Beam does not ship, document, or support a bundled identity provider for deployment. Beam is
bring-your-own-IdP, permanently.** The bundled Dex stays exactly what FR-110 says it is: an opt-in
development fixture. The setup bar ADR-0003 accepted is lowered instead by provider-specific
quickstarts in the user documentation, for Authentik, Keycloak, Authelia and Pocket ID.

The admin-claim gap is deliberately **not** among the reasons. It was the only reason #149 gave
that depended on a third party, and it has closed. What decides this is everything else a
supported bundled IdP would make Beam own:

**It re-imports the liability ADR-0003 removed.** ADR-0003's first positive consequence was that
"Beam never stores or verifies a password — that responsibility moves entirely to the IdP, which is
built for it." A bundled Dex puts the password database back inside Beam's deployment and Beam's
support promise. Dex's password DB provides authentication and nothing around it: no self-service
reset, no rotation guidance, and no account-recovery story. Every one of those gaps becomes a Beam
bug report, and the answer ADR-0003 gave — "that belongs to your IdP" — stops being available.

**User management would have to be built.** Static users live in Dex's config file, so adding a
user means editing YAML and restarting the IdP. Dynamic users need Dex's gRPC API, and no
management UI ships. A supported mode would therefore need a Beam-side user administration surface
— a sign-up, password and user-list UI in all but name — which is the thing FR-101 says does not
exist.

**Secrets and posture become Beam's.** The fixture's client secret, `beam-dev-secret`, is
committed. A deployment mode needs generated secrets, a rotation procedure, and a hardening story
for an internet-facing login form. Shipping it makes Beam answerable for an IdP's security posture
in a way that "point `BEAM_OIDC_ISSUER` at your provider" never does.

**The self-hosters it would serve already have a better option.** Lightweight single-container
providers built for exactly this audience exist and are maintained by people whose product *is* the
IdP. Pocket ID, for one, authenticates with passkeys and keeps no passwords at all. Documenting them costs Beam a
page each and no security promise.

## Consequences

**Positive:**
- The trust boundary ADR-0003 drew stays where it is: Beam holds a session cookie and an
  `(issuer, subject)` pair, never a credential.
- No new configuration surface, no second deployment topology, and nothing to test that
  `cargo test --workspace` cannot already exercise through `OidcClient` / `FakeOidcClient`.
- The quickstarts document the one step every provider gets wrong in a different way — getting the
  admin claim into the **ID token** — which the generic instructions left to the operator.

**Negative / accepted cost:**
- A self-hoster with no IdP still has to run one. The quickstarts make that shorter; they do not
  make it disappear.
- The quickstarts describe third-party software Beam does not control and will drift as those
  products change. They are kept to concepts (a confidential client, the redirect URI, the claim
  in the ID token) rather than UI paths, and each ends in a check against Beam itself (`/v1/me`)
  so a stale instruction fails visibly rather than silently.

**Follow-up, not part of this decision:**
- Now that Dex v2.45+ can assert `groups` for static users, the dev fixture could give only its
  admin user a `beam-admin` group and point `BEAM_OIDC_ADMIN_CLAIM` at `groups`, so the dev stack
  exercises a non-admin user. That is a change to `dex/config.yaml` and `mise run dev:up`, and it
  does not reopen this decision. Done in #195.

**Reversing this decision** means a new ADR adopting a bundled IdP that supersedes this one. That
ADR would have to answer the user-management, password-lifecycle and secret-rotation questions
above; the Dex group-claim limitation is no longer an obstacle to it.
