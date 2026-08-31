# ADR-0013: The Apple client owns its player shell and demuxes Matroska itself

## Status

Accepted.

## Context

[ADR-0012](ADR-0012-native-client-rust-core.md) built `beam-client-core` so that Android, Apple and
GTK would share one implementation of the logic that decides what plays, and named iOS/tvOS
([#66](https://github.com/justin13888/beam/issues/66)) as the second consumer. Its source comments
have said "the surface Kotlin and Swift actually call" since the day it was written. This ADR is
what happens when the Swift half is actually built.

The reason a native client exists is unchanged and, on Apple, sharper.
[ADR-0004](ADR-0004-never-transcode.md) commits Beam to direct play: the server never transcodes and
never remuxes, so whether a title plays is decided entirely by the client. On Android that is a
codec question — Media3 demuxes Matroska natively, and what is left is asking `MediaCodecList` what
the hardware decodes. On Apple it is a *container* question first. AVFoundation demuxes MP4, MOV and
a handful of others, and cannot open an `.mkv` at all — not badly, not partially. A large share of a
typical self-hosted library is Matroska.

Under "never transcode" there is no server-side remux to fall back on. The only honest answers were
to tell a large fraction of every library that it will never play, or to demux it ourselves.

## Decision

**Beam owns the player shell and swaps the engine underneath.** `PlaybackEngine` is the seam;
`EngineSelector` is the single place the choice is made, from the container the server probed.

**`AVPlayerEngine` is preferred wherever AVFoundation can open the file.** This is not laziness. It
brings Picture in Picture, AirPlay, the system transport, remote-control and Now Playing
integration, and buffering and seeking behaviour tuned over a decade. Every one of those would have
to be rebuilt on top of the other engine, and none would be as good. Two details make it
Beam-specific rather than a bare `VideoPlayer`:

- `beam-server` authenticates the stream endpoint with the `beam_session` cookie and refuses tokens
  in the URL (FR-504). AVFoundation's loader does its own range requests and never goes through
  `URLSession`, so the credential reaches it through `AVURLAssetHTTPCookiesKey`.
- AVFoundation exposes no `URLSession` delegate, so a user-accepted certificate would be rejected
  before a byte was fetched — which is every self-hosted server on a LAN.
  `AVAssetResourceLoaderDelegate`'s authentication-challenge callback is the one hook it offers, and
  it is what makes the trust model of ADR-0012 work here at all.

**`SampleBufferEngine` handles Matroska and WebM, and decodes nothing.** The core demuxes the
container into encoded samples with their codec-private bytes; Swift wraps those in
`CMSampleBuffer`s and lets VideoToolbox and Core Audio decode them in hardware exactly as they would
for an MP4, under one `AVSampleBufferRenderSynchronizer`. Owning the *container parsing* and nothing
else is what preserves the hardware decode path, which is the whole value proposition of a native
client. The cost is stated rather than discovered: no Picture in Picture, no AirPlay, no system
transport, because those are `AVPlayer` features. That is why the selector prefers `AVPlayerEngine`.

**The demuxer lives in the Rust core, not in Swift.** It is `beam-client-core::demux`, over
[`matroska-demuxer`](https://crates.io/crates/matroska-demuxer) — pure Rust, no dependencies,
permissively licensed. Putting it in the core rather than in the app is the same decision ADR-0012
made about capability matching: a Matroska parser written in Swift would have to be written again
for GTK, and would diverge. The platform still owns the bytes: `ByteSource` is a foreign trait with
an HTTP-ranged and a file-backed implementation, because the platform already has a tuned HTTP stack
with the user's trust decisions wired in.

The extractor seeks to **the keyframe at or before** the requested position and reports where it
landed. Matroska positions at the first frame at or *after* a timestamp, and a decoder handed one of
those renders nothing until the next keyframe arrives.

**What neither engine can do is reported, never hidden.** A DTS or TrueHD track has no Core Audio
decoder; PGS and VobSub subtitles are bitmap formats this renderer does not implement; VP9 has no
`CMVideoFormatDescription` constructor. All of them appear in the menu marked unsupported. This is
the same treatment `capability::select` gives an undecodable source, and for the same reason: under
direct play that is a permanent property of the file the viewer may need to act on, and a menu that
silently omitted the track would look like the file did not have it.

**iOS 26 and macOS 26 are the floor, and Liquid Glass is unconditional.** Supporting one release
earlier would mean an `if #available` in every container view and two visual languages to keep in
step. The floor has exactly one source: `mise.toml` exports it to cargo when cross-compiling and
writes it into `Version.xcconfig` for the Xcode targets, because a static library built against the
SDK's newest OS is free to call symbols the app's declared floor does not have.

**macOS is a native SwiftUI target, not Mac Catalyst.** Liquid Glass on macOS 26 is native to
SwiftUI, and Catalyst would compromise the design language while adding an iOS-shaped layout to a
window.

**The Xcode project is generated by XcodeGen from a committed `project.yml`.** The reviewable
artifact is a hundred lines of YAML rather than a `.pbxproj` nobody can read in a diff and everyone
conflicts on — the same reasoning that has `beam-android` generate its bindings and `jniLibs`.

**CI runs on macOS runners, with Xcode pinned.** Four jobs behind a path filter that also watches
`beam-client-core`, since the Swift bindings and the XCFramework are generated from it.

**tvOS is not in this release, and could not be.** Two independent blockers, recorded here as
blockers rather than worked around:

1. `aarch64-apple-tvos` is a Tier 3 Rust target. rustup ships no `rust-std` for it, so building the
   core for tvOS needs a nightly toolchain and `-Z build-std`, against a repository that pins stable
   in `rust-toolchain.toml`.
2. tvOS has no `WKWebView`. The `beam_session` cookie lift is the only authentication flow the
   server supports (NFR-605), and it needs a web view whose cookie jar the app can read. There is no
   such thing on tvOS, so a tvOS client cannot sign in at all until a native token mint exists.

The second blocker is the more interesting one: on tvOS the native token mint NFR-605 calls a
"should" becomes a hard prerequisite. It is the same endpoint Android TV
([#65](https://github.com/justin13888/beam/issues/65)) needs.

## Consequences

Apple users get hardware-decoded playback of the Matroska files that make up much of a self-hosted
library, which no browser and no `AVPlayer`-only client can offer. The capability matcher decides
what plays through exactly the code Android uses, so a fix to source selection is a fix on both.

The sample-buffer engine is the least CI-verifiable part of the client, in the same way and for the
same reason as Android's hardware decoding. A simulator has software decoders and a synthetic
render path; A/V sync, seek accuracy and hardware decode are judged on real hardware. Recorded in
`docs/testing.md` beside the equivalent Android gap.

Snapshot references are light mode only. `glassEffect` does not render its material offscreen and
resolves content colours against the light appearance regardless of the host's
`overrideUserInterfaceStyle`, so a recorded dark reference is black text on a black ground — a
picture of a renderer limitation that would then pass forever whether or not dark mode worked.
Recording it would have been worse than not having it.

Every Rust CI job now installs four more `rust-std` target sets, on Linux runners that will never
build an Apple binary. That is the price of declaring the target set once in `rust-toolchain.toml`
rather than in a build script, and it is the same trade the Android targets already make.

The Apple deployment floor is stated in three places — `mise.toml`, `Version.xcconfig` (generated
from it) and `Package.swift`, whose `platforms:` takes literals only. The first two cannot drift;
the third must be kept in step by hand.
