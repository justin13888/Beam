# Beam — Product Requirements

Status: target state for the current documentation + refactor push. This document describes where
Beam is going, not a snapshot of what exists in the repository today. Where existing behavior is
being replaced or removed, that is called out explicitly.

## Vision

Beam is a self-hosted media server for home labs and small businesses — a leaner, more opinionated
alternative to systems like Jellyfin or Plex. Beam deliberately narrows scope in exchange for
simplicity and robustness: it indexes a read-only media library, enriches it with rich metadata, and
serves it to clients over a small, clean domain API. It does not attempt to be a universal media
transcoding platform.

The defining architectural bet of this push is that **the server never transcodes media on the
fly**. Instead, Beam pushes format handling to the client (the browser can direct-play whatever
source format is present) and pushes quality selection to the library (operators who want a lower
bitrate option index a second file version of the same title). This keeps the server simple, keeps
CPU/memory usage predictable, and avoids an entire class of transcoding-pipeline bugs and operational
cost. It is a philosophy, not a temporary limitation.

This push delivers exactly one client: a web application, built as the reference implementation of
the domain API. The API is designed so that future native clients (mobile, TV, desktop) can be built
against it without server-side changes — see NFR-6xx in `non-functional.md` — but no such client is
built or scheduled in this push.

## Personas

**Self-hoster / administrator**
Runs the Beam server, typically on home-lab hardware (NAS, mini-PC, small VM). Owns the media
filesystem and the Beam data directory. Responsible for: configuring library root paths, triggering
and monitoring scans, managing which users may access the instance (via OIDC identity provider
configuration and the admin allowlist), and triaging enrichment or indexing failures. Interacts with
Beam primarily through the admin area of the web client, occasionally through server configuration
(environment variables) and logs.

**End user / viewer**
A household or organization member who logs in via the configured identity provider and consumes
media: browses libraries, searches for titles, views details, plays or downloads files, and resumes
partially watched content. Has no visibility into or interest in filesystem layout, indexing
internals, or server operations.

## The three delivery scenarios

Every piece of media Beam serves is delivered to a client under exactly one of three scenarios. These
are first-class domain concepts, not incidental HTTP behaviors — the domain model, the API surface,
and the client UI are all organized around them.

| Scenario | Trigger condition | What the server does | What the client does |
|---|---|---|---|
| **Full download** | User explicitly requests a file for offline use | Serves the file's bytes as an `attachment` download over HTTP, honoring Range requests for resumable downloads | Issues a normal browser download; no player involved |
| **Direct-play streaming** | Common case: adequate bandwidth, user presses play | Serves the original file's bytes byte-for-byte over HTTP Range requests; performs no transcoding, remuxing, or format conversion | Player (Vidstack) requests byte ranges as needed and decodes the source format natively in-browser |
| **Source selection (constrained bandwidth / high latency)** | User (or the client, based on network conditions) chooses a lower-quality existing version of the same title | Resolves the request against a different `files` row already indexed under the same `movie_entries`/`episode`; no on-the-fly re-encoding occurs — the "lower quality" stream is itself just direct-play against a smaller pre-existing file | Presents a source-quality picker enumerating the file versions available for the title, and switches playback to the selected file |

Scenario (c) depends entirely on the library already containing multiple indexed file versions of
the same logical title (e.g., a 1080p remux and a 480p re-encode of the same movie, both indexed as
separate `files` rows). Beam does not create those versions; it only lets the client choose among
whatever the operator has indexed. This is the mechanism by which Beam supports low-bandwidth
delivery without ever running a transcoder.

## Scope of this push

### In scope

Built on top of the project's existing strengths — trait-based repository/service abstractions with
in-memory fakes at every external boundary, a zero-infrastructure Rust test suite (`cargo test
--workspace` with no Postgres/Redis/Docker required), a working Rust → TypeScript OpenAPI codegen
pipeline, and CI enforcing fmt/clippy/tests/lint/typecheck on every PR — this push delivers:

- A single modular-monolith server binary (`beam-server`) that absorbs indexing in-process, replacing
  the separate gRPC indexer process.
- A single OpenAPI-first REST API, replacing the GraphQL stack entirely.
- Real-time scan and enrichment progress over Server-Sent Events (SSE).
- Strictly OIDC-based authentication via a backend-for-frontend (BFF) pattern, with JIT user
  provisioning and admin-role determination via an email allowlist.
- Rich metadata enrichment (posters, backdrops, descriptions, ratings, genres, release dates,
  season/episode titles) via the `cameo` crate (TMDB + AniList), run as an asynchronous background
  pipeline after indexing.
- A web client covering: OIDC login, library browsing, movie and show/season/episode detail pages,
  direct-play video playback with a source-quality picker, resume playback with a continue-watching
  row, server-side instant search (Postgres `pg_trgm`), full-file download, and an admin area
  (library management, scan progress, enrichment status and manual re-enrich, log viewing).
- A security posture in which no bearer or session tokens ever appear in a URL or query string,
  CSRF is defended via SameSite cookies plus Origin/Referer validation, and all admin-only mutations
  are gated by role, not merely by authentication. See `non-functional.md` NFR-1xx.

### Out of scope this push (deferred, not forgotten)

- Native mobile, TV, or desktop clients. The domain API is designed not to preclude these, but none
  is built now.
- Live transcoding of any kind. This is a rejected philosophy for Beam, not a deferred feature.
- HLS/DASH adaptive streaming. The existing HLS generator (a stub with TODOs and a panic) is deleted,
  not completed, in favor of the three-scenario delivery model above.
- Server-side image proxying or caching of poster/backdrop art. Images are served as direct CDN URLs
  from TMDB/AniList this push; this is a documented privacy tradeoff (see NFR-5xx), not an oversight.
- Distributed or Kubernetes-native deployment. Remains a long-term aspiration, not addressed here.
- A Bazel build migration (previously attempted on an abandoned branch).
- Non-Postgres storage backends.
- Full-text or fuzzy search beyond `pg_trgm` similarity matching.
- Watch-history analytics or recommendations.

## Cross-references

See `functional.md` for numbered, testable functional requirements (FR-1xx through FR-7xx) and
`non-functional.md` for cross-cutting quality attributes (NFR-1xx through NFR-6xx).
