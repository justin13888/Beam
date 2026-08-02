# Beam — Product Requirements

## Vision

Beam is a self-hosted media server for home labs and small businesses — a leaner, more opinionated
alternative to systems like Jellyfin or Plex. It indexes a read-only media library, enriches it with
rich metadata, and serves it to clients over a small, clean domain API.

Beam's defining architectural commitment is that **the server never transcodes media**. Format
handling belongs to the client (the browser direct-plays the source format) and quality selection
belongs to the library (operators index additional pre-encoded file versions of the same title).
See [ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md).

Beam ships exactly one client: the `beam-web` web application, the reference implementation of the
domain API. The API is designed so that native clients (mobile, TV, desktop) can be built against it
without server-side changes (see NFR-6xx in `non-functional.md`); none exists today — see the
[client roadmap umbrella #78](https://github.com/justin13888/beam/issues/78).

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
- A single OpenAPI-first REST API ([ADR-0002](../architecture/decisions/ADR-0002-rest-only-api.md)),
  with real-time scan and enrichment progress over Server-Sent Events (SSE).
- OIDC-only authentication via the backend-for-frontend (BFF) pattern, with JIT user provisioning
  and admin-role determination via an email allowlist
  ([ADR-0003](../architecture/decisions/ADR-0003-oidc-bff-auth.md)).
- Rich metadata enrichment (posters, backdrops, descriptions, ratings, genres, release dates,
  season/episode titles) via the `cameo` crate (TMDB + AniList), run as an asynchronous background
  pipeline after indexing ([ADR-0006](../architecture/decisions/ADR-0006-cameo-enrichment.md)).
- A web client covering: OIDC login, library browsing, movie and show/season/episode detail pages,
  direct-play playback with a source-quality picker, resume playback with a continue-watching row,
  server-side instant search (Postgres `pg_trgm`), full-file download, and an admin area (library
  management, scan progress, enrichment status and manual re-enrich, log viewing).
- A security posture in which no bearer or session tokens ever appear in a URL or query string, CSRF
  is defended via SameSite cookies plus Origin/Referer validation, and all admin-only mutations are
  gated by role, not merely by authentication. See `non-functional.md` NFR-1xx.

## Out of scope (tracked in issues)

- Native clients — umbrella [#78](https://github.com/justin13888/beam/issues/78):
  Android TV [#65](https://github.com/justin13888/beam/issues/65),
  tvOS/iOS [#66](https://github.com/justin13888/beam/issues/66),
  Android mobile [#67](https://github.com/justin13888/beam/issues/67).
- HLS/DASH exploration — [#75](https://github.com/justin13888/beam/issues/75).
- Server-side image proxy for poster/backdrop art (currently direct CDN links, see NFR-501 and
  [ADR-0008](../architecture/decisions/ADR-0008-image-cdn-direct.md)) —
  [#70](https://github.com/justin13888/beam/issues/70).
- Rate limiting on auth endpoints — [#69](https://github.com/justin13888/beam/issues/69).
- Enrichment tuning knobs (batch size, min confidence, metadata language) —
  [#71](https://github.com/justin13888/beam/issues/71).
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
