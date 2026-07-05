# Coverage

This document describes how code coverage is measured, run locally, and enforced in CI. See
`docs/testing/strategy.md` for the testing philosophy this coverage measures the extent of, including
the parts of the system deliberately excluded from hermetic unit testing.

## Tooling

| Stack | Tool | Config location |
|---|---|---|
| Rust | [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) | Invocation lives in `.github/workflows/rust.yml`'s `test` job; no separate config file |
| Web (`beam-web`) | `@vitest/coverage-v8` (Vitest's built-in v8 coverage provider) | `beam-web/vitest.config.ts`, `test.coverage` block |

## Running coverage locally

### Rust

```sh
cargo llvm-cov --workspace --lcov --output-path lcov.info
cargo llvm-cov report --summary-only
```

On a host with system FFmpeg development libraries (`.pc` files for `libavutil` etc.) installed, this
runs the same way CI does. On a host without them — see `docs/operations/dev-setup.md` and
[ADR-0007](../architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md) — build with the
`vendored-ffmpeg` feature enabled on the three crates that link `ffmpeg-next`:

```sh
cargo llvm-cov --workspace \
  --features beam-domain/vendored-ffmpeg,beam-index/vendored-ffmpeg,beam-stream/vendored-ffmpeg \
  --lcov --output-path lcov.info
```

There is no `llvm-cov` equivalent of the `t-local`/`clippy-local`/`build-local` `.cargo/config.toml`
aliases; pass the `--features` flag directly as shown above when running coverage on a vendored-ffmpeg
host.

### Web

```sh
bun run test:coverage
```

This runs `vitest run --coverage` (see `beam-web/package.json`) and writes `text`, `lcov`, and `html`
reports to `beam-web/coverage/` per the `reportsDirectory` setting in `vitest.config.ts`.

## Enforced thresholds

This push turns coverage from *measured* into *enforced*:

| Suite | Threshold | Enforced by |
|---|---|---|
| Rust workspace (lines) | 70% | `cargo llvm-cov --workspace --fail-under-lines 70` in the `test` job of `.github/workflows/rust.yml` |
| Web (`beam-web`) | 60% (lines/functions/branches/statements) | `coverage.thresholds` in `beam-web/vitest.config.ts` |

Previously, both of these existed only as commented-out TODOs (a `--fail-under-lines 80` flag in
`rust.yml` and a `thresholds` block in `vitest.config.ts`), with coverage measured and uploaded as a
CI artifact but never gating a PR. This push enables both gates for real, at 70% (Rust) and 60% (web)
rather than the previously-sketched 80% figures — chosen deliberately as a realistic floor given the
current baseline test inventory (`docs/testing/strategy.md`), not as an aspirational target. There is
still no external coverage service (no Codecov/Coveralls integration); the lcov/html reports remain
CI artifacts (7-day retention) for local inspection, and the threshold flags are the actual gate.

**Ratchet, don't relax.** The intent is to raise both thresholds over time as the suites mature and
genuine coverage grows — not to lower them. If a PR fails the coverage gate, the correct response is
to add the missing tests (or, if coverage is inflated by genuinely untestable code, to isolate that
code so the tested surface reflects real behavior), not to reduce the threshold to make the PR pass.
Threshold changes should be a deliberate, reviewed decision tied to an actual improvement in the
suite, not a byproduct of an unrelated PR.

## What coverage doesn't tell you

A line-coverage percentage says a line executed during the test run; it says nothing about whether
the assertions around it were meaningful, and nothing about the integrations this project has
deliberately excluded from the hermetic unit-test layer. In particular, high coverage numbers here do
**not** mean the OIDC ↔ Dex round-trip, real `cameo` → TMDB/AniList network calls, or real Postgres
migration/query behavior have been validated — those are covered at the trait-fake level in the unit
suite (which coverage does measure) but the *real* integration is validated separately via the manual
runbook in `docs/operations/e2e-validation.md` (which coverage tooling has no visibility into at all).
Treat the coverage gate as a floor against undertested code, not as a substitute for the e2e runbook or
for the code-review scrutiny of what a covered line's test actually asserts.
