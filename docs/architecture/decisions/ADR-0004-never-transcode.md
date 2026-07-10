# ADR-0004: Never transcode server-side; three delivery scenarios instead

## Status

Accepted.

## Context

The previous streaming path remuxed source media to fragmented MP4 on the fly into a filesystem
cache (with a `Command::new("ffmpeg")` CLI shell-out specifically for subtitle handling, in addition
to the ffmpeg-next library linkage used elsewhere), backed by a `stream_cache` table that nothing
actually wrote to. A parallel HLS generator existed as a stub — 14 TODOs and a literal `panic!()` —
and was never wired into a working path. Server-side transcoding/remuxing, done properly, is a large and
operationally expensive feature: it requires careful CPU/GPU resource management, hardware
acceleration plumbing for anything at scale, a correctness story for every source codec/container
combination a library might contain, and a cache-invalidation story for derived artifacts. None of
that had actually been built, and building it properly is a multi-month undertaking orthogonal to
Beam's core value (indexing, cataloging, enriching, and serving a personal media library).

## Decision

`beam-server` never transcodes or remuxes media at request time, full stop. Delivery is organized
around three scenarios instead: (a) full download of the original file, (b) direct-play of the
original file via HTTP Range requests (the default case), and (c) source-quality selection among
multiple pre-existing indexed file versions of the same title (e.g. a 1080p and a 480p rip, both
indexed independently) — never a server-generated bitrate. Consequently, the stubbed HLS generator,
the fMP4 remux-on-request path, the `ffmpeg` CLI shell-out, the `stream_cache` table, and
`TranscodeService` were all deleted. `ffmpeg-next` usage is confined entirely to `beam-index`, for
probing technical metadata at index time only; `beam-server` and `beam-domain` dropped the
dependency entirely, which also fixed the `CodecId::Other(AVCodecID)` FFI-type leak in what is
supposed to be a framework-agnostic domain layer — codecs are plain strings/enums.

## Consequences

**Positive:**
- Eliminates an entire category of server-side complexity and operational cost (no encoder resource
  management, no hardware-acceleration configuration, no derived-artifact cache to invalidate).
- Server CPU/memory usage becomes predictable and roughly constant regardless of concurrent playback
  count, since serving byte ranges is cheap compared to encoding.
- Removes a meaningful attack surface: no external process invocation (`ffmpeg` CLI) driven by
  request-time, partially attacker-influenceable inputs.
- Fixes a genuine architecture violation (ffmpeg FFI types leaking into `beam-domain`) as a side
  effect of the broader simplification.

**Negative / accepted cost:**
- Beam cannot serve an arbitrary bitrate/format to a bandwidth- or device-constrained client unless
  the operator has proactively indexed a suitable alternate version. There is no automatic "detect
  slow connection, serve a lower bitrate of the same source" capability.
- No adaptive bitrate streaming (HLS/DASH) and no mid-playback seamless quality switching — switching
  quality means switching to a different `files` resource, which surfaces to the user as a discrete
  action, not an invisible ABR ladder. Revisiting HLS/DASH is tracked in
  [#75](https://github.com/justin13888/beam/issues/75).
- Some source formats/codecs that a browser cannot natively decode simply will not direct-play
  (some old codec choices, unusual container/subtitle combinations); the mitigation is "index a
  compatible version," not "server transcodes it for you," which is a real, user-visible limitation
  for libraries with exotic content.
- This is a rejected philosophy, not a deferred feature — reversing it later is a significant
  architectural undertaking, not a config flag.
