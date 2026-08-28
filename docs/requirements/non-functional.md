# Beam — Non-Functional Requirements

Requirements use RFC 2119 keywords (MUST, MUST NOT, SHOULD, SHOULD NOT, MAY) to indicate normative
strength. See `product.md` for narrative context and `functional.md` for numbered functional
requirements (referenced below as FR-xxx).

## NFR-1xx — Security

- **NFR-101**: The server MUST support exactly one authentication mechanism: OIDC via the
  backend-for-frontend pattern (FR-101–FR-110,
  [ADR-0003](../architecture/decisions/ADR-0003-oidc-bff-auth.md)). No alternate auth mechanism
  (password, API key, bearer token issued to the browser) is offered as a login path.
- **NFR-102**: No bearer token, session token, or other credential MUST ever appear in a URL, query
  string, or Referer-visible location, for any endpoint including streaming and download (FR-504).
- **NFR-103**: The session cookie MUST be `httpOnly` and `SameSite=Lax` (FR-103). The web application
  MUST NOT persist any authentication-relevant value in `localStorage`, `sessionStorage`, or a
  non-httpOnly cookie. A likely cookie-`Secure` misconfiguration (e.g., `BEAM_COOKIE_SECURE=false`
  against an HTTPS deployment) is a startup error: the server refuses to start rather than serving
  with an insecure session cookie.
- **NFR-104**: All state-mutating requests (non-GET methods) MUST be validated against the `Origin`
  and/or `Referer` header by the server, in addition to `SameSite=Lax` cookie behavior, as defense in
  depth against CSRF. Requests with a missing or mismatched `Origin`/`Referer` on a mutating request
  MUST be rejected.
- **NFR-105**: Every admin-only mutation (library CRUD, rescan trigger, re-enrich trigger, log
  access) MUST be authorized against the admin role at the server, independent of any client-side UI
  gating (FR-601–FR-607). Authentication alone MUST NOT be treated as sufficient authorization.
- **NFR-106**: The server's indexer and streaming/download paths MUST treat configured library root
  paths as read-only. The server process SHOULD run with filesystem permissions that make write
  access to library roots impossible, not merely avoided by convention (FR-202).
- **NFR-107**: Authentication-related endpoints (login initiation, callback, session refresh) SHOULD
  be subject to rate limiting or an equivalent abuse-resistance mechanism. Enforced since
  [#69](https://github.com/justin13888/beam/issues/69) via in-process token buckets on the auth
  endpoints (`/v1/auth/login`, `/v1/auth/callback`) and the browse/search endpoint (`GET /v1/media`),
  keyed per client IP. Streaming and download paths are deliberately excluded (a player legitimately
  bursts range requests). Tunable and switchable via the `BEAM_RATE_LIMIT_*` variables.
- **NFR-108**: The domain API MUST NOT expose raw filesystem paths, database primary keys of internal
  infrastructure tables, or other implementation details capable of enabling path traversal or
  infrastructure-probing, in any client-facing response (FR-407).

## NFR-2xx — Testability

- **NFR-201**: The full Rust workspace test suite MUST pass via `cargo test --workspace` with zero
  external infrastructure running — no Postgres, Docker Compose, or network access is required for
  any unit or subcutaneous test to pass.
- **NFR-202**: Every external boundary (database access via `beam-entity`, filesystem I/O, OIDC
  provider calls, TMDB/AniList calls via `cameo`) MUST be abstracted behind a Rust trait, with an
  in-memory or fake implementation available for tests.
- **NFR-203**: Complex stateful external dependencies (e.g., a media repository, a session store)
  SHOULD be tested against a stateful `InMemory*` fake rather than a mocking framework. `mockall`
  SHOULD be reserved for simple, strict contract verification.
- **NFR-204**: Core request-handling flows (auth, library CRUD, search, playback position updates,
  admin actions) MUST have at least one subcutaneous end-to-end test that instantiates the
  application router with in-memory implementations and drives it through the active framework's
  in-process test client, asserting on both the HTTP response and the resulting state mutation.
  Automated browser e2e tests are deferred — tracked in
  [#74](https://github.com/justin13888/beam/issues/74).
- **NFR-205**: Edge cases that would otherwise require manual verification (missing file, corrupted
  or unreadable media entry, database-call failure, expired session, unauthorized admin action) MUST
  be codified as unit tests by configuring the relevant injected trait to return the corresponding
  `Result::Err`.
- **NFR-206**: Rust workspace coverage SHOULD be maintained at or above 70% of lines, and web
  (`beam-web`) coverage at or above 60% of lines, measured and enforced in CI. Rust MUST additionally
  gate region and function coverage; regions stand in for branches, whose measurement requires a
  nightly toolchain the project does not use. Thresholds are calibrated below the measured value and
  ratcheted upward; a threshold MUST NOT be lowered to make a failing pull request pass.
- **NFR-207**: Domain entity test data MUST be constructed via shared builder patterns exposed under
  the `test-utils` feature, rather than duplicated ad hoc struct literals across test files.
- **NFR-209**: A test MUST NOT take a test double as its subject. Asserting the behaviour of an
  `InMemory*`, `Fake*`, `Stub*`, `Noop*`, `Mock*` or `TestClock` verifies scaffolding, not the
  product. Where a fake and a real implementation share a trait, both MUST be verified by the same
  shared contract test.
- **NFR-210**: Mutation testing (`cargo-mutants`) MUST be available as a project task and run in CI
  as an advisory, non-blocking signal. A surviving mutant MUST be resolved by removing the redundant
  branch, or by making the illegal state unrepresentable, in preference to adding a test; an
  exemption MUST be recorded with a justification of why the mutant is behaviourally equivalent.
- **NFR-211**: Test doubles gated on `feature = "test-utils"` MUST be excluded from mutation
  explicitly, and that exclusion MUST be enforced by an automated check rather than by convention.
- **NFR-212**: Repository implementations MUST have their generated SQL verified without external
  infrastructure. Behaviour that only a real Postgres can exhibit (trigram similarity, index usage,
  migration up/down) MUST be covered by a tier that is disabled by default, so NFR-201 holds
  unconditionally.
- **NFR-213**: Every route registered on the HTTP router MUST appear exactly once in the generated
  OpenAPI document, and vice versa, verified by an automated test. Web test doubles MUST be typed
  against the generated client so a response shape cannot drift from the specification unnoticed.
- **NFR-214**: Code whose behaviour depends on the passage of time MUST read it from an injected
  clock rather than from the wall clock directly. A test MUST NOT use a real sleep to order two
  events or to reach an expiry; it MUST advance the injected clock. This is what makes
  session TTLs, rescan cadence, debounce windows and rate-limit refill assertable at all, and it
  removes the class of failure where a test passes or fails according to machine load.
- **NFR-208**: CI MUST enforce, on every pull request: `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, the full Rust test suite, and the TypeScript `bun run check` step
  (covering lint/typecheck for `beam-web`/`beam-docs`).

## NFR-3xx — Performance

- **NFR-301**: Search (FR-404) MUST be executed as a server-side Postgres query using `pg_trgm`
  similarity. The server MUST NOT implement search by loading the full title set into application
  memory and filtering in application code, regardless of library size.
- **NFR-302**: Metadata enrichment (FR-301–FR-309) MUST run asynchronously relative to request
  handling and library scanning, and MUST NOT hold a scan or an HTTP request open while waiting on an
  external TMDB/AniList call.
- **NFR-303**: Streaming and download responses (FR-501, FR-502) MUST be served by streaming file
  bytes from disk to the HTTP response incrementally, never by reading an entire media file into
  memory first.
- **NFR-304**: Streaming and download endpoints MUST support HTTP Range requests (`Range`,
  `Content-Range`, `Accept-Ranges`) so that seeking and resumable downloads do not require
  re-transferring previously-sent bytes.
- **NFR-305**: Streaming and download endpoints SHOULD support conditional requests via `ETag` and/or
  `Last-Modified`/`If-Modified-Since`, allowing clients and intermediary caches to avoid re-fetching
  unchanged byte ranges.
- **NFR-306**: Library scanning (FR-201–FR-210) MUST detect already-indexed, unchanged files via
  modification-time comparison and MUST skip re-processing them, so that repeated scans of a large,
  mostly-unchanged library remain fast.
- **NFR-307**: Scan and enrichment progress (FR-208, FR-309) MUST be reported over SSE rather than
  client-side polling, avoiding repeated request/response overhead during long-running operations.

## NFR-4xx — Operability

- **NFR-401**: Beam MUST be deployable as a single binary (`beam-server`), with indexing, enrichment,
  and API serving running in-process
  ([ADR-0001](../architecture/decisions/ADR-0001-modular-monolith.md)). Database migrations apply
  automatically at startup (`BEAM_AUTO_MIGRATE`, default `true`), so no separate migration step is
  required to deploy.
- **NFR-402**: All environment-specific configuration MUST be configurable via `BEAM_`-prefixed
  environment variables (e.g., `BEAM_VIDEO_DIR`, `BEAM_DATA_DIR`, `BEAM_DATABASE_URL`,
  `BEAM_OIDC_ISSUER`, `BEAM_OIDC_ADMIN_CLAIM`, `BEAM_TMDB_API_TOKEN`), with sensible behavior (per
  FR-307) when optional variables are absent. See `../operations/configuration.md`.
- **NFR-403**: The server MUST emit structured logs (machine-parseable, e.g. JSON or key-value
  fields) for scan lifecycle events, enrichment lifecycle events, authentication events, and
  admin-mutating actions, sufficient to back the admin log viewer (FR-604, FR-605).
- **NFR-404**: Real-time progress surfaces (scan, enrichment) MUST be implemented via Server-Sent
  Events rather than WebSockets or GraphQL subscriptions, consistent with the REST-only API
  ([ADR-0010](../architecture/decisions/ADR-0010-openapi-3-2-kynos.md)).
- **NFR-405**: The server MUST expose a single OpenAPI specification describing its full REST API
  surface, from which the TypeScript client types consumed by `beam-web` are generated, keeping the
  client and server contract in sync without hand-maintained duplicate type definitions.
- **NFR-406**: Local development dependencies (Postgres, Dex) MUST be runnable via
  `compose.dependencies.yaml`. No external service dependency may be added there without a
  corresponding in-memory trait implementation for the test suite (NFR-604).

## NFR-5xx — Privacy

- **NFR-501**: Poster and backdrop images are served to the browser as direct URLs pointing at the
  TMDB/AniList CDN — a deliberate, documented tradeoff in which third-party CDNs can observe which
  viewer requests which title's artwork
  ([ADR-0008](../architecture/decisions/ADR-0008-image-cdn-direct.md)). A server-side image proxy is
  deferred — tracked in [#70](https://github.com/justin13888/beam/issues/70).
- **NFR-502**: The server MUST NOT forward any first-party session cookie, auth header, or other
  Beam-specific credential to TMDB/AniList when resolving enrichment data or when the client fetches
  CDN image URLs directly.

## NFR-6xx — Extensibility

- **NFR-601**: The domain API MUST express itself entirely in domain terms (title id, file id,
  season/episode id, user id) and MUST NOT require a client to know or construct filesystem paths,
  internal database row identifiers unrelated to the domain, or web-framework-specific conventions,
  so that a native client (see [#78](https://github.com/justin13888/beam/issues/78)) can be built
  against the same API without server changes.
- **NFR-602**: Business logic in the service layer MUST remain isolated from web-framework types
  (HTTP requests/responses/extractors); such logic MUST be reachable and testable without going
  through an HTTP layer, preserving the option of a non-HTTP transport without a rewrite.
- **NFR-603**: The three delivery scenarios (FR-501–FR-506) MUST be modeled as domain concepts
  independent of any particular client's UI, so that a future client can implement its own source
  picker or download UI against the same underlying API contract.
- **NFR-604**: Introducing a new external service dependency into `compose.dependencies.yaml` MUST
  NOT occur without first providing an in-memory trait implementation usable by the test suite, so
  that future extensions do not erode zero-dependency testability (NFR-201).
