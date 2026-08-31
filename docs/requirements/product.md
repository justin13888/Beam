# Beam — Product Requirements

## Vision

Beam is a self-hosted media server for home labs and small businesses — a leaner, more opinionated
alternative to systems like Jellyfin or Plex. It indexes a read-only media library, enriches it with
rich metadata, and serves it to clients over a small, clean domain API.

Beam's defining architectural commitment is that **the server never transcodes media**. Format
handling belongs to the client (the browser direct-plays the source format) and quality selection
belongs to the library (operators index additional pre-encoded file versions of the same title).
See [ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md).

Beam ships three clients: the `beam-web` web application, the reference implementation of the domain
API; `beam-android`, a native Android client for phone and tablet; and `beam-apple`, a native
SwiftUI client for iOS, iPadOS and macOS. All three are built against the same domain API (see
NFR-6xx in `non-functional.md`). Android TV and tvOS remain outstanding, both blocked on the same
thing — neither has a web view to lift a session cookie from, so neither can authenticate until a
native token mint exists (NFR-605). See the
[client roadmap umbrella #78](https://github.com/justin13888/beam/issues/78).

Native clients exist for a specific reason rather than as a matter of taste. Beam direct-plays, so
whether a title plays at all is decided by what the client can decode; a browser without an HEVC or
AV1 decoder simply fails on a file a phone would play in hardware. The logic that makes that
decision is shared across native clients by `beam-client-core`, a Rust crate consumed over UniFFI —
see [ADR-0012](../architecture/decisions/ADR-0012-native-client-rust-core.md).

## Personas

**Self-hoster / administrator** — runs the Beam server, typically on home-lab hardware. Owns the
media filesystem and the Beam data directory. Configures library root paths, triggers and monitors
scans, manages access (OIDC identity provider configuration, including the claim that grants admin —
`BEAM_OIDC_ADMIN_CLAIM`),
and triages enrichment or indexing failures. Interacts with Beam through the admin area of the web
client, server environment variables, and logs.

**End user / viewer** — a household or organization member who logs in via the configured identity
provider and consumes media: browses libraries, searches, views details, plays or downloads files,
and resumes partially watched content. Has no visibility into filesystem layout or server internals.

## The three delivery scenarios

Every piece of media Beam serves is delivered under exactly one of three scenarios. These are
first-class domain concepts — the domain model, API surface, and client UI are organized around them
([ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md)).

| Scenario | Trigger condition | What the server does | What the client does |
|---|---|---|---|
| **Full download** | User explicitly requests a file for offline use | Serves the file's bytes as an `attachment` download over HTTP, honoring Range requests for resumable downloads | Issues a normal browser download; no player involved |
| **Direct-play streaming** | Common case: adequate bandwidth, user presses play | Serves the original file's bytes byte-for-byte over HTTP Range requests; no transcoding, remuxing, or format conversion | Player (Vidstack) requests byte ranges and decodes the source format natively in-browser |
| **Source selection (constrained bandwidth / high latency)** | User (or client) chooses a lower-quality existing version of the same title | Resolves the request against a different `files` row already indexed under the same `movie_entries`/`episode`; the "lower quality" stream is itself direct-play against a smaller pre-existing file | Presents a source-quality picker enumerating the available file versions and switches playback to the selected file |

Scenario (c) depends on the library already containing multiple indexed file versions of the same
logical title. Beam does not create those versions; it only lets the client choose among whatever
the operator has indexed. This is how Beam supports low-bandwidth delivery without a transcoder.

## What Beam delivers

- A single modular-monolith server binary (`beam-server`): HTTP API, OIDC auth, in-process indexing
  and enrichment, direct-play streaming
  ([ADR-0001](../architecture/decisions/ADR-0001-modular-monolith.md)).
- A single OpenAPI-first REST API
  ([ADR-0010](../architecture/decisions/ADR-0010-openapi-3-2-kynos.md)),
  with real-time scan and enrichment progress over Server-Sent Events (SSE).
- OIDC-only authentication via the backend-for-frontend (BFF) pattern, with JIT user provisioning
  and admin-role determination from a configured ID-token claim
  (`BEAM_OIDC_ADMIN_CLAIM`/`BEAM_OIDC_ADMIN_VALUE`), recomputed on every login so the role is
  revoked as well as granted (FR-106,
  [ADR-0003](../architecture/decisions/ADR-0003-oidc-bff-auth.md)).
- Rich metadata enrichment (posters, backdrops, descriptions, ratings, genres, release dates,
  season/episode titles) via the `cameo` crate (TMDB + AniList), run as an asynchronous background
  pipeline after indexing ([ADR-0006](../architecture/decisions/ADR-0006-cameo-enrichment.md)).
- A web client covering: OIDC login, library browsing, movie and show/season/episode detail pages,
  direct-play playback with a source-quality picker, resume playback with a continue-watching row,
  server-side instant search (Postgres `pg_trgm`), full-file download, and an admin area (library
  management, scan progress, enrichment status and manual re-enrich, log viewing).
- In-process per-client token-bucket rate limiting on the OIDC login/callback endpoints and on the
  browse/search endpoint, tunable and switchable via `BEAM_RATE_LIMIT_*` (NFR-107). Streaming and
  download are deliberately excluded, because a player legitimately bursts range requests.
- Operator-tunable enrichment: batch size, interval, minimum match confidence and metadata
  language, via `BEAM_ENRICH_*` and `BEAM_METADATA_LANGUAGE`.
- A security posture in which no bearer or session tokens ever appear in a URL or query string, CSRF
  is defended via SameSite cookies plus Origin/Referer validation, and all admin-only mutations are
  gated by role, not merely by authentication. See `non-functional.md` NFR-1xx.

## Out of scope (tracked in issues)

- Remaining native clients — umbrella [#78](https://github.com/justin13888/beam/issues/78):
  Android TV [#65](https://github.com/justin13888/beam/issues/65) and the tvOS remainder of
  [#66](https://github.com/justin13888/beam/issues/66). Android mobile
  [#67](https://github.com/justin13888/beam/issues/67) and Apple iOS/macOS
  [#66](https://github.com/justin13888/beam/issues/66) have shipped; both remaining clients inherit
  `beam-client-core`, and both need a native token mint before they can sign in at all.
- HLS/DASH exploration — [#75](https://github.com/justin13888/beam/issues/75).
- Server-side image proxy for poster/backdrop art (currently direct CDN links, see NFR-501 and
  [ADR-0008](../architecture/decisions/ADR-0008-image-cdn-direct.md)) —
  [#70](https://github.com/justin13888/beam/issues/70).
- Automated browser e2e tests (Playwright) — [#74](https://github.com/justin13888/beam/issues/74).
- Kubernetes/Helm deployment — [#76](https://github.com/justin13888/beam/issues/76).

## Non-goals

- Live transcoding or remuxing of any kind — a rejected philosophy, not a deferred feature
  ([ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md)).
- Non-Postgres storage backends.
- Full-text or fuzzy search beyond `pg_trgm` similarity matching.
- Watch-history analytics or recommendations.

## Cross-references

See `functional.md` for numbered, testable functional requirements (FR-1xx through FR-7xx) and
`non-functional.md` for cross-cutting quality attributes (NFR-1xx through NFR-6xx).
