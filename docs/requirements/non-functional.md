# Beam — Non-Functional Requirements

Status: target state for the current push. Requirements use RFC 2119 keywords (MUST, MUST NOT,
SHOULD, SHOULD NOT, MAY) to indicate normative strength. See `product.md` for narrative context and
`functional.md` for numbered functional requirements (referenced below as FR-xxx).

## NFR-1xx — Security

- **NFR-101**: The server MUST support exactly one authentication mechanism: OIDC via the
  backend-for-frontend pattern (FR-101–FR-110). No alternate auth mechanism (password, API key,
  bearer token issued to the browser) MUST be offered as a login path.
- **NFR-102**: No bearer token, session token, or other credential MUST ever appear in a URL, query
  string, or Referer-visible location, for any endpoint including streaming and download (FR-504).
  This supersedes and replaces the prior 6-hour `?token=` stream-token JWT design, which MUST be
  removed rather than retained as a fallback.
- **NFR-103**: The session cookie MUST be `httpOnly` and `SameSite=Lax` (FR-103). The web application
  MUST NOT persist any authentication-relevant value in `localStorage`, `sessionStorage`, or a
  non-httpOnly cookie.
- **NFR-104**: All state-mutating requests (non-GET methods) MUST be validated against the `Origin`
  and/or `Referer` header by the server, in addition to `SameSite=Lax` cookie behavior, as defense in
  depth against CSRF. Requests with a missing or mismatched `Origin`/`Referer` on a mutating request
  MUST be rejected.
- **NFR-105**: Every admin-only mutation (library CRUD, rescan trigger, re-enrich trigger, log
  access) MUST be authorized against the admin role at the server, independent of any client-side UI
  gating (FR-601–FR-607). Authentication alone MUST NOT be treated as sufficient authorization for
  these endpoints. This closes a known gap in the prior GraphQL implementation, where library
  mutations were reachable by any authenticated user.
- **NFR-106**: The server's indexer and streaming/download paths MUST treat configured library root
  paths as read-only. The server process SHOULD run with filesystem permissions that make write
  access to library roots impossible, not merely avoided by convention (FR-202).
- **NFR-107**: Authentication-related endpoints (login initiation, callback, session refresh) MUST be
  subject to rate limiting or an equivalent abuse-resistance mechanism to reduce exposure to
  credential-stuffing or denial-of-service attempts against the identity provider integration.
- **NFR-108**: The domain API MUST NOT expose raw filesystem paths, database primary keys of internal
  infrastructure tables, or other implementation details capable of enabling path traversal or
  infrastructure-probing, in any client-facing response (FR-407).

## NFR-2xx — Testability

- **NFR-201**: The full Rust workspace test suite MUST pass via `cargo test --workspace` with zero
  external infrastructure running — no Postgres, Redis, Docker Compose, or network access MUST be
  required for any unit or subcutaneous test to pass.
- **NFR-202**: Every external boundary (database access via `beam-entity`, filesystem I/O, OIDC
  provider calls, TMDB/AniList calls via `cameo`) MUST be abstracted behind a Rust trait, with an
  in-memory or fake implementation available for tests, per the project's stated trait-based
  abstraction pattern.
- **NFR-203**: Complex stateful external dependencies (e.g., a media repository, a session store)
  SHOULD be tested against a stateful `InMemory*` fake rather than a mocking framework. `mockall`
  SHOULD be reserved for simple, strict contract verification, not for simulating complex state
  transitions.
- **NFR-204**: Core request-handling flows (auth, library CRUD, search, playback position updates,
  admin actions) MUST have at least one subcutaneous end-to-end test that instantiates the
  application router with in-memory implementations and drives it via Axum's test helpers, asserting
  on both the HTTP response and the resulting state mutation.
- **NFR-205**: Edge cases that would otherwise require manual verification (missing file, corrupted
  or unreadable media entry, database-call failure, expired session, unauthorized admin action) MUST
  be codified as unit tests by configuring the relevant injected trait to return the corresponding
  `Result::Err`, rather than left as manual QA steps.
- **NFR-206**: Rust workspace code coverage SHOULD be maintained at or above 70%, and web
  (`beam-web`) code coverage SHOULD be maintained at or above 60%, measured in CI.
- **NFR-207**: Domain entity test data MUST be constructed via builder patterns defined in
  `#[cfg(test)]` modules, rather than duplicated ad hoc struct literals across test files.
- **NFR-208**: CI MUST enforce, on every pull request: `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, the full Rust test suite, and the TypeScript `bun run check` step (
  covering lint/typecheck for `beam-web`/`beam-docs`).

## NFR-3xx — Performance

- **NFR-301**: Search (FR-404) MUST be executed as a server-side Postgres query using `pg_trgm`
  similarity. The server MUST NOT implement search by loading the full title set into application
  memory (`find_all()`) and filtering in application code, regardless of library size.
- **NFR-302**: Metadata enrichment (FR-301–FR-309) MUST run asynchronously relative to request
  handling and library scanning, and MUST NOT hold a scan or an HTTP request open while waiting on an
  external TMDB/AniList call.
- **NFR-303**: Streaming and download responses (FR-501, FR-502) MUST be served by streaming file
  bytes from disk to the HTTP response incrementally. The server MUST NOT read an entire media file
  into memory before beginning to respond.
- **NFR-304**: Streaming and download endpoints MUST support HTTP Range requests (`Range`,
  `Content-Range`, `Accept-Ranges`) so that seeking and resumable downloads do not require
  re-transferring previously-sent bytes.
- **NFR-305**: Streaming and download endpoints SHOULD support conditional requests via `ETag` and/or
  `Last-Modified`/`If-Modified-Since`, allowing clients and intermediary caches to avoid re-fetching
  unchanged byte ranges.
- **NFR-306**: Library scanning (FR-201–FR-210) MUST detect already-indexed, unchanged files via
  modification-time comparison and MUST skip re-processing them, so that repeated scans of a large,
  mostly-unchanged library remain fast.
- **NFR-307**: SSE-based progress reporting (FR-208, FR-309) MUST be used for scan and enrichment
  progress in place of client-side polling, to avoid unnecessary repeated request/response overhead
  during long-running operations.

## NFR-4xx — Operability

- **NFR-401**: Beam MUST be deployable as a single binary (`beam-server`), with indexing,
  enrichment, and API serving running in-process, rather than requiring coordination of multiple
  server processes for a functioning deployment.
- **NFR-402**: All environment-specific configuration (library roots, data directory, OIDC issuer/
  client credentials, `BEAM_ADMIN_EMAILS`, TMDB API key) MUST be configurable via environment
  variables, with sensible behavior (per FR-307) when optional variables are absent.
- **NFR-403**: The server MUST emit structured logs (machine-parseable, e.g. JSON or key-value
  fields) for scan lifecycle events, enrichment lifecycle events, authentication events, and
  admin-mutating actions, sufficient to back the admin log viewer (FR-604, FR-605).
- **NFR-404**: Real-time progress surfaces (scan, enrichment) MUST be implemented via Server-Sent
  Events rather than WebSockets or GraphQL subscriptions, consistent with the removal of the GraphQL
  stack.
- **NFR-405**: The server MUST expose a single OpenAPI specification describing its full REST API
  surface, from which the TypeScript client types consumed by `beam-web` are generated, keeping the
  client and server contract in sync without hand-maintained duplicate type definitions.
- **NFR-406**: Local development dependencies (Postgres, Dex) MUST be runnable via
  `compose.dependencies.yaml` without requiring any additional external service to be introduced
  without a corresponding in-memory trait implementation being provided first, per the project's
  workflow rules.

## NFR-5xx — Privacy

- **NFR-501**: Poster and backdrop images MUST be served to the browser as direct URLs pointing at
  the TMDB/AniList CDN in this push. This is a deliberate, documented tradeoff: it avoids
  server-side image proxying/caching work this push, at the cost of the end user's browser making
  direct requests to third-party CDNs (and those third parties observing that a given viewer is
  requesting a given title's artwork). This is not an oversight and MUST be treated as a known,
  accepted limitation rather than a defect, until server-side image proxying is scoped as future
  work.
- **NFR-502**: The server MUST NOT forward any first-party session cookie, auth header, or other
  Beam-specific credential to TMDB/AniList when resolving enrichment data or when the client fetches
  CDN image URLs directly.

## NFR-6xx — Extensibility

- **NFR-601**: The domain API MUST express itself entirely in domain terms (title id, file id,
  season/episode id, user id) and MUST NOT require a client to know or construct filesystem paths,
  internal database row identifiers unrelated to the domain, or web-framework-specific conventions,
  so that a future native client can be built against the same API without server changes.
- **NFR-602**: Business logic in the service layer MUST remain isolated from web-framework types
  (HTTP requests/responses/extractors); such logic MUST be reachable and testable without going
  through an HTTP layer, preserving the option to expose it via a non-HTTP transport in the future
  without a rewrite.
- **NFR-603**: The three delivery scenarios (FR-501–FR-506) MUST be modeled as domain concepts
  independent of any particular client's UI, so that a future client can implement its own source
  picker or download UI against the same underlying API contract.
- **NFR-604**: Introducing a new external service dependency into `compose.dependencies.yaml` MUST
  NOT occur without first providing an in-memory trait implementation usable by the test suite,
  per the project's workflow rules, so that future extensions do not erode zero-dependency
  testability (NFR-201).
