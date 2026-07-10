# ADR-0009: One command source of truth, one product version, one release train

## Status

Accepted.

## Context

Beam's automation had three sources of truth for "what a check is". `.github/workflows/rust.yml`,
`.github/workflows/typescript.yml`, and `.github/workflows/lint.yml` hardcoded commands inline;
`lefthook.yml` restated them with different flags (`cargo clippy-local` locally versus `cargo clippy
--workspace` in CI); and `AGENTS.md` restated them again in prose. They drifted, as duplicated
configuration always does: Bun was pinned to `latest` in CI but `1.3` in the container images, and
the container images built FFmpeg 8.0 while `ffmpeg-next` and CI had moved to 8.1.

Versioning had no source of truth at all. Six crates each declared `version = "0.1.0"`
independently, the root `package.json` said `0.0.1`, `beam-docs` said `0.0.1`, and `beam-web` had no
version field. Nothing kept them in step and nothing said which one described "Beam".

There were no tags, no `CHANGELOG.md`, and no published images. Every deployment built both
Containerfiles from source, including compiling FFmpeg — the single largest deployment-friction item
([#72](https://github.com/justin13888/beam/issues/72)).

## Decision

**`mise.toml` is the single source of truth for tools and commands.** Every check has exactly one
definition, as a mise task. CI (`.github/workflows/ci.yml`) and the git hooks (`hk.pkl`) both invoke
`mise run <task>`; neither restates a command. Adding a check means adding a task. Rust is the one
deliberate exception: `rust-toolchain.toml` keeps the pin, because rustup and rust-analyzer already
honor it and duplicating it in `mise.toml` would recreate the very drift this ADR removes.

**`hk` runs the git hooks.** `pre-commit` is staged-file-scoped and auto-fixing; `commit-msg` runs
`convco` so that non-Conventional Commits are rejected at authorship; `pre-push` approximates CI,
omitting only coverage instrumentation and container builds.

**Conventional Commits are enforced, and drive versioning.** `convco` lints — at commit time, over
the commit range on a pull request, and against the pull request title, since a squash merge lands
that title as the commit release-please parses. `convco` does not generate the changelog;
release-please does, from the same commits.

**Beam has one product version.** `[workspace.package].version` in the root `Cargo.toml` is
inherited by all six crates; the three `package.json` versions and `version.txt` move in lockstep.
One tag, `vX.Y.Z`; one GitHub Release; one `CHANGELOG.md`. This is honest about what Beam is: a
server binary and a web image built from the same commit. No crate is published to crates.io, so
per-crate versions would carry no information.

**release-please runs the release train.** It keeps a `chore: release vX.Y.Z` pull request open on
`master`, and merging it tags the commit and creates the Release. That Release publishes multi-arch
`linux/amd64` + `linux/arm64` images to `ghcr.io/justin13888/beam-server` and
`ghcr.io/justin13888/beam-web` (tagged `vX.Y.Z`, `X.Y`, and `latest`), attaches `openapi.json`,
`beam-web-dist.tar.gz`, and `SHA256SUMS`, and deploys the docs site.

## Consequences

Container images are built on **native** runners (`ubuntu-latest` and `ubuntu-24.04-arm`) rather
than under QEMU, because `beam-server` compiles FFmpeg from source and emulated arm64 would take
hours. This depends on the repository being public, where arm64 runners are free.

`beam-web/openapi.json` is a **build-context input**, not something the image generates. The web
image previously compiled FFmpeg and the entire Rust workspace across four stages purely to run
`cargo run --example export_openapi`. CI and `release.yml` now run `mise run codegen:openapi` before
building. The cost is that a bare `podman compose build web` requires that task first; the image
fails with an explicit message when the spec is absent.

Bumping `[workspace.package].version` invalidates the six `beam-*` entries in `Cargo.lock`, and
release-please's `simple` strategy has no Cargo updater. `release.yml` therefore refreshes the
lockfile on the release branch, and the `rust-lockfile` CI job is the tripwire if that ever
regresses. The alternative — release-please's `cargo-workspace` plugin — updates `Cargo.lock` for
you but produces per-crate tags, contradicting the single-version decision above.

`ci-ok` is the only status check branch protection should require. Path-filtered jobs report as
"skipped", and GitHub counts a skipped required check as passing, so requiring the individual jobs
would let a Rust-only pull request merge without the TypeScript jobs having run. `ci-ok` fails
unless every job either succeeded or was skipped.

**Two prerequisites must be configured by a repository admin**, or the release train does not run:

- **`RELEASE_PLEASE_TOKEN`** — a fine-grained PAT with `contents: write` and `pull-requests: write`.
  Pull requests created with the default `GITHUB_TOKEN` do not trigger workflows, so without this
  the release pull request shows zero status checks and branch protection blocks its merge. The
  `Cargo.lock` sync push needs it for the same reason.
- **A bootstrap tag** matching `.release-please-manifest.json` (`v0.1.0`), so release-please has a
  baseline rather than walking the entire history.

Optionally, `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` enable the docs deploy. Cloudflare
Pages' Git integration triggers only on branch pushes, never on tags or Releases, so a released docs
site has to be pushed with `wrangler`; the job skips silently until those secrets exist.

The `cargo t-local` / `clippy-local` / `build-local` aliases are gone with `.cargo/config.toml`: they
hardcoded a third copy of the vendored-FFmpeg feature list. `BEAM_CARGO_FEATURES` in `mise.toml` is
now the only place it appears.

Contributors now need `mise` (and, for the vendored-FFmpeg build, `nasm`). `mise install && mise run
setup` replaces the previous rustup/bun/`lefthook install` sequence. Existing clones must delete the
stale lefthook shims in `.git/hooks/`: `hk` installs via git's `hook.*` config on git 2.54+ rather
than by writing hook files, so the old shims would otherwise keep running alongside it.
