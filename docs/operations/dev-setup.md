# Development Setup

This is the step-by-step path from a clean checkout to a running `beam-server` + `beam-web` on your
own machine. For the full local end-to-end validation flow (login, scan, playback), see
`docs/operations/e2e-validation.md`. For the environment variable reference, see
`docs/operations/configuration.md`.

## 1. Install the toolchains

- **Rust**: install [rustup](https://rustup.rs/). The exact toolchain is pinned in
  `rust-toolchain.toml` (currently `1.91.0`, with the `rustfmt`, `clippy`, and `llvm-tools-preview`
  components) — rustup will pick this up automatically once you `cd` into the repo, no separate
  `rustup toolchain install` step is required.
- **Bun**: install [Bun](https://bun.sh/) (used for both `beam-web` and `beam-docs`, and as the
  package manager for the JS/TS side of the monorepo). Then run `bun install` from the repo root.

## 2. FFmpeg (only if the host lacks FFmpeg development libraries)

`beam-domain`, `beam-index`, and `beam-stream` link `ffmpeg-next` (pinned to `8.1` in the workspace
`Cargo.toml`). Building against a system FFmpeg installation requires its *development* files (the
`.pc` files for `libavutil`, `libavformat`, etc. that `pkg-config`/`ffmpeg-sys-next` look for at build
time) to be present, in addition to the runtime libraries. On many distributions — and notably on
immutable-OS setups such as Fedora Silverblue — these dev packages are unavailable or awkward to
install system-wide, and `cargo test --workspace` fails at build time with missing `.pc` files.

If your host already has FFmpeg 8.x development libraries installed and discoverable by
`pkg-config`, skip this section and use plain `cargo build`/`cargo test`/`cargo clippy` as usual.

If it doesn't, use the `vendored-ffmpeg` Cargo feature instead. This statically compiles a plain
LGPL-only FFmpeg from source via `ffmpeg-sys-next`'s `build`/`static` features — no GPL codec
libraries, since beam only ever needs FFmpeg's probing/decoding for reading technical stream metadata
at index time (resolution, codec, duration, bitrate), never encoding or transcoding (see
[ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md)). The full reasoning for this
tradeoff — local dev throwaway builds vs. distributed container artifacts — is documented in
[ADR-0007](../architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md); this doc only covers how
to use it.

Prerequisites on the build host: a C toolchain (`build-essential`/`gcc`+`make` equivalent), `nasm`,
and `libclang` (required by `bindgen`, which `ffmpeg-sys-next` uses to generate FFI bindings).

`.cargo/config.toml` defines three aliases that enable `vendored-ffmpeg` on the three
ffmpeg-consuming crates in one shot:

```sh
cargo t-local       # cargo test --workspace --features beam-domain/vendored-ffmpeg,beam-index/vendored-ffmpeg,beam-stream/vendored-ffmpeg
cargo clippy-local   # cargo clippy --workspace --all-targets --features ...same... -- -D warnings
cargo build-local    # cargo build --workspace --features ...same...
```

Use these instead of the plain `cargo test`/`clippy`/`build` commands whenever you're on a host
without system FFmpeg dev libraries. CI and container builds are unaffected by any of this — they
continue to dynamically link a system-built FFmpeg (see `docs/operations/ci.md` and
`docs/operations/deployment.md`).

## 3. Install git hooks

```sh
lefthook install
```

This wires up the hooks defined in `lefthook.yml`:

- **pre-commit** (staged files only, fast): `cargo fmt --check` on staged `.rs` files, `biome check`
  on staged TS/JS/Astro files.
- **pre-push** (CI-equivalent, gated by which file types changed): `cargo clippy-local`, `cargo
  t-local` (the vendored-FFmpeg aliases from section 2 above, so the hook builds hermetically
  without a system FFmpeg dev install), `bun run check`, plus (this push) `bun run
  test` and `bun run typecheck` — see `docs/operations/ci.md` for the hook/CI-parity fix this push
  makes and why hooks are a fast local approximation of CI, not a replacement for it.

`lefthook` itself is a declared devDependency as of this push (see `docs/operations/ci.md`), so
`bun install` in step 1 already fetches it; you still need to run `lefthook install` yourself once per
clone to activate the git hooks.

## 4. Configure environment variables

```sh
cp .env.example .env
./verify-config.sh
```

`verify-config.sh` checks that the required environment variables are set before you bring up the
compose stack, and prints warnings for common misconfiguration (e.g. a default Postgres password).
See `docs/operations/configuration.md` for the full variable reference; note that `verify-config.sh`
predates this push's new OIDC/session/enrichment variables and does not yet validate them — treat it
as the config-validation entry point, with updating it for the new variable surface tracked as
separate follow-up work, not something this push's docs rewrite it.

## 5. Generate the API client (OpenAPI codegen)

`beam-web` consumes a TypeScript client generated from `beam-server`'s OpenAPI spec. From
`beam-web/`:

```sh
bun run codegen:openapi:full
```

This runs `export-openapi` (builds and runs `beam-stream`'s `export_openapi` example, writing
`openapi.json`) followed by `codegen:openapi` (`openapi-typescript` generating `src/api.gen.ts`). This
push removes the GraphQL codegen scripts (`export-schema`, `codegen`, `codegen:full`, and the
`schema.graphql`/`gql.ts` outputs they produced) entirely, since the GraphQL API surface itself is
removed — REST/OpenAPI is the only API this push ships. If you're on a checkout mid-migration and
still see the GraphQL scripts in `beam-web/package.json`, they are dead and slated for removal, not an
alternative path.

## 6. Run the dependency stack

```sh
podman compose -f compose.dependencies.yaml up
```

Today this brings up Postgres, Valkey, and Traefik. This push replaces Valkey with a Dex OIDC identity
provider preloaded with static test users (see `docs/operations/configuration.md` and
`docs/operations/e2e-validation.md`) — once that lands, this same command brings up Postgres, Dex,
and Traefik instead. `beam-server` needs Postgres to start at all, and needs Dex (or another reachable
OIDC provider) for login to work end-to-end; you can run `beam-server` without Dex up if you only need
to exercise unauthenticated endpoints.

Run pending sea-orm migrations against the compose Postgres instance before starting the server for
the first time — see `beam-migration`.

## 7. Run `beam-server` locally

From the repo root (or the relevant crate directory once the workspace is restructured — see
`docs/architecture/overview.md` for the `beam-stream` → `beam-server` rename):

```sh
cargo run -p beam-stream   # today's binary name; beam-server once the rename lands
```

or, on a host using the vendored-ffmpeg path:

```sh
cargo build-local
./target/debug/beam-stream   # or the built beam-server binary
```

The server reads its configuration from environment variables (`docs/operations/configuration.md`);
make sure `.env` is sourced into your shell, or run it via `podman compose` once `compose.beam.yaml`'s
`server`/`web` services are wired up for the target-state topology (`docs/operations/deployment.md`).

## 8. Run `beam-web` locally

```sh
cd beam-web
bun dev
```

This starts Vite's dev server, defaulting to port 5173. It talks to `beam-server` via the
`C_STREAM_SERVER_URL` environment variable, which defaults to `http://localhost:8000` — `beam-server`'s
default bind port. If you've changed `BIND_ADDRESS`/`STREAM_HOST_PORT`, set `C_STREAM_SERVER_URL`
(and, for OIDC redirects, `BEAM_WEB_URL`) to match.

At this point you have a full local stack: Postgres + Dex (compose), `beam-server` (cargo run), and
`beam-web` (`bun dev`). Continue to `docs/operations/e2e-validation.md` for the manual login/scan/
playback validation flow.
