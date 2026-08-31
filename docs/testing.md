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
programmatic HTTP requests through the active framework's in-process test client and asserting on
both the HTTP response and the resulting state mutation (NFR-204). This style — real routing, real
handler code, real service logic, fake infrastructure below the trait line — is deliberately
favored over deep unit-level mocking because it exercises the composition of components the way
production traffic does, while remaining hermetic. OIDC login/callback/logout, media
browse/detail/sources, library CRUD, search,
playback-position updates, streaming Range handling, and admin actions are all covered this way.

**Edge cases as tests, not manual QA.** Any scenario that would otherwise require manual
verification — a missing file, a corrupted or unreadable media entry, a database-call failure, an
expired session, an unauthorized admin action — is codified as a unit test by configuring the
relevant injected trait to return the corresponding `Result::Err` (NFR-205). If a bug class can be
reached by making a fake return `Err` instead of `Ok`, it belongs in the test suite.

**Test data builders.** Domain entity test data is constructed via builder patterns and shared
fixture helpers under the `test-utils` feature (NFR-207), rather than ad hoc struct literals
duplicated across test files. `ServerConfig` implements `Default` derived from its own
`#[config(default = ...)]` attributes, so a test writes only the fields it cares about
(`ServerConfig { video_dir, ..Default::default() }`) and adding a configuration field does not
touch a single test.

**Shared repository contracts.** Each repository/store trait has one behavioural contract, written
purely against the trait, and instantiated over every implementation of it — the in-memory double
(hermetic, always run) and the SQL implementation against a real Postgres (`pg-integration`). This
is the one legitimate exception to "never test the double": the same assertions constrain both, so
a divergence between the fake and production fails instead of drifting. Writing the first two of
these immediately found two such divergences — an in-memory session store that listed expired
sessions the SQL one filtered out, and one that echoed the caller's `created_at` back instead of
stamping its own. The contracts live beside the traits they constrain, in
`beam-domain/src/repositories/contract.rs` and `beam-auth/src/utils/contract.rs`.

**Time is injected, never slept through.** Every store and service that stamps or compares a
timestamp takes a `Clock` (`beam-domain/src/services/clock.rs`). One trait covers both time bases:
wall-clock `now()` for persisted rows and rescan cadence, monotonic `monotonic()` for elapsed
intervals in the rate limiter. A test advances a `TestClock`; nothing in the suite sleeps. That is
what makes a 14-day session idle-expiry or a one-hour rescan interval testable at all.

All Rust tests are inline `#[cfg(test)]` modules or sibling `*_tests.rs` files, run via `mise run
rust:test`, which wraps `cargo test --workspace` and vendors FFmpeg on hosts without the system
development libraries (see
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

Coverage spans the pure modules (`src/hooks`, `src/lib`), the components, and the route components,
which are exercised through the shared harness in `beam-web/src/test/harness.tsx`. `renderRoute`
mounts the **generated route tree** over `createMemoryHistory` with a real `AuthProvider` and
`QueryClientProvider`, so `beforeLoad` guards, `validateSearch` parsers, loaders and
`errorComponent` wiring all execute. Requests are asserted at the wire (`src/test/requests.ts`
records what MSW actually saw) rather than against a mocked client's call arguments, and every
handler and factory is typed against `components["schemas"]` from the generated client.

Nothing mocks `@tanstack/react-router` or `@/lib/apiClient`. Two collaborators are still
substituted, both for environment reasons rather than convenience: Vidstack's `<MediaPlayer>`,
which needs a real media element (its own logic is extracted into `playerHandlers` and tested
directly), and `useAdminEventStream`, which needs `EventSource` (tested on its own).

Getting there required a production fix worth recording: `openapi-fetch` captures `globalThis.fetch`
when the module is first imported, which is before any test interceptor installs itself — so MSW
could never see a request the app made, and that is *why* every test had mocked the client away.
`apiClient` now resolves `fetch` per call.

## What a real test asserts

A line-coverage percentage says a line executed. It says nothing about whether anything *checked*
what the line did. The patterns below all produce coverage and prove nothing; each was found in this
repository and removed, and `AGENTS.md` states them as rules for new code.

- **Testing the double.** A test whose subject is an `InMemory*`, `Fake*`, `Stub*`, `Noop*`, `Mock*`
  or `TestClock` asserts the behaviour of scaffolding. Configuring `InMemoryPathValidator` to return
  "path escapes root" and then asserting that error comes back is a tautology -- it says nothing
  about `OsPathValidator`, which does the real containment check. The legitimate form is a **shared
  contract test** run over both the fake and the real implementation, so one suite constrains both.
- **Restating a constant.** Asserting that a declared default equals itself, or that a body which is
  literally `Ok(())` is `is_ok()`.
- **Mirroring the implementation.** A test that is a second hand-maintained copy of a table in the
  source drifts in lockstep with it. Derive one from the other, or assert the invariant connecting
  them.
- **Mocking away the subject.** A test that mocks the module it is named for is not a test of that
  module.
- **Sleeping for ordering.** A real `sleep` to force two timestamps apart is flaky under load and
  hides the `Clock` seam that already exists.

## Mutation testing

Coverage tells you where a test *cannot* help -- uncovered code. Mutation testing tells you where
coverage lied: it changes the program in small, plausible ways (a comparison flipped, a branch
deleted, a return value replaced with a default) and reports which changes the suite failed to
notice. A mutant that survives at 100% line coverage is, by definition, an assertion gap.

The project uses [cargo-mutants](https://mutants.rs), pinned in `mise.toml`, configured in
`.cargo/mutants.toml` (that exact path -- a `mutants.toml` at the repository root is silently
ignored).

**Feature selection matters more than it looks.** cargo-mutants builds each package on its own, so
the feature name it is given has to be unqualified and valid for every mutated package -- a
workspace-qualified `beam-index/vendored-ffmpeg` fails the baseline build for a package that does
not depend on beam-index. Each of the four mutated crates therefore declares a `mutation-testing`
feature naming the set that compiles *all* of its `cfg`-gated production paths, and
`BEAM_MUTANTS_FEATURES` passes that one name. Without it, mutants in (for example)
`cfg(feature = "entity")` code come back MISSED because the build never compiled them -- a result
that says nothing about the tests.

**It is advisory and never a merge gate.** `.github/workflows/mutants.yml` runs it over the changed
code on every pull request and over the whole workspace on a schedule, both `continue-on-error`;
`ci-ok` does not depend on either. A surviving mutant is a prompt to improve the code, not a broken
build.

### Resolving a surviving mutant

The order matters, and it is deliberately not "write a test":

1. **Is the branch reachable and meaningful?** If not, delete it. Dead defensive code is the most
   common cause of a survivor, and deleting it is strictly better than testing it.
2. **Can a type remove the check?** A `-> u32 with 0` survivor on a limit or an offset usually wants
   a `NonZeroU32` or a newtype. Making the illegal state unrepresentable removes the mutant *and*
   the class of bug, where a test only detects one instance of it.
3. **Is it real, load-bearing behaviour?** Then write the test.
4. **Is the mutant genuinely equivalent** -- indistinguishable in observable behaviour? Mark it
   `#[mutants::skip]` with a comment explaining *why it is equivalent*, not that it is hard to test,
   and record it in ADR-0011's decision log so the exemptions stay countable and reviewable.

Between mutation runs (which cost hours) use coverage as the cheap proxy (which costs minutes): run
`mise run rust:coverage:report` and watch the **region** column. If a batch of new tests does not
move it, those tests are re-covering already-covered lines and will not kill anything -- a free early
exit from a bad approach.

### Test doubles must be marked `#[mutants::skip]`

cargo-mutants decides what to leave alone by looking for the *literal* attribute `#[cfg(test)]`.
Against `#[cfg(any(test, feature = "test-utils"))]` its parse of the nested meta fails, it takes the
error branch, and the item is **not** excluded. Because the `test` arm of that cfg is active under
`cargo test`, the resulting mutants compile and run: they are viable, they fill the report with
survivors that describe scaffolding, and the ones that do get killed inflate the caught rate with
kills that prove nothing about the product.

A name-based `exclude_re` cannot fix this safely -- the doubles are interleaved with real logic in
the same files, and this workspace has had un-gated production types whose names begin with
`InMemory`. Silently exempting real code is the one failure mode a mutation setup must not have.

So the exclusion is explicit: test-utils-gated code lives in a single `pub mod in_memory` (or
`fake`, or `test_utils`) block carrying `#[mutants::skip]` immediately above the `cfg` attribute, and
is re-exported at the module root where callers need the original path. `use` items are exempt --
attribute macros are not accepted on them, and re-exports generate no mutants.
`mise run check:mutants-skip-fakes` enforces this, runs in `mise run ci`, and is in the `pre-push`
hook.

### The recorded baseline

Measured on the tree that introduced this loop. The first number is what the suite scored before
any hardening; the second is after one pass per crate.

| Crate | Mutants | Survivors before | Survivors after |
|---|---|---|---|
| `beam-domain` | 94 | 21 | 0 |
| `beam-auth` | 149 | 52 | 0 |
| `beam-server` | 392 | 17 | 0 |
| `beam-index` | 704 | 13 | 0 |

Roughly a fifth of all mutants are `unviable` (the mutated program does not compile), which is
normal and not a signal about the tests.

### Running it

```
mise run rust:mutants:list                     # free: how many mutants, and where
mise run rust:mutants:crate beam-domain        # the loop; --iterate for later passes
mise run rust:mutants:diff                     # just what this branch changed
mise run rust:mutants:shard 0/12               # one slice of the full run
```

`mutants.out/` holds the results: `missed.txt` is the work queue, `caught.txt` the wins,
`timeout.txt` either a genuinely unbounded loop the mutation exposed or a broken timeout setting
(resolve these first -- a bad timeout corrupts every other result), and `outcomes.json` the
machine-readable form, with the exact source change for each mutant under `diff/`.

## The `pg-integration` tier

The hermetic suite covers "the business logic behaves correctly given any trait response". It cannot
cover "the SQL we generate means what we think it means", because the fakes and the real repositories
only have to agree on the trait signature.

Two mechanisms close that:

- **`sea_orm::MockDatabase`** asserts the SQL a repository actually generates -- which column a
  filter binds and to what value, sort direction, pagination numbers, the `ON CONFLICT` target and
  its update list -- with no database running. Hermetic, and part of the default suite
  (`beam-index/src/repositories/sql_shape_tests.rs`). These assert *properties*, never a
  full statement string: pinning the whole generated SQL is a second copy of the query builder's
  output, which fails on harmless formatting changes and catches nothing a property does not.
- **The `pg-integration` cargo feature** runs the shared behavioural contract against a real
  Postgres, and covers what only a real engine can answer: `ON CONFLICT` atomicity under
  concurrency, foreign keys and unique indexes, `DELETE ... RETURNING` single-use semantics,
  `pg_trgm`, index usage, and migration up/down.

`MockDatabase` required one structural change: enabling sea-orm's `mock` feature removes `Clone`
from `DatabaseConnection` for the whole build graph, so every repository now holds an
`Arc<DatabaseConnection>` and the pool is wrapped once at the process entry point. That is a better
shape anyway — one pool handle, shared explicitly, rather than an enum cloned per repository.

`pg-integration` is **off by default**. `cargo test --workspace`, `mise run rust:test`,
`mise run rust:coverage`, `mise run ci` and the `pre-push` hook must never enable it -- NFR-201 is
unconditional. Run it deliberately:

```
docker compose -f compose.dependencies.yaml up -d
BEAM_TEST_DATABASE_URL=postgres://beam:password@localhost:5432/beam mise run rust:test:pg
```

With the variable unset the tier fails loudly rather than skipping: a tier that silently passes
when it did not run is worse than one that does not run. The harness lives in `beam-test-support`,
a crate that is a dev-dependency of the crates owning repositories and is depended on by nothing in
production; with the feature off it compiles to an empty library.

**What the tier has already caught.** `files.file_status` is a Postgres `enum` type while
`beam_entity::files::Model` declared the field as a bare `String`. sea-orm bound the parameter as
`text`, which Postgres refuses to assign to an enum column, so **every file insert and update
failed at runtime** — while every hermetic test passed, because neither an in-memory `HashMap` nor
`MockDatabase` type-checks a parameter. The entity now models it as a `DeriveActiveEnum`.

## Contract testing

The frontend consumes a client generated from the server's OpenAPI document, so the compiler checks
that call sites match the spec. Two gaps remain, and both are closed by tests rather than by types:

- **The router and the spec can disagree.** A route can be registered and never documented, or
  documented and never registered. A Rust test asserts every route in `create_router` appears exactly
  once in the generated document, and the reverse. This is also a ratified ADR-0010 readiness
  criterion (`docs/architecture/kynos-migration-readiness.md`).
- **Test doubles can drift from the spec.** Mocked responses written as bare object literals are
  checked by nothing. The web suite's MSW handlers and factories are typed against
  `components["schemas"]` from the generated client, so a response shape that no longer matches the
  spec fails type-checking. This caught a stale field the first time it was applied.
- **The spec itself can change without anyone noticing.** `beam-web/openapi.json` was gitignored
  and regenerated on every CI run, so a breaking wire change left no trace in review. It is
  committed, and `mise run codegen:openapi:check` (part of `mise run ci`) fails when the spec the
  router generates differs from the committed one. Drift is now a reviewable diff.

Both router-facing contract tests derive the route table from `create_docs_router` itself, by
reading the router's own `Debug` tree — not from a list restated in the test. A third asserts that
every registered route has a bounded metrics class, which replaced a hand-maintained second copy of
the route table in `metrics_mw_tests.rs` that could not fail when a route was added.

## What the hermetic layer deliberately does not cover

Some integrations cannot be exercised without a real external system. The strategy is to test
everything up to the trait boundary hermetically, and validate the real round-trip separately:

- **OIDC ↔ IdP round-trip.** The `beam-auth` client logic is unit-tested exhaustively against a
  `FakeOidcClient` — authorization URL construction, PKCE handling, callback/token exchange, JIT
  user provisioning, admin-claim evaluation. The real browser-driven Authorization Code + PKCE
  flow is exercised manually against the bundled Dex today; an automated Playwright suite is
  tracked in [#74](https://github.com/justin13888/beam/issues/74).
- **Real `cameo` → TMDB/AniList network calls.** The enrichment pipeline is unit-tested against its
  provider trait with a programmable fake, covering success, partial failure, retry/backoff, and
  missing-API-key behavior. Real network calls are only exercised by running the server against
  real credentials.
- **Semantics only a real Postgres has** -- `pg_trgm` similarity ranking, actual index usage, and
  migration up/down behaviour. These are covered, but by the opt-in `pg-integration` tier rather
  than the hermetic suite; see "The `pg-integration` tier" below. What remains outside every
  automated suite is the behaviour of a *specific production* database: its data volume, its
  planner statistics, and its version-specific quirks.

- **Hardware video decoding.** `beam-client-core`'s capability matching is table-tested against
  synthetic `DeviceProfile`s, and the Android suite runs on the JVM, so no test in CI ever decodes
  a frame. Whether a device's HEVC, AV1, HDR or passthrough audio decoder behaves as its
  `MediaCodecList` entry advertises can only be established on real hardware. This is the sharpest
  edge of the boundary: under direct play ([ADR-0004](architecture/decisions/ADR-0004-never-transcode.md))
  a wrong capability verdict is the difference between a title playing and a green screen, and the
  emulator has software decoders only.
- **The Android UI on a device.** Robolectric renders real pixels on the JVM, which is enough to
  catch a layout or colour regression, but not to catch a surface-lifecycle bug, a
  picture-in-picture transition, or a media session that misbehaves against a real notification
  shade.
- **The Apple sample-buffer engine on real hardware.** `beam-apple`'s `SampleBufferEngine` demuxes
  Matroska in the core and feeds VideoToolbox itself ([ADR-0013](architecture/decisions/ADR-0013-apple-client-two-engines.md)),
  so it owns A/V synchronisation, seek accuracy and decoder configuration that `AVPlayer` would
  otherwise own. The suite covers the parts that can be: the demuxer against committed real
  Matroska files, and `avcC`/`hvcC` parsing against the records those files actually carry. What no
  simulator can establish is whether a hardware decoder, driven by buffers we built, stays in step
  with its audio over an hour -- the same edge as Android's, arrived at from the other side.
- **Dark mode in the Apple snapshot tier.** The references are light mode only, because
  `glassEffect` does not render its material offscreen and resolves content colours against the
  light appearance regardless of the host's `overrideUserInterfaceStyle`. A recorded dark reference
  would be black text on a black ground -- a picture of a renderer limitation that would then pass
  forever whether or not dark mode worked. Dark mode is checked on a device.

This is a deliberate, accepted boundary: the unit suite guarantees "business logic behaves
correctly given any trait response"; only a real system can guarantee "the real external system
behaves the way our fakes assume."

## Coverage & CI gates

| Suite | Tool | Threshold | Enforced by |
|---|---|---|---|
| Rust workspace | `cargo-llvm-cov` | lines 81%, regions 81%, functions 73% | the `rust:coverage` task in `mise.toml`, run by the `rust-test` job in `.github/workflows/ci.yml` |
| Web (`beam-web`) | `@vitest/coverage-v8` | lines 79%, functions 72%, branches 70%, statements 76% | `coverage.thresholds` in `beam-web/vitest.config.ts` |
| Android (`beam-android`) | JUnit + Robolectric | no percentage gate; the suite must pass | the `android:test` task in `mise.toml`, run by the `android-test` job |
| Android screenshots | Roborazzi | every reference must match | the `android:screenshot` task, run by the `android-screenshot` job |
| Apple (`beam-apple`) | swift-testing | no percentage gate; the suite must pass on macOS and the iOS simulator | the `apple:test` task in `mise.toml`, run by the `apple-test` job |
| Apple snapshots | swift-snapshot-testing | every reference must match, on one pinned simulator and Xcode | the `apple:snapshot` task, run by the `apple-snapshot` job |

**Regions, not branches.** `cargo-llvm-cov`'s `--branch` requires
`-Z coverage-options=branch` and therefore a nightly toolchain, and
`rust-toolchain.toml` pins stable 1.91.0. LLVM *regions* are a superset of branches -- every match
arm and every `&&` / `||` short-circuit carries its own counter -- so `--fail-under-regions` is the
honest stable-toolchain branch gate, and it is the number that tracks mutation score. Measure with
`mise run rust:coverage:report`, which prints the per-file Regions / Functions / Lines table without
enforcing anything.

`beam-client-core` is part of the Rust workspace and counts towards the gate above, with one
exclusion: spargen's generated client is emitted into `OUT_DIR` and is excluded by
`--ignore-filename-regex`, since it is a generator's output rather than hand-written logic. The
crate's own logic — capability matching, up-next, paging, the progress throttle — is pure and
exhaustively table-testable, so it raises the workspace number rather than straining the gate.

The Android suite has no percentage threshold, deliberately. A Compose codebase is mostly
declarative UI, and a line-coverage denominator counting every composable rewards tests that
instantiate screens without asserting anything about them. The gate is that the behavioural tests
and the screenshot references both pass; screenshot diffs upload as CI artifacts on failure,
because a failed screenshot check without the diff tells a reviewer only that something moved.

Run locally with `mise run rust:coverage`, `mise run ts:coverage`, `mise run android:test` and
`mise run android:screenshot`. Both use the same commands CI
does, and the Rust one vendors FFmpeg by default on hosts without the system development libraries
(see `BEAM_CARGO_FEATURES` in `mise.toml` and
[ADR-0007](architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md)). Reports are CI artifacts;
the threshold flags are the actual gate — there is no external coverage service.

`coverage.include` counts every matching source file in the denominator, not just the files a test
imports, so these numbers describe the whole SPA rather than the tested slice of it.

**Ratchet, don't relax.** Raise thresholds as the suites mature; never lower them to make a PR
pass. If a PR fails the coverage gate, add the missing tests (or isolate genuinely untestable
code). Threshold changes are a deliberate, reviewed decision tied to an actual improvement in the
suite.

A line-coverage percentage says a line executed; it says nothing about assertion quality, and
nothing about the integrations deliberately excluded from the hermetic layer above. Treat the gate
as a floor against undertested code, not as validation of the real external round-trips -- assertion
quality is what the mutation-testing loop measures, and the real round-trips are what the
`pg-integration` tier and manual OIDC verification cover.
