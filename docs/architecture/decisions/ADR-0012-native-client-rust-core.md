# ADR-0012: A shared Rust core for native clients, with a generated REST client

## Status

Accepted.

## Context

Beam had exactly one client, `beam-web`. Three more are wanted: Android
([#67](https://github.com/justin13888/beam/issues/67)), Android TV
([#65](https://github.com/justin13888/beam/issues/65)), and iOS/tvOS
([#66](https://github.com/justin13888/beam/issues/66)), under umbrella
[#78](https://github.com/justin13888/beam/issues/78).

A native client is not a nicety here. [ADR-0004](ADR-0004-never-transcode.md) commits Beam to direct
play: the server never transcodes and never remuxes, so whether a title plays at all is decided by
what the *client* can decode. A browser that cannot decode HEVC or AV1 simply fails, with a file
sitting on disk that a phone three feet away could play in hardware. Client-side codec capability
matching is therefore the entire value proposition of a native client, and it is the same problem on
every platform.

The naive approach writes that logic three times. Worse, it writes the *API client* three times:
Beam's REST surface is 31 operations, and three hand-written clients means three places for a field
name to be wrong in a way that compiles.

## Decision

**A shared Rust core, `beam-client-core`, owns everything above the platform.** The API client,
authentication state, TLS trust decisions, codec capability matching, source selection, up-next
resolution, the progress-report throttle and its durable retry queue, and Relay cursor paging all
live there once. Android, and later Apple and GTK, consume it over UniFFI. The boundary is drawn so
that the platform owns what only the platform can do -- rendering, decoding, persistence, and the
media session -- and the core owns everything that would otherwise be reimplemented per platform and
diverge.

**The REST client is generated, never hand-written.** `api/openapi.json` is exported from
`beam-server`'s own handler annotations and lowered to Rust by [spargen](https://github.com/getkono/spargen)
in `build.rs`. A gap in spargen blocks and is fixed upstream rather than worked around locally,
because a hand-written exception is exactly how a generated client stops describing the server.
The document is now OpenAPI 3.2, emitted by Kynos under
[ADR-0010](ADR-0010-openapi-3-2-kynos.md), and spargen 0.4.0 reads 3.2 natively -- including the
typed SSE stream, which it lowers to an `EventStream<AdminEventDto>`. This was written while the
server still emitted 3.1 and said the two were independent; they are not, and the dependency turned
out to be already satisfied.

The four media-delivery operations are omitted from generation (`build.rs`). Playback never goes
through this client -- `MediaSource` carries the URLs, `ServerRecord::absolute_url` resolves them,
and Media3 does its own ranged HTTP -- so a generated method that buffered a whole file would be one
no caller may use. That the omission also sidesteps a Kynos/spargen disagreement about how a binary
body is described is a coincidence, not the reason; the disagreement is filed upstream.

**`beam-client-core` depends on none of `beam-domain`, `beam-index`, or `beam-server`.**
`beam-domain` is not reusable despite the name: it takes a non-optional `sea-orm` dependency and
spells every repository error as `sea_orm::DbErr`, so linking it would drag a Postgres wire-protocol
driver into an Android `.so`. `beam-index` links FFmpeg, which does not cross-compile to Android.
The shared vocabulary is reproduced from the generated types instead, which are derived from the
same handler annotations, so nothing is actually duplicated.

**Authentication is an in-app WebView, as an interim.** `beam-server` reads exactly one credential,
the `beam_session` cookie, and `sanitize_redirect_path` accepts only same-origin relative paths --
so the OIDC provider cannot redirect to a custom scheme a native app could intercept. A Custom Tab's
cookie jar is not readable by the app, which would leave the credential somewhere the app can never
see it. Lifting the cookie out of a WebView is the only flow the server supports as it stands. The
core exposes an `AuthStrategy` seam so a native token mint can replace it without the screens
changing. This falsifies NFR-601's claim that a native client needs no server changes; a
corresponding NFR is added.

**Trust is decided by the user, once, per certificate.** Self-hosted servers on a LAN routinely
present a self-signed certificate, so "the platform trust store said no" cannot be the end of the
story -- but nor can it be waved away. The public trust store is always consulted first and its
acceptance is final. Only on failure is a user-accepted certificate considered, and only when the
whole-certificate SHA-256 matches, the SANs cover the host, and it has not expired. A pin therefore
widens trust for exactly one certificate on one host; it can never reject a publicly-valid one and
never generalises. The digest shown is the one `openssl x509 -fingerprint -sha256` prints, so a user
comparing the app against their server is comparing like with like -- a trust decision the user
cannot independently verify is theatre.

**Android CI is JVM-only.** Unit tests, Robolectric behaviour tests, and Roborazzi screenshot tests
run on the JVM and are required checks. No emulator runs in CI: it needs nested virtualisation that
hosted runners do not reliably provide, and a flaky required check is worse than a missing one.
Robolectric's native graphics mode renders real pixels, so the screenshots are a record of what the
app draws rather than of blank rectangles.

**One product version.** The Android `versionName` is read from `version.txt`, the file
release-please already rewrites, so the app follows the single product version
([ADR-0009](ADR-0009-release-engineering.md)) with nothing to keep in step by hand. `versionCode` is
derived from it, because Android orders installs by that integer alone.

## Consequences

Android, Android TV and iOS share one implementation of the logic that decides what plays. A fix to
capability matching or to the progress queue is a fix everywhere, and a divergence between clients
becomes a bug in one place rather than a difference of opinion between three.

The workspace gains a seventh Cargo member, which enters `clippy --workspace -D warnings` and
`cargo test --workspace`. Host builds are cheap; the NDK is needed only for the cross-compile task.

Generated client code is excluded from the coverage denominator, since it is spargen's output rather
than hand-written logic. The core's own logic is unusually testable -- capability matching, up-next,
paging and the throttle are pure functions -- so the crate raises the workspace coverage number
rather than straining the gate.

Offline downloads are in scope for the Android client, which
[#67](https://github.com/justin13888/beam/issues/67) had explicitly excluded as "its own project".
This is a deliberate widening, recorded here so it is a decision rather than an oversight.

The WebView auth flow is a liability recorded as such. It is the only flow the current server
supports, it works, and it is fenced behind a seam. A native mint is tracked separately.

Hardware decoder behaviour is not verified by CI. The emulator has software decoders only, so HEVC,
AV1, HDR and audio passthrough are exercised only against those -- which is precisely the case the
capability-matching code exists to handle, and therefore the highest-value thing to re-test on real
hardware.
