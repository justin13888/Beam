# ADR-0008: Poster/backdrop images served as direct CDN links, not proxied

## Status

Accepted.

## Context

The enrichment pipeline (ADR-0006) populates `poster_url` and `backdrop_url` on movies and shows from
TMDB and AniList. These providers serve images from their own CDNs. A media server can either store
those URLs and let the browser fetch images directly from the provider's CDN, or proxy every image
request through the server (fetching from the provider once, then serving/caching the bytes itself).
Building a correct image proxy means handling upstream fetch failures, caching/eviction policy for a
potentially large volume of poster/backdrop art, cache-busting when a title's artwork changes on the
provider side, and an additional request path through `beam-server` for every image render in the
UI — a non-trivial amount of new surface area for a concern (image delivery) that isn't core to
Beam's value.

## Decision

We store `poster_url`/`backdrop_url` as direct CDN URLs from TMDB/AniList and let the browser fetch
them directly — no server-side image proxy or cache. This is recorded as a deliberate, documented
tradeoff, not an oversight, precisely so it isn't quietly rediscovered as a "gap" later without
context on why it was skipped.

## Consequences

**Positive:**
- Zero additional server-side code, storage, or cache-invalidation logic for image delivery — the
  entire image pipeline is "store the URL the provider gave us."
- Image loading performance benefits from the provider's own CDN (likely faster and more geographically
  distributed than anything `beam-server` would implement itself for a self-hosted deployment).
- No image-proxying failure mode for `beam-server` to handle (a dead image link degrades gracefully
  in the browser — a broken `<img>` — without taking down or slowing an API response).

**Negative / accepted cost:**
- The end user's browser makes a direct request to a third-party CDN (TMDB's or AniList's) whenever
  a poster/backdrop renders, exposing the viewing user's IP address to that third party — a minor
  privacy tradeoff. TMDB and AniList are treated as low-risk, purpose-fit third parties for this
  tradeoff; an operator with stricter privacy requirements does not currently have an opt-out short of
  disabling enrichment/poster display entirely.
- If a provider changes or removes an image URL (e.g. TMDB reorganizes its image paths), Beam has no
  cached fallback — the poster simply breaks until the next enrichment pass refreshes it.
- Self-hosting purists who prefer a fully "phones-home to nothing but configured providers at
  enrichment time" model will notice that browsing the library still generates ongoing third-party
  network traffic for images, which is a different privacy posture than an image proxy would provide.
- A server-side image proxy remains a legitimate future addition, not rejected outright, if this
  tradeoff proves unacceptable to operators in practice — tracked in
  [#70](https://github.com/justin13888/beam/issues/70).
