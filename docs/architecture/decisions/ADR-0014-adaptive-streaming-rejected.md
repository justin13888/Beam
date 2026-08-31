# ADR-0014: Adaptive streaming is rejected; playback capability belongs to the client

## Status

Accepted. Settles the revisit condition [ADR-0004](ADR-0004-never-transcode.md) left open in
[#75](https://github.com/justin13888/beam/issues/75); does not supersede it.

## Context

ADR-0004 committed Beam to direct play and listed "no adaptive bitrate streaming (HLS/DASH)" among
its accepted costs, with a pointer to #75 for revisiting it. #75 framed that revisit narrowly: *if
real-world direct-play compatibility or constrained-bandwidth behaviour proves insufficient, explore
packaging-only HLS/DASH (still no transcoding) or capability-based encode selection improvements
first.*

That pointer propagated. Five documents — this directory's ADR-0004, `streaming.md`, `overview.md`,
FR-503 and `product.md` — described adaptive streaming as *deferred*, which is a claim about
schedule rather than about design. Meanwhile the public documentation had already hardened in the
opposite direction ("This is a rejected design, not a missing feature"), and the codebase committed
to a different answer entirely: [ADR-0012](ADR-0012-native-client-rust-core.md) built
`beam-client-core::capability`, whose `playability()` and `select_source()` decide what plays from
the client's own decoder set, and [ADR-0013](ADR-0013-apple-client-two-engines.md) shipped an Apple
client that demuxes Matroska itself, so a container AVFoundation cannot open plays in hardware
anyway.

The gap between "deferred" and what is actually being built is the thing this ADR closes.

## Decision

**`beam-server` will not generate or serve HLS or DASH manifests or segments.** This is settled, not
deferred. FR-503 loses its issue pointer and becomes unconditional, and #75 is closed. Three
arguments, taken in the order #75 raised them.

**Packaging-only HLS/DASH is remuxing under another name.** HLS and DASH are defined over segments —
CMAF/fMP4 or MPEG-TS. A library's MKV, AVI and unfragmented MP4 files contain none, so producing a
manifest for them means repackaging their bytes. Doing that at request time is the fMP4 remux path
ADR-0004 deleted; doing it at index time is a derived artifact, which brings back the cache and the
invalidation story ADR-0004 deleted along with it — the `stream_cache` table dropped in
`m20260704_000001_drop_stream_cache.rs` is what that looked like the first time. "Packaging, not
transcoding" sounds like a smaller commitment than it is: only an already-fragmented MP4 could be
served under a manifest without touching a byte, and libraries are not made of those.

**Adaptive bitrate over pre-existing encodes is not achievable, packaging or not.** Beam's alternate
sources are independently produced rips — a 1080p remux and a 480p re-encode that never met. Their
keyframes fall in different places, their segment boundaries cannot be made to align, and their
durations often differ by frames. A multi-variant playlist over them can list them; it cannot switch
between them mid-segment without a visible break, which is the entire point of an ABR ladder.
Switching quality stays what ADR-0004 made it: a discrete, visible action against a different
`files` resource. #141 automates *when* that action fires without pretending it is seamless.

**Packaging would not fix the failure viewers actually hit.** Repackaging changes the container, not
decodability. The failure modes documented in `playback-compatibility.mdx` — video with no audio, a
black screen with sound, nothing at all — are codec and container *support* failures on the client.
A manifest in front of an HEVC stream a browser cannot decode changes nothing about that stream.
Client-side capability matching and client-side demuxing do.

**What replaces the revisit condition.** #75's two conditions were real; they were pointed at the
wrong remedy. Each now has a named issue answering it directly:

| #75's condition | What answers it |
|---|---|
| Direct-play compatibility insufficient | [#138](https://github.com/justin13888/beam/issues/138) capability-aware source selection in `beam-web`, which today picks `sources[0]` blind and so also violates FR-704; [#140](https://github.com/justin13888/beam/issues/140) client-side Matroska demux in the browser, the counterpart of [ADR-0013](ADR-0013-apple-client-two-engines.md); [#139](https://github.com/justin13888/beam/issues/139) the measured browser capability matrix |
| Constrained-bandwidth behaviour insufficient | [#141](https://github.com/justin13888/beam/issues/141) client-driven downgrade to a smaller indexed source on sustained rebuffering, built on the `QualityPolicy` the native core already has |
| Neither could be settled with evidence | [#143](https://github.com/justin13888/beam/issues/143) playback failure and rebuffer telemetry |

Capability belongs in one place per platform and nowhere else: `beam-client-core::capability` for
the native clients, its browser equivalent for `beam-web`. A second implementation that drifts from
the first is the outcome ADR-0012 exists to prevent.

## Consequences

**The rejection rests on a mitigation that does not exist for TV.** Both of ADR-0004's answers —
"index a compatible version" and "place a second, smaller rip in the library" — require a title to
carry more than one file. Movies can; **episodes cannot**. The schema permits it (`files.episode_id`
is a plain foreign key), but `IndexService` calls `create_episode` unconditionally, so a second file
for the same episode collides with `idx_episodes_unique` and fails the scan. For television, Beam
today offers neither a compatibility fallback nor a bandwidth fallback, and the user-facing docs say
so. **Closing [#142](https://github.com/justin13888/beam/issues/142) is a condition of this decision
standing**; if it is not closed, the honest description of Beam's TV behaviour is "one file, take it
or leave it", and the argument above weakens accordingly.

**Negative / accepted cost:**
- A viewer on a genuinely constrained link, whose library holds exactly one encode of the title, has
  no option at all. ADR-0004 accepted this provisionally; this ADR accepts it permanently.
- Quality switching remains visible and discrete. A viewer who expects the invisible ladder every
  commercial service provides will experience Beam as less capable, and that is correct — it is less
  capable in this one respect, deliberately.
- The burden moves onto clients, which is where it is hardest to test: hardware decoder behaviour
  cannot be verified in CI (ADR-0012), and the browser matrix has to be measured rather than cited.
- Reversing this needs a new ADR and, realistically, the transcoder ADR-0004 rejected.

**Positive:**
- `beam-server` stays a byte server. No encoder pool, no segment cache, no derived artifacts, no
  invalidation story, and no request-path ffmpeg — the properties ADR-0004 bought are now permanent
  rather than provisional.
- The five documents that called this "deferred" can state Beam's actual delivery model without a
  footnote, and the public documentation, which already stated it plainly, stops being ahead of the
  engineering record.
- Effort that a packaging pipeline would have consumed goes to the clients instead, where it fixes
  the failures viewers actually report rather than the one they do not.
