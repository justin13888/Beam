# Testing Strategy

Status: describes the testing philosophy for this push and its current baseline. Where noted, test
inventory numbers are a snapshot that will grow as this push's new features (OIDC auth, in-process
indexing, enrichment, REST API) land.

## The zero-dependency mandate

`AGENTS.md` (checked into the repo root, symlinked as `CLAUDE.md`) states the project's testing
mandate directly:

> Unit tests must verify essential services end-to-end without spinning up external dependencies
> (e.g., Postgres, Docker Compose)... All tests must pass immediately using `cargo test --workspace`.
> They must NEVER require the services in `compose.dependencies.yaml` to be running.

This is restated as a normative requirement in `docs/requirements/non-functional.md` (NFR-201): the
full Rust workspace suite must pass with zero external infrastructure running — no Postgres, Redis,
Docker Compose, or network access. This is treated as a hard architectural invariant, not an
aspiration: it is enforced by construction (trait boundaries at every external dependency) rather
than by discipline alone, and it is what keeps the suite fast enough to run on every save and every
push without a compose stack.

## How the mandate is achieved

**Trait-based abstraction and dependency injection.** Every external boundary — database access
(`beam-entity`), filesystem I/O, and external network calls (the OIDC provider, TMDB/AniList via
`cameo`) — is abstracted behind a Rust trait (NFR-202). Services and handlers receive their
dependencies as `Arc<dyn Trait>` or generic bounds, never a concrete infrastructure type. This is the
same rule that makes the architecture testable and the same rule that keeps the service layer
decoupled from web-framework types (see `docs/architecture/overview.md`): the service layer has no
knowledge of Salvo/Axum request or response types, so it can be exercised directly in tests without a
running HTTP stack.

**Fakes over mocks.** For complex, stateful dependencies — a media/library repository, a session
store — the project prefers a robust, stateful `InMemory*` struct (e.g. `InMemoryMediaRepository`)
over a mocking framework (NFR-203). These fakes hold real state across calls within a test, so a test
can, for example, insert a record through one trait method and assert its presence through another,
exercising real state-machine behavior rather than a canned sequence of expectations. `mockall` is
reserved for simple, strict contract verification — asserting that a method was called with specific
arguments a specific number of times — not for simulating multi-step state transitions.

**Subcutaneous end-to-end tests.** Rather than testing units in isolation, core request-handling
flows are tested by instantiating a real in-process application router/service graph with fakes
wired in at the trait boundaries, then driving it with programmatic HTTP requests (via Axum's test
helpers) and asserting on both the HTTP response and the resulting state mutation (NFR-204). This
"subcutaneous" style — real routing, real handler code, real service logic, fake infrastructure below
the trait line — is deliberately favored over deep unit-level mocking, because it exercises the
composition of components the way production traffic does, while remaining hermetic. This push's new
flows that require this treatment include: OIDC login/callback/logout, library CRUD, search,
playback-position updates, and admin actions (NFR-204).

**Edge cases as tests, not manual QA.** Any scenario that would otherwise require manual verification
— a missing file, a corrupted or unreadable media entry, a database-call failure, an expired session,
an unauthorized admin action — is codified as a unit test by configuring the relevant injected trait
to return the corresponding `Result::Err` (NFR-205), rather than left as a runbook step. If a bug
class can be reached by making a fake return `Err` instead of `Ok`, it belongs in the test suite.

**Test data builders.** Domain entity test data is constructed via builder patterns defined in
`#[cfg(test)]` modules (NFR-207), rather than duplicated ad hoc struct literals scattered across test
files. This keeps fixture construction consistent across suites and keeps tests resilient to
non-semantic field additions on domain structs.

## What this hermetic layer deliberately does not cover

Some integrations cannot be exercised end-to-end without a real external system, by definition. The
strategy here is not to skip testing them — it is to test everything up to the trait boundary
hermetically, and to validate the real round-trip through a separate, explicitly manual process
rather than pretending an automated hermetic test can stand in for it:

- **OIDC ↔ Dex round-trip.** The `beam-auth` OIDC client is unit-tested exhaustively against a
  `FakeOidcClient` that never touches a network — authorization URL construction, PKCE parameter
  handling, callback/token exchange logic, JIT user provisioning, and admin-allowlist evaluation are
  all covered this way. The real browser-driven Authorization Code + PKCE flow against a real Dex
  instance is validated manually, per the runbook in `docs/operations/e2e-validation.md`.
- **Real `cameo` → TMDB/AniList network calls.** The enrichment pipeline's use of `cameo` is
  unit-tested against the pipeline's own provider trait and a programmable fake, covering success,
  partial-failure, retry/backoff, and missing-API-key behavior (FR-304, FR-306, FR-307). Real network
  calls to TMDB and AniList are exercised manually as part of the e2e runbook using fixture titles
  chosen to hit both a keyed (TMDB) and keyless (AniList) code path.
- **Real Postgres migration behavior and query semantics** (e.g., `pg_trgm` similarity search
  behavior, actual index usage). Repository logic is unit-tested against `InMemory*` fakes that
  implement the same trait contract; the migrations themselves and any Postgres-specific query
  behavior are validated by actually running the migrations against the compose-provided Postgres
  instance, which is a manual/CI-adjacent step rather than part of `cargo test --workspace`.

This is a deliberate, accepted boundary, not an oversight: it keeps the unit suite's guarantee
precise ("business logic behaves correctly given any trait response") while keeping a separate,
clearly-named process responsible for the guarantee that only a real system can give ("the real
external system actually behaves the way our fake assumes it does"). See
`docs/operations/e2e-validation.md` for that process.

## Current test inventory (baseline snapshot)

As of this push's starting point, before its new features land, the Rust workspace has roughly 202
test functions:

| Crate | Test functions | Notes |
|---|---|---|
| `beam-auth` | 42 | |
| `beam-domain` | 5 | |
| `beam-index` | 63 | |
| `beam-stream` | 92 | |
| `beam-entity` | 0 | Pure sea-orm entity/data-shape layer, no business logic to unit test — acceptable by design. |
| `beam-migration` | 0 | Pure sea-orm-migration schema DDL, no business logic to unit test — acceptable by design. |

All of these are inline `#[cfg(test)]` modules or sibling `*_tests.rs` files — there are no
`tests/` integration directories and no fixtures directories today. Every test runs via plain
`cargo test --workspace`, with no Postgres, Redis, or Docker required.

This number is expected to grow substantially through this push: new OIDC auth flows, in-process
indexing (absorbing what is today a separate `beam-index` gRPC process), the enrichment pipeline, and
the REST API surface (replacing GraphQL) all bring new subcutaneous end-to-end tests per NFR-204, plus
edge-case tests per NFR-205. Track actual counts in `docs/testing/coverage.md` rather than treating the
numbers above as a target ceiling.

## Web test stack

`beam-web` currently has 3 Vitest test files (an auth hook, an API client smoke test, and a login
route), using:

- **Vitest** as the test runner, configured in `beam-web/vitest.config.ts` (jsdom environment).
- **Testing Library** (`@testing-library/react`, `@testing-library/user-event`,
  `@testing-library/jest-dom`) for component-level rendering and interaction assertions.
- **MSW** (Mock Service Worker) to intercept HTTP calls at the network layer, so component and hook
  tests exercise real client code (the generated `openapi-fetch` client) against a programmable fake
  HTTP layer rather than a real backend — the frontend equivalent of the Rust trait-fake pattern.

This is a thin test surface today. It is expected to grow substantially through this push as new
REST-backed routes and features (OIDC login flow, library browsing, admin area, player, search) are
added, each needing its own MSW-backed test coverage. See `docs/testing/coverage.md` for the
enforced coverage threshold and how to measure it locally.
