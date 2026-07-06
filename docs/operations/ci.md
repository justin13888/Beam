# Continuous Integration

This document describes the GitHub Actions workflows and git hooks as they exist today, then the
changes this push makes to them. Workflow files live in `.github/workflows/`; composite actions live
in `.github/actions/`; git hooks are defined in `lefthook.yml`.

## Workflows today

### `rust.yml` — Rust CI

Triggered on push/PR to `master` when any `**/*.rs`, `**/Cargo.toml`, `Cargo.lock`, or
`rust-toolchain.toml` file changes.

- **`fmt`**: `cargo fmt --check`. No FFmpeg or toolchain setup beyond checkout — formatting doesn't
  need a compiled workspace.
- **`build-ffmpeg`**: runs the `./.github/actions/setup-ffmpeg` composite action to build/cache
  FFmpeg. Both `clippy` and `test` depend on this job (`needs: [build-ffmpeg]`) so the FFmpeg build/
  cache step only runs once per workflow run.
- **`clippy`**: installs the pinned toolchain (`dtolnay/rust-toolchain@1.91`) with the `clippy`
  component, runs `./.github/actions/install-rust-build-deps` (apt packages for
  x264/x265/vpx/opus/ass/freetype/fontconfig/fribidi/ogg/ssl, clang/libclang, protobuf-compiler) and
  `./.github/actions/setup-ffmpeg`, restores a `Swatinem/rust-cache@v2` cache keyed `clippy`, then
  runs `cargo clippy --workspace --all-targets -- -D warnings`.
- **`test`** ("Tests & Coverage"): same toolchain/build-deps/FFmpeg setup as `clippy`, with a
  `rust-cache` keyed `test`, then installs `cargo-llvm-cov` (via `taiki-e/install-action`) and runs:

  ```sh
  cargo llvm-cov --workspace --lcov --output-path lcov.info
  cargo llvm-cov report --summary-only
  ```

  The `lcov.info` is uploaded as a 7-day-retention GitHub Actions artifact (`rust-coverage`). There is
  **no** coverage threshold enforced today and no external coverage service (no Codecov/Coveralls) —
  an `80%` `--fail-under-lines` flag exists only as a commented-out TODO in the workflow file.

The `setup-ffmpeg` composite action (`.github/actions/setup-ffmpeg/action.yml`) builds **FFmpeg 8.0**
from source (cached at `/opt/ffmpeg`, cache key `ffmpeg-8.0-ubuntu-latest-v1`) with `--enable-gpl
--enable-version3` and the codec libraries listed above, then exports `PKG_CONFIG_PATH`,
`LD_LIBRARY_PATH`, and `PATH` for the rest of the job. This is a version drift from the workspace
`Cargo.toml`, which pins `ffmpeg-next = "8.1"` — CI builds and links FFmpeg 8.0 while the Rust crate
expects to bind against 8.1's API/ABI. This is flagged as a gap to fix (see "Changes this push"
below), not a currently-broken build, since FFmpeg 8.0→8.1 has not introduced a binding-breaking
change so far, but it is drift that should not be allowed to compound.

### `typescript.yml` — TypeScript CI

Triggered on push/PR to `master` when TS/JS/Astro files, `tsconfig*.json`, `package.json`/`bun.lock`,
or — because Rust API changes affect generated OpenAPI types — `beam-auth/src/**/*.rs`,
`beam-stream/src/**/*.rs`, or `Cargo.lock` change.

- **`generate-openapi`** ("Generate OpenAPI Artifacts"): builds the Rust toolchain + FFmpeg (same
  composite actions as `rust.yml`), sets up Bun, then:
  1. `cargo run --example export_openapi -p beam-stream > beam-web/openapi.json`
  2. `cd beam-web && bun run codegen:openapi` → generates `src/api.gen.ts`
  3. `cargo run --bin export_schema -p beam-stream > beam-stream/schema.graphql`
  4. `cd beam-web && bun run codegen` → generates `src/gql.ts` (GraphQL codegen)

  Uploads `beam-web/openapi.json`, `beam-web/src/api.gen.ts`, and `beam-web/src/gql.ts` as a
  7-day-retention artifact (`openapi-artifacts`), consumed by the three downstream jobs.
- **`build`**, **`typecheck`**, **`test`** (all `needs: [generate-openapi]`): each downloads the
  `openapi-artifacts` artifact into `beam-web`, then runs `bun run build`, `bun run typecheck`, and
  `bun run test:coverage` respectively. `test` uploads any `**/coverage` directories as a
  7-day-retention artifact (`if-no-files-found: ignore`).

  `test:coverage` runs `vitest run --coverage` with v8 coverage. As with Rust, thresholds exist in
  `beam-web/vitest.config.ts` only as a commented-out TODO block — coverage is measured and uploaded,
  never gated.

### `lint.yml` — Lint

Single job, `biome`: sets up Bun (`./.github/actions/setup-bun`) and runs `bun run check` (Biome)
across the whole repo. Triggered on the same TS/JS/Astro/`package.json`/`bun.lock`/`biome.json` path
filters as `typescript.yml`.

### Composite actions

- **`install-rust-build-deps`**: apt packages needed to *build* Rust crates against system FFmpeg/
  media libraries (build-essential, pkg-config, clang/libclang-dev, protobuf-compiler, and the
  x264/x265/vpx/opus/ass/freetype/fontconfig/fribidi/ogg/ssl dev packages).
- **`setup-bun`**: installs Bun, restores a `bun.lock`-keyed cache of `~/.bun/install/cache` and
  `**/node_modules`, and runs `bun install --frozen-lockfile` on a cache miss.
- **`setup-ffmpeg`**: see above.

## Git hooks today (`lefthook.yml`)

- **pre-commit** (parallel, staged-file-scoped, fast): `cargo fmt --check` (glob: any `.rs` file
  staged), `bun biome check {staged_files}` (glob: staged TS/JS/Astro).
- **pre-push** (parallel, CI-equivalent checks, gated per file type so a docs-only push doesn't wait
  on Rust compilation): `cargo clippy-local` and `cargo t-local` (glob: Rust/Cargo files) — the
  vendored-FFmpeg aliases from `.cargo/config.toml`, so the hook compiles hermetically without a
  system FFmpeg dev install (see [ADR-0007](../architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md));
  still plain `cargo test` under the hood, not `cargo llvm-cov` — and `bun run check` (glob: any
  JS/TS/Astro/JSON file) — Biome only.

Notably, pre-push today does **not** run `vitest` or `tsc`/`typecheck`. A web-breaking change (a
failing test, a type error) can pass all local hooks and only get caught once `typescript.yml` runs in
CI. `lefthook` itself is also not declared as a dependency anywhere in the repo (no `lefthook` entry
in any `package.json`, nothing installs or pins it) — a new contributor has to know to run `lefthook
install` manually per the README, with no automated enforcement that they have.

## Changes this push

- **Coverage thresholds actually enforced, not just measured.** `rust.yml`'s `test` job gains
  `--fail-under-lines 65` on the `cargo llvm-cov` invocation; `beam-web/vitest.config.ts` gains a
  `coverage.include` list (so untested files count in the denominator) and a `thresholds` block
  (lines 12% / functions 10% / branches 3% / statements 12%). These are calibrated against an actual
  measurement taken when the gate was wired, not the `70%`/`60%` figures originally sketched here —
  see `docs/testing/coverage.md` for the full rationale and the ratchet-only policy.
- **FFmpeg CI version bumped 8.0 → 8.1**, matching the pinned `ffmpeg-next = "8.1"` in the workspace
  `Cargo.toml`. `setup-ffmpeg`'s source URL, cache key, and `./configure` invocation update
  accordingly; this closes the version-drift gap noted above.
- **GraphQL codegen step removed from `typescript.yml`.** The `generate-openapi` job's steps 3 and 4
  above (`export_schema` → `schema.graphql` → `bun run codegen` → `gql.ts`) are deleted, along with
  `schema.graphql` and `gql.ts` from the uploaded artifact list. This push is REST/OpenAPI-only — the
  GraphQL stack is removed entirely (see `docs/architecture/overview.md` and
  `docs/requirements/product.md`), so there is no schema left to generate.
- **New `cargo-deny` job** in `rust.yml`: runs `cargo deny check` (advisories + licenses). This
  becomes meaningful — not just a formality — now that the FFmpeg licensing posture is concrete: the
  vendored-ffmpeg build path (`docs/operations/dev-setup.md`,
  [ADR-0007](../architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md)) is LGPL-only with no
  GPL/nonfree components, and `cargo-deny` is what keeps the dependency tree from silently drifting
  into a GPL or unlicensed dependency.
- **New `dependabot.yml`**, covering the `cargo`, `bun` (npm ecosystem), and `github-actions`
  ecosystems, for automated dependency-update PRs.
- **CI/hook parity fix**: `lefthook.yml`'s `pre-push` stage gains a `bun run test` (vitest) command
  and a typecheck command, gated on the same TS/JS/Astro glob as the existing `ts-full-check` command,
  so a web-breaking test failure or type error is caught locally before push instead of only in CI.
  `lefthook` becomes a declared devDependency (in the root `package.json`) so `bun install` guarantees
  it's present, rather than relying on an undocumented manual install step.

**Hooks remain a fast local approximation of CI, not a replacement for it.** Even after the parity
fix above, pre-push hooks run a vendored-FFmpeg `cargo test --workspace` (no coverage instrumentation) and a
scoped `bun run test`/`typecheck`, while CI additionally runs `cargo fmt --check` unconditionally,
full `cargo llvm-cov` coverage-gated runs, the OpenAPI codegen pipeline, `cargo-deny`, and Biome across
the whole repository regardless of what's staged. Passing local hooks is a strong signal a push will
pass CI; it is not a guarantee, and CI remains the actual merge gate — see `docs/requirements/non-
functional.md` NFR-208 for the enumerated set of checks CI is required to enforce on every PR.
