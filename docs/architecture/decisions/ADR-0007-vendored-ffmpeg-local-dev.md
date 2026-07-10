# ADR-0007: Vendored FFmpeg feature for local dev/test only

## Status

Accepted.

## Context

`cargo test --workspace` links `ffmpeg-next`, which requires system FFmpeg *development* libraries
(the `.pc` pkg-config files and headers) to build against — not just the FFmpeg runtime. Many hosts,
particularly immutable-OS setups like Fedora Silverblue, don't ship these development packages by
default even when the FFmpeg runtime itself is present, which means a plain `git clone && cargo test`
fails on a meaningful fraction of contributor machines. CI works around this by compiling
FFmpeg 8.0 from source and exporting `PKG_CONFIG_PATH` — which works, but is slow and
does nothing for a developer's local machine, where the same friction remains. ADR-0004 confined
`ffmpeg-next` usage to `beam-index` alone, which narrowed the problem but didn't
remove it: `beam-index`'s tests (and any workspace-wide `cargo test`) still need something to link
against.

## Decision

We added a `vendored-ffmpeg` Cargo feature to `beam-index`, using `ffmpeg-sys-next`'s `build` +
`static` features to statically compile a plain FFmpeg from source as part of the build. This
vendored build is deliberately non-GPL, LGPL-only — no `libx264`, `libx265`, or other
proprietary/copyleft-triggering codec libraries — since it only needs to back the *probing* path
(reading container/stream metadata), which relies on FFmpeg's native decoders, not its encoders or
GPL-licensed codec libraries. This feature is strictly a local-dev/test convenience: container
images and CI continue to dynamically link a system-provided FFmpeg.

## Consequences

**Positive:**
- `cargo test --workspace` becomes hermetic and works out of the box on hosts without system FFmpeg
  dev packages, directly fixing the Fedora Silverblue-class friction this ADR is written to solve.
- Because the vendored build is LGPL-only with no proprietary codec libraries, it introduces no new
  licensing obligations for a developer's local throwaway test binary.
- Confining this to a feature flag (opt-in) rather than the default keeps normal builds unaffected
  for contributors who already have system FFmpeg dev libraries installed and would rather not pay a
  static-compile cost on every clean build.

**Negative / accepted cost:**
- The first build with `vendored-ffmpeg` enabled compiles FFmpeg from source, which is slow (minutes,
  not seconds) — mitigated by normal Cargo build-artifact caching across subsequent builds, but a real
  cost on a clean checkout or CI cache miss.
- Static-linking FFmpeg into a *distributed* artifact (a container image, a release binary) has
  different licensing implications than a developer's local test binary — specifically, this ADR does
  **not** extend to container images or CI, which deliberately keep dynamically linking a
  system-provided FFmpeg for licensing and image-size reasons. This is a considered boundary, not an
  oversight: mixing the two build modes inconsistently across environments is an accepted complexity
  in exchange for keeping distributed artifacts on the simpler, well-understood dynamic-linking
  licensing story.
- A vendored, LGPL-only FFmpeg build supports a strict subset of the codecs a full system FFmpeg
  build might (no proprietary codec libraries), which is sufficient for `beam-index`'s probing
  use case (which relies on natively-supported decoders) but would not be sufficient if some future
  use case needed encoding via a licensed codec library locally.
