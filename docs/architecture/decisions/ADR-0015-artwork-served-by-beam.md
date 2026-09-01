# ADR-0015: Poster and backdrop art is served by Beam, not linked to a provider CDN

## Status

Accepted. Supersedes [ADR-0008](ADR-0008-image-cdn-direct.md), which took the opposite decision and
recorded the conditions under which it should be revisited, tracked in
[#70](https://github.com/justin13888/beam/issues/70).

## Context

ADR-0008 stored `poster_url` and `backdrop_url` as direct TMDB/AniList CDN links and let the browser
fetch them, accepting that every viewer's IP reaches a third party on every poster render. It called
an image proxy "a legitimate future addition, not rejected outright, if this tradeoff proves
unacceptable to operators in practice", and named three conditions: stricter privacy requirements,
CDN instability or hotlinking policy changes, and offline art requirements.

Two of the three are now met by evidence inside this repository rather than by speculation, and the
codebase had already drifted into contradicting itself about which world it was in.

**The Android client was written for a world where Beam serves artwork.** `BeamApplication.kt` routes
Coil through the shared authenticated OkHttp client, and says why: "posters and backdrops are served
from the same authenticated origin as everything else, so an image loader with its own client would
send no session cookie and every poster would be a 401." That was false -- artwork came
unauthenticated from a provider CDN -- so the comment described an intention rather than the system.
The Apple client asserted the opposite in `PosterImage.swift`: "Beam serves poster and backdrop URLs
straight from the metadata provider's CDN (ADR-0008), so this is an ordinary remote image load with
no auth to attach." Two first-party clients held contradictory beliefs about one field.

**`beam-client-core` already resolved Beam-relative artwork.** `catalog.rs` runs every artwork URL
through `ServerRecord::absolute_url`, and its own fixtures use `/artwork/m1/poster.jpg`.

**Offline art does not work.** `DownloadTitleStore` writes a download's title text down at enqueue
time precisely because "resolving those over the network would defeat the point of an offline
feature" -- and then stores `posterUrl` as a remote CDN link, so every downloaded title renders a
placeholder with no network.

**A CSP could not be written.** [#81](https://github.com/justin13888/beam/issues/81) has to allow
`img-src` for the TMDB host for as long as artwork is fetched from it.

## Decision

`beam-server` serves all artwork -- movie and show posters and backdrops, season posters, episode
thumbnails -- from `GET|HEAD /v1/artwork/{kind}/{id}/{variant}`, fetching each image from the
provider once and caching it on disk. `poster_url`, `backdrop_url` and `thumbnail_url` carry that
path instead of a provider URL. A viewer's client never contacts TMDB or AniList.

**Unconditionally, with no opt-out.** A toggle would fork the contract: `poster_url` would mean
"provider URL" or "Beam path" depending on one operator's configuration, and every client -- web,
Apple, Android, and every generated one -- would have to handle both forever. It would also leave
#81 unable to write `img-src 'self'`, since the TMDB host would have to stay allowlisted against the
possibility the proxy is off, which would mean this decision never delivered one of the things it is
a prerequisite for. It is also the shape ADR-0010 and ADR-0012 exist to protect: one declaration
derives the routes, the document and every client, and a mode switch that changes what a documented
field contains is how a document stops describing its server. Resource knobs remain
(`BEAM_ARTWORK_CACHE_MAX_BYTES`, `BEAM_ARTWORK_FETCH_TIMEOUT_SECS`,
`BEAM_ARTWORK_MAX_IMAGE_BYTES`, `BEAM_ARTWORK_NEGATIVE_TTL_SECS`); they tune the mechanism without
changing what the API means.

**The URL names the title, not the image.** A content-addressed path would let every response be
`immutable`, but it changes whenever enrichment refreshes a title's art -- and clients store these
URLs, which is the whole of the offline case. The URL is therefore stable and the *validator* moves:
the strong `ETag` is the cache key, a digest of the provider URL, so it changes exactly when the
artwork does. Revalidation costs one primary-key row read and a `304` with no body.

**One cache, and no invalidation.** Images are stored one file per key under
`BEAM_DATA_DIR/artwork`, keyed by a digest of the upstream URL, evicted least-recently-used against
a byte ceiling, with concurrent misses for one key collapsed into a single upstream fetch. There is
no in-process byte cache above the files: the OS page cache already holds every recently served
poster, and a second LRU would duplicate it while needing its own ceiling, eviction and accounting.

There is also no cache invalidation, which is the objection ADR-0008 raised when it called
"cache-busting when a title's artwork changes on the provider side" part of what made a proxy
non-trivial. It dissolves rather than being solved, and rests on one assumption, stated here so that
it can be falsified: **TMDB and AniList serve content-addressed, immutable image paths**
(`/wwemzKWzjKYJFfCeiB57q3r4Bcm.jpg`, `bx5114-….jpg`), so bytes never change under a URL. The only
turnover event is enrichment writing a *different* URL to a title, which is a different key, a
different `ETag` and fresh bytes -- and which strands the old entry for the LRU to reclaim. High
turnover costs one extra cache entry, never a stale poster. If that assumption ever fails, the fix
is a TTL on a cache entry, not a redesign.

One qualification, found by fetching a real poster rather than by reasoning: **TMDB content-
negotiates on `Accept`**, and returns WebP for a `.jpg` path when WebP is offered. So a URL does not
name one representation in general -- it names one *per `Accept` header*. Beam's is constant,
derived from the formats it is willing to store, so the cache stays correct: the same URL always
yields the same bytes for Beam. The consequence to know is that widening that set does not
retroactively re-fetch anything already cached, since the key and therefore the `ETag` are unchanged.
That is harmless -- a cached entry records the format it was stored as and is served under it -- but
it is the reason the key is a digest of the URL alone rather than of the URL and the `Accept`.

**No SSRF surface, and not because of an allowlist.** The only URL ever fetched is one enrichment
itself wrote onto a row. Nothing a client sends is a URL -- only an id and two enum variants -- so
there is no allowlist to maintain and no bypass to find. The fetcher additionally refuses anything
that is not `https`, will not follow a redirect off `https`, refuses a body over a configured
ceiling before reading it, accepts only image content types, and attaches no Beam credential of any
kind (NFR-502).

### What replaces the revisit condition

| ADR-0008's condition | What answers it |
|---|---|
| Stricter privacy requirements, with no opt-out short of disabling enrichment | Answered here. No client contacts a provider CDN, so there is nothing left to opt out of. |
| CDN instability or a hotlinking policy change | Answered here. A cached image survives a provider reorganising or removing a path; before, the poster simply broke until the next enrichment pass. |
| Offline art requirements | **Made possible here, not finished here.** The URL is now stable and reachable over the client's authenticated session, so Coil's disk cache can hold it -- but priming that cache at download-enqueue time is [#152](https://github.com/justin13888/beam/issues/152). |

**Closing [#152](https://github.com/justin13888/beam/issues/152) is a condition of this decision
fully answering #70.** Until it is closed, Beam can say its artwork is private, cached and stable,
but not that a downloaded title renders on a plane.

## Consequences

**Positive:**
- No third party observes which viewer requests which title's artwork. NFR-501 becomes a property of
  the system rather than a documented exception to it.
- A provider that reorganises or deletes an image path no longer breaks a poster that Beam has
  already cached.
- [#81](https://github.com/justin13888/beam/issues/81) can write `img-src 'self'`, which it could not
  while any artwork came from elsewhere.
- Android's image loader is now correct rather than aspirational, and the two native clients agree
  about what an artwork URL is.
- One upstream fetch serves every client: twenty viewers opening the same grid produce one request
  per image, where twenty browsers hitting a CDN produced twenty.

**Negative / accepted cost:**
- **A remote-access deployment now carries artwork egress it used to offload to the provider CDN.**
  Posters are small and every client caches them hard, so this is modest -- but it is real, and it is
  the cost ADR-0008 was buying when it chose the other way. An operator on a metered uplink pays it.
- Beam is now on the path of every image render, so an artwork bug degrades a grid where previously
  only a provider outage could. The failure is bounded to a placeholder: a title with no art, an
  unknown id, an inapplicable variant and a provider that has dropped the image are all `404`, which
  every client already renders as a placeholder.
- The cache is state on disk that an operator must size. Sizing it below a library's artwork costs
  re-fetches, never correctness.
- The generated Rust client cannot describe the endpoint, so it omits it. That is right on its own
  merits -- artwork is fetched by the platform image loader, not the typed client -- but the
  underlying gap is real and is filed as
  [getkono/spargen#82](https://github.com/getkono/spargen/issues/82): `classify_media` has no arm for
  a media type range, so `image/*` is rejected, and naming an exact `image/jpeg` does not help.
- This is a breaking API change. `poster_url` and its siblings changed meaning, and every client had
  to be updated in step.
