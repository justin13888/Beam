# Beam

## Context & Persona
You are an expert software engineer working on `beam`, a media management server. The project is a multi-language monorepo.

Your primary goal is to write highly performant, robust code while strictly adhering to architecture patterns that allow for offline, dependency-free testing.

## Testing strategy

The whole strategy rests on one invariant: **`cargo test --workspace` passes with zero
infrastructure** -- no Postgres, no Docker Compose, no network (NFR-201). Everything below exists to
make that possible without making the tests worthless. `docs/testing.md` is the long form.

### The four pillars

* **Trait-Based Abstraction:** All external boundaries -- database access (`beam-entity`), file
  system I/O, external APIs -- MUST be abstracted behind Rust traits. Never tightly couple business
  logic to concrete infrastructure implementations.
* **Dependency Injection:** Pass dependencies (via generic bounds or `Arc<dyn Trait>`) into services
  and handlers.
* **Domain Isolation:** Isolate core media management and streaming logic from web framework types.
  Your service layer should not know about HTTP requests, responses, or extractors.
* **Fakes over Mocks:** Prefer stateful `InMemory*` structs over a mocking framework for data stores,
  so a test can insert through one trait method and assert through another. Use `mockall` only for
  simple, strict contract verification -- never to simulate multi-step state.

### What a real test asserts

A test earns its place only if it can fail for a reason that matters. These patterns cannot, and are
forbidden -- each one was found in this repo and removed:

* **Never test the double.** An `InMemory*`, `Fake*`, `Stub*`, `Noop*`, `Mock*` or `TestClock` is
  scaffolding, never the subject. `InMemoryPathValidator::validation_error(...)` returning the error
  you configured proves nothing about `OsPathValidator`. If a fake seems to need its own test, the
  fake is too complex. The one legitimate exception is a **shared contract test** run over both the
  fake and the real implementation -- that constrains both at once.
* **Never restate a constant.** No asserting that `#[config(default = X)]` yields `X`, that a body
  which is literally `Ok(())` is `is_ok()`, or that a literal in the test equals the same literal in
  the source.
* **Never mirror the implementation.** A test that is a second hand-maintained copy of a table in the
  source drifts in lockstep with it and catches nothing. Derive one from the other, or assert the
  invariant that connects them.
* **Never mock away the subject.** If a test mocks the module it is named for, it is not a test of
  that module.
* **Never sleep for ordering.** Injecting `Clock` is always available and always correct; a
  wall-clock sleep is flaky under load and hides the seam.

### Canonical seams

Reach for the existing seam; do not invent a second one for the same concern.

| Concern | Seam |
|---|---|
| Time (wall clock *and* monotonic) | `beam_domain::services::Clock` (`RealClock` / `TestClock`) |
| Identifiers | `beam_domain::services::IdGenerator` (`UuidGenerator` / `SequentialIdGenerator`) |
| Filesystem | a real `TempDir` -- see below |
| Persistence | the per-aggregate repository traits in `beam-domain` |
| Identity provider | `OidcClient` (`FakeOidcClient`) |
| Metadata providers | `EnrichmentProvider` |
| Library path safety | `PathValidator` |
| Health checks | `DependencyProbe` |
| Media probing | `MediaInfoService` |
| Background indexing | `BackgroundIndexer` (`beam-index/src/runtime.rs`) |

There is deliberately **no `FileSystem` trait**. A `TempDir` is a real filesystem that needs no
infrastructure, so filesystem code is tested against actual files -- a fake would only prove the
fake returns what it was configured to return, which is the first forbidden pattern above. Where an
error path cannot be produced with a temp directory, inject the narrow trait for *that* operation
rather than abstracting the filesystem wholesale.

Every repository and store trait has one **shared behavioural contract** (`contract.rs` beside the
trait), instantiated over the in-memory double and -- under `pg-integration` -- over the SQL
implementation. New implementations of an existing trait bind the contract; they do not get their
own bespoke tests.

### Choosing a technique

| Use | When |
|---|---|
| Subcutaneous `kynos::test::TestClient` | Default for any request-handling flow. Real router, real handlers, real services, fakes below the trait line. Assert the response **and** the state mutation. |
| Table-driven unit test | A pure function whose interesting cases are enumerable. |
| `proptest` | A pure function with an algebraic invariant (parsing never panics, a slice never exceeds its source, an operation is idempotent). |
| `sea_orm::MockDatabase` | A repository's generated SQL -- which column a filter binds and to what, sort direction, pagination, `ON CONFLICT` target. Assert *properties*, never a whole statement string. Hermetic. |
| `pg-integration` feature | Only semantics a real Postgres has: `ON CONFLICT` atomicity, foreign keys, `pg_trgm`, index usage, migration up/down. Opt-in (`mise run rust:test:pg`), never in the default run. |
| `renderRoute` (`beam-web/src/test/harness.tsx`) | Any web route or component. Real memory router, real auth, MSW at the wire. Never `vi.mock` the router or the API client -- that stops testing the thing under test. |
| MSW + `recordRequests()` | Assert what the app actually put on the wire, not what a mocked client was called with. |
| `mockall` | Strict call-contract verification only. |
| Test data builders | Always, from the `test-utils` builders beside each domain type, instead of ad-hoc struct literals. |

### Edge cases are tests, not manual QA

Any scenario that would otherwise be verified by hand -- missing file, corrupted media, database
failure, expired session, unauthorized admin action -- is codified by configuring the injected trait
to return the relevant `Result::Err` (NFR-205).

### Test doubles must be marked `#[mutants::skip]`

cargo-mutants recognises only the *literal* `#[cfg(test)]`. It parses
`#[cfg(any(test, feature = "test-utils"))]` as an unrecognised form and mutates the contents, so
without an explicit skip the doubles are mutated as if they were product code -- filling the report
with meaningless survivors and inflating the caught rate with kills that prove nothing.

So: test-utils-gated code lives in one `pub mod in_memory` (or `fake`/`test_utils`) block carrying
`#[mutants::skip]` immediately above the `cfg` attribute, re-exported at the module root if callers
need the old path. `mise run check:mutants-skip-fakes` enforces this and runs in `mise run ci`.

### The hardening loop

Mutation testing is **advisory** -- it never blocks a merge. A surviving mutant is a prompt to
improve the code, and the resolution order is deliberate:

1. **Is the branch reachable and meaningful?** If not, delete it.
2. **Can a type remove the check?** A `-> u32 with 0` survivor on a limit or offset usually wants a
   `NonZeroU32` or a newtype, not a test. Prefer making the illegal state unrepresentable.
3. **Is it real, load-bearing behaviour?** Then write the test.
4. **Is the mutant genuinely equivalent?** `#[mutants::skip]` with a comment saying *why it is
   equivalent* -- not "hard to test" -- and a line in ADR-0011's decision log.

Between mutation runs use coverage as the cheap proxy: `mise run rust:coverage:report`, and watch the
**region** column. If a batch of new tests does not move it, those tests are re-covering
already-covered lines and will not kill anything.

```
mise run rust:mutants:list              # free: how many mutants exist, and where
mise run rust:mutants:crate beam-domain # the loop; add --iterate for later passes
mise run rust:coverage:report           # the between-runs proxy
```

## Rust Styling
- Prefer more verbose, explicit patterns if it avoids refactoring bugs (e.g., destructure if almost all struct fields are being used.)

## Workflow Rules
1. Before modifying database schema, check `beam-migration` and `beam-entity`.
2. Do not add new external service dependencies to `compose.dependencies.yaml` without explicitly providing an in-memory trait implementation for the test suite first.
3. **A gap in `kynos` or `spargen` blocks and is fixed upstream, never worked around locally.** Both are first-party
   ([getkono/kynos](https://github.com/getkono/kynos), [getkono/spargen](https://github.com/getkono/spargen)) and both
   sit on the same seam: they derive the served routes, the OpenAPI document, and every generated client from one
   declaration. A local exception -- a hand-written response, a second router, a patched schema, an `unchecked` waiver --
   is exactly how the document stops describing the server, which is the failure [ADR-0010](docs/architecture/decisions/ADR-0010-openapi-3-2-kynos.md)
   and [ADR-0012](docs/architecture/decisions/ADR-0012-native-client-rust-core.md) exist to prevent. File the issue
   upstream, then take the fix from a published release: Beam tracks both as crates.io version requirements, never
   git revisions. Until a release carries it, record the gap in a comment naming the issue.

   Using a different *supported* API is not a workaround. When `kynos` accepted a route-level `tag = ...` and silently
   dropped it, the fix was to declare the tags on group scopes -- where Kynos does read them -- and file the bug, not to
   post-process the emitted document.

## Where to look first

`docs/` is the canonical, ratified engineering documentation: `docs/requirements/` (product/FRs/
NFRs), `docs/architecture/` (overview, api, data model, streaming, security, components, ADRs),
`docs/testing.md` (strategy and coverage gates), `docs/operations/` (configuration, deployment).
Check the relevant doc there before making an architectural assumption.

## CI Commands to ensure pass before pushing completed work (e.g. before PR)

`mise.toml` is the single source of truth for every command CI and the git hooks run
([ADR-0009](docs/architecture/decisions/ADR-0009-release-engineering.md)). Do not add a check by
writing a command into a workflow or a hook -- add a mise task and call it from both.

```
mise run ci        # everything CI enforces, except coverage and image builds
mise tasks         # list every task
```

Individual tasks, if you need them: `rust:fmt`, `rust:clippy`, `rust:test`, `rust:deny`,
`rust:lockfile`, `rust:coverage`, `rust:coverage:report`, `ts:check`, `ts:typecheck`, `ts:test`,
`docs:build`, `codegen:openapi`, `codegen:openapi:check`, `check:ffmpeg-version`,
`check:mutants-skip-fakes`. The `:fix` variants (`rust:fmt:fix`, `ts:check:fix`) write their fixes.

`rust:test:pg` runs the opt-in real-Postgres tier and is deliberately outside `ci`; it needs
`docker compose -f compose.dependencies.yaml up -d` and `BEAM_TEST_DATABASE_URL`. `cargo test
--workspace` must always pass with none of that running (NFR-201).

`check:ffmpeg-build` builds the `ffmpeg-builder` stage of `beam-server/Containerfile`, proving the
FFmpeg pin compiles on Debian and not only that the three pinned strings match. It needs a container
runtime, so like `rust:test:pg` it is outside `ci` (an image build, which `ci` excludes by
definition) and outside the pre-push hook. CI runs it in the `ffmpeg-build` job, under the same path
filter as `check:ffmpeg-version`.

The `rust:mutants*` tasks are advisory and deliberately outside `ci` -- they take hours, and a
surviving mutant is a prompt to harden the code, not a broken build. See the hardening loop above.

Commits must be [Conventional Commits](https://www.conventionalcommits.org/); `convco` enforces this
in the `commit-msg` hook, and release-please derives the version and `CHANGELOG.md` from them.

The Rust tasks statically vendor an LGPL-only FFmpeg by default (via `ffmpeg-sys-next`'s `build`
feature), so they compile on hosts without system FFmpeg development libraries -- no `.pc` files for
`libavutil` etc., which is common outside CI/containers. This requires a `nasm` assembler on `PATH`.
See [ADR-0007](docs/architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md). CI and container
builds dynamically link a system FFmpeg instead, by setting `BEAM_CARGO_FEATURES=""`; do the same in
a gitignored `mise.local.toml` if your host has the development libraries.
