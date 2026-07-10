# Testing

## The zero-dependency mandate

`AGENTS.md` (checked into the repo root, symlinked as `CLAUDE.md`) states the project's testing
mandate directly:

> Unit tests must verify essential services end-to-end without spinning up external dependencies
> (e.g., Postgres, Docker Compose)... All tests must pass immediately using `cargo test --workspace`.
> They must NEVER require the services in `compose.dependencies.yaml` to be running.

This is restated as a normative requirement in `docs/requirements/non-functional.md` (NFR-201): the
full Rust workspace suite passes with zero external infrastructure running — no Postgres, Docker
Compose, or network access. It is a hard architectural invariant, enforced by construction (trait
boundaries at every external dependency) rather than by discipline alone, and it is what keeps the
suite fast enough to run on every save and every push without a compose stack.

## How the mandate is achieved

**Trait-based abstraction and dependency injection.** Every external boundary — database access
(`beam-entity`), filesystem I/O, and external network calls (the OIDC provider, TMDB/AniList via
`cameo`) — is abstracted behind a Rust trait (NFR-202). Services and handlers receive their
dependencies as `Arc<dyn Trait>` or generic bounds, never a concrete infrastructure type. The
service layer has no knowledge of web-framework request/response types (see
`docs/architecture/overview.md`), so it can be exercised directly in tests without a running HTTP
stack.

**Fakes over mocks.** For complex, stateful dependencies — a media/library repository, a session
store — the project prefers a robust, stateful `InMemory*` struct over a mocking framework
(NFR-203). These fakes hold real state across calls within a test, so a test can insert a record
through one trait method and assert its presence through another, exercising real state-machine
behavior rather than a canned sequence of expectations. `mockall` is reserved for simple, strict
contract verification — asserting that a method was called with specific arguments a specific
number of times — not for simulating multi-step state transitions.

**Subcutaneous end-to-end tests.** Core request-handling flows are tested by instantiating the real
in-process router/service graph with fakes wired in at the trait boundaries, then driving it with
programmatic HTTP requests (Salvo's `TestClient`) and asserting on both the HTTP response and the
resulting state mutation (NFR-204). This style — real routing, real handler code, real service
logic, fake infrastructure below the trait line — is deliberately favored over deep unit-level
mocking because it exercises the composition of components the way production traffic does, while
remaining hermetic. OIDC login/callback/logout, media browse/detail/sources, library CRUD, search,
playback-position updates, streaming Range handling, and admin actions are all covered this way.

**Edge cases as tests, not manual QA.** Any scenario that would otherwise require manual
verification — a missing file, a corrupted or unreadable media entry, a database-call failure, an
expired session, an unauthorized admin action — is codified as a unit test by configuring the
relevant injected trait to return the corresponding `Result::Err` (NFR-205). If a bug class can be
reached by making a fake return `Err` instead of `Ok`, it belongs in the test suite.

**Test data builders.** Domain entity test data is constructed via builder patterns and shared
fixture helpers in `#[cfg(test)]` modules (NFR-207), rather than ad hoc struct literals duplicated
across test files.

All Rust tests are inline `#[cfg(test)]` modules or sibling `*_tests.rs` files, run via plain
`cargo test --workspace` (or `cargo t-local` on hosts without system FFmpeg — see
[ADR-0007](architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md)). `beam-entity` and
`beam-migration` have no unit tests by design: they are pure data-shape/DDL layers with no
business logic.

## Web test stack

`beam-web` uses:

- **Vitest** as the test runner (`beam-web/vitest.config.ts`, jsdom environment).
- **Testing Library** (`@testing-library/react`, `@testing-library/user-event`,
  `@testing-library/jest-dom`) for component-level rendering and interaction assertions.
- **MSW** (Mock Service Worker) to intercept HTTP calls at the network layer, so component and hook
  tests exercise real client code (the generated `openapi-fetch` client) against a programmable
  fake HTTP layer rather than a real backend — the frontend equivalent of the Rust trait-fake
  pattern. Shared handlers live in `beam-web/src/test/`.

Coverage today concentrates on the pure modules (`src/hooks`, `src/lib`); the large route
components are mostly untested and need a React Testing Library + TanStack Router/Query harness
investment (reflected in the low enforced threshold below).

## What the hermetic layer deliberately does not cover

Some integrations cannot be exercised without a real external system. The strategy is to test
everything up to the trait boundary hermetically, and validate the real round-trip separately:

- **OIDC ↔ IdP round-trip.** The `beam-auth` client logic is unit-tested exhaustively against a
  `FakeOidcClient` — authorization URL construction, PKCE handling, callback/token exchange, JIT
  user provisioning, admin-allowlist evaluation. The real browser-driven Authorization Code + PKCE
  flow is exercised manually against the bundled Dex today; an automated Playwright suite is
  tracked in [#74](https://github.com/justin13888/beam/issues/74).
- **Real `cameo` → TMDB/AniList network calls.** The enrichment pipeline is unit-tested against its
  provider trait with a programmable fake, covering success, partial failure, retry/backoff, and
  missing-API-key behavior. Real network calls are only exercised by running the server against
  real credentials.
- **Real Postgres migration behavior and query semantics** (e.g. `pg_trgm` similarity search,
  actual index usage). Repository logic is unit-tested against `InMemory*` fakes implementing the
  same trait contract; the migrations themselves run against real Postgres at server startup
  (`BEAM_AUTO_MIGRATE`, see `docs/operations/deployment.md`) or via the `beam-migration` CLI.

This is a deliberate, accepted boundary: the unit suite guarantees "business logic behaves
correctly given any trait response"; only a real system can guarantee "the real external system
behaves the way our fakes assume."

## Coverage & CI gates

| Suite | Tool | Threshold | Enforced by |
|---|---|---|---|
| Rust workspace (lines) | `cargo-llvm-cov` | 65% | `cargo llvm-cov --workspace --fail-under-lines 65` in `.github/workflows/rust.yml` |
| Web (`beam-web`) | `@vitest/coverage-v8` | lines 12%, functions 10%, branches 3%, statements 12% | `coverage.thresholds` in `beam-web/vitest.config.ts` |

Run locally with `cargo llvm-cov --workspace --lcov --output-path lcov.info` (add
`--features beam-index/vendored-ffmpeg,beam-server/vendored-ffmpeg` on hosts without system
FFmpeg) and `bun run test:coverage` in `beam-web`. Reports are CI artifacts; the threshold flags
are the actual gate — there is no external coverage service.

The web thresholds are an honest, intentionally low baseline: `coverage.include` counts every
matching source file in the denominator (not just files a test imports), and the untested route
components dominate that denominator.

**Ratchet, don't relax.** Raise thresholds as the suites mature; never lower them to make a PR
pass. If a PR fails the coverage gate, add the missing tests (or isolate genuinely untestable
code). Threshold changes are a deliberate, reviewed decision tied to an actual improvement in the
suite.

A line-coverage percentage says a line executed; it says nothing about assertion quality, and
nothing about the integrations deliberately excluded from the hermetic layer above. Treat the gate
as a floor against undertested code, not as validation of the real external round-trips.
