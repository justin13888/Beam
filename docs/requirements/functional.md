# Beam — Functional Requirements

Requirements use RFC 2119 keywords (MUST, MUST NOT, SHOULD, SHOULD NOT, MAY) to indicate normative
strength. Each requirement is independently testable. See `product.md` for narrative context and
`non-functional.md` for cross-cutting quality attributes.

## FR-1xx — Authentication & Session

- **FR-101**: The server MUST support authentication exclusively via OpenID Connect (OIDC). No
  password-based login, registration, or "forgot password" flow exists
  ([ADR-0003](../architecture/decisions/ADR-0003-oidc-bff-auth.md)).
- **FR-102**: The server MUST implement the OIDC Authorization Code flow with PKCE, performed
  entirely server-side. The browser MUST NOT receive, store, or handle ID tokens, access tokens, or
  refresh tokens at any point.
- **FR-103**: On successful OIDC authentication, the server MUST establish a session identified by an
  opaque session cookie. The cookie MUST be `httpOnly` and MUST use `SameSite=Lax`.
- **FR-104**: Session state MUST be persisted server-side in Postgres
  ([ADR-0005](../architecture/decisions/ADR-0005-sessions-in-postgres.md)).
- **FR-105**: On a user's first successful login, the server MUST just-in-time (JIT) provision a
  local user record keyed by the `(issuer, subject)` pair from the OIDC identity token.
- **FR-106**: On every login, the server MUST derive the user's admin role solely from the configured
  ID-token claim (`BEAM_OIDC_ADMIN_CLAIM`/`BEAM_OIDC_ADMIN_VALUE`) asserted by the IdP, and MUST set
  or clear the stored admin role accordingly (granting **and** revoking). With no admin claim
  configured, no user is admin. The server MUST NOT expose any other mechanism to grant admin.
- **FR-107**: The web client MUST initiate login by redirecting the browser to `/v1/auth/login`. The
  client MUST NOT embed or invoke any OIDC client-side library.
- **FR-108**: The web client MUST determine current-session identity and role by calling
  `/v1/auth/me`, and MUST treat a non-2xx response as "not authenticated."
- **FR-109**: The server MUST provide a logout endpoint that invalidates the server-side session and
  clears the session cookie.
- **FR-110**: For local development, the identity provider MUST be satisfiable by Dex running via
  `compose.dependencies.yaml`, configured with static test users, requiring no external network
  dependency.

## FR-2xx — Library & Indexing

- **FR-201**: The server MUST run as a single binary (`beam-server`) that performs library indexing
  in-process; no separate indexer process or gRPC service boundary is required at runtime
  ([ADR-0001](../architecture/decisions/ADR-0001-modular-monolith.md)).
- **FR-202**: The server MUST access configured media library root paths in a strictly read-only
  fashion. The indexer MUST NOT write, rename, move, or delete files under a library root.
- **FR-203**: The server MUST maintain a separate, writable data directory (`BEAM_DATA_DIR`)
  distinct from any library root, used for its own state (e.g., the enrichment metadata cache).
- **FR-204**: The server MUST classify indexed filesystem entries into movies and TV
  shows/seasons/episodes, persisting them as `movie_entries` and `episode`-family rows respectively.
- **FR-205**: The server MUST support multiple indexed file versions (distinct `files` rows) under a
  single logical movie or episode entry, to support the source-selection delivery scenario.
- **FR-206**: The server MUST detect and de-duplicate files that have already been indexed, based on
  filesystem identity and modification-time change detection, without re-processing unchanged files
  on subsequent scans.
- **FR-207**: The server MUST support triggering a library scan from an admin action in the web
  client; no manual out-of-band process invocation is required.
- **FR-208**: The server MUST emit scan progress events (started, per-item progress, completed,
  failed) over Server-Sent Events (SSE) for consumption by the web client.
- **FR-209**: The server MUST support adding and removing library root paths via an admin-facing API,
  without requiring a server restart or manual configuration file edit.
- **FR-210**: A library scan MUST complete (or fail) independently of metadata enrichment; enrichment
  MUST NOT block or extend the scan's completion.

## FR-3xx — Metadata Enrichment

- **FR-301**: The server MUST enrich indexed movies and shows with metadata (poster URL, backdrop
  URL, description, ratings, genres, release date) via the `cameo` crate, which unifies TMDB and
  AniList sources ([ADR-0006](../architecture/decisions/ADR-0006-cameo-enrichment.md)).
- **FR-302**: Metadata enrichment MUST run as a background pipeline that begins after a title has been
  indexed and classified, and MUST NOT block the indexing/classification scan itself.
- **FR-303**: The server MUST persist enrichment status per title (e.g., pending, enriched, failed)
  and MUST make that status queryable by the admin area.
- **FR-304**: On enrichment failure, the server MUST retry with backoff rather than permanently
  marking the title as failed after a single attempt.
- **FR-305**: The server MUST enrich TV show titles at the season and episode level (season/episode
  titles, descriptions, air dates), not only at the top-level show.
- **FR-306**: Enrichment MUST proceed for AniList-sourced titles without requiring any API key. The
  absence of a configured `BEAM_TMDB_API_TOKEN` MUST NOT prevent AniList-sourced titles from being
  enriched.
- **FR-307**: If no `BEAM_TMDB_API_TOKEN` is configured, the server MUST leave TMDB-eligible titles
  un-enriched (rather than failing indexing or scan completion) and MUST surface this condition in
  admin-visible enrichment status.
- **FR-308**: The server MUST expose an admin-triggerable "re-enrich" action, scoped to a single title
  or to all titles, that re-runs enrichment regardless of current status. Operator-facing enrichment
  tuning knobs (batch size, minimum confidence, metadata language) are deferred — tracked in
  [#71](https://github.com/justin13888/beam/issues/71).
- **FR-309**: The server MUST emit enrichment progress/status-change events over SSE, in the same
  manner as scan progress (FR-208).
- **FR-310**: Poster and backdrop images MUST be served to the client as direct URLs to the
  TMDB/AniList CDN; the server does not proxy, cache, or re-host them
  ([ADR-0008](../architecture/decisions/ADR-0008-image-cdn-direct.md), NFR-501). A server-side image
  proxy is deferred — tracked in [#70](https://github.com/justin13888/beam/issues/70).

## FR-4xx — Browse, Search & Detail

- **FR-401**: The web client MUST provide a library browsing view listing movies and shows with
  poster art, title, and, where available, genres and rating.
- **FR-402**: The server MUST expose a movie detail endpoint returning enriched metadata and the set
  of available file versions (for source selection, per FR-205).
- **FR-403**: The server MUST expose a show detail endpoint supporting season and episode
  enumeration, including enriched per-episode metadata where available.
- **FR-404**: The server MUST provide a search endpoint that performs matching server-side using
  Postgres `pg_trgm` similarity, and MUST NOT implement search by loading the full title set into
  application memory and filtering in Rust.
- **FR-405**: The web client MUST provide debounced type-ahead search that queries the search
  endpoint as the user types and renders results without a full page navigation.
- **FR-406**: Search results MUST include enough information (title, poster URL, media type, id) for
  the client to render a result list and navigate directly to the corresponding detail page.
- **FR-407**: The client-facing API MUST expose only domain identifiers (e.g., title id, file id) in
  browse/search/detail responses, and MUST NOT expose raw filesystem paths (see NFR-601).

## FR-5xx — Playback, Streaming & Download

- **FR-501**: The server MUST serve file bytes for direct-play via HTTP Range requests, without
  server-side transcoding or remuxing of any kind
  ([ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md)).
- **FR-502**: The server MUST serve file bytes for full download as an attachment response, and MUST
  support Range requests to allow download resumption.
- **FR-503**: The server MUST NOT generate or serve HLS/DASH manifests or segments. Adaptive
  streaming exploration is deferred — tracked in
  [#75](https://github.com/justin13888/beam/issues/75).
- **FR-504**: Streaming and download endpoints MUST authenticate the request using the session
  cookie established per FR-103. The server MUST NOT accept a bearer or stream token supplied via URL
  query string.
- **FR-505**: For a title with multiple indexed file versions, the server MUST expose an endpoint
  (`/media/{id}/sources`) enumerating the available versions — including real probed per-stream
  codec information, resolution, container, and size — so the client can present a source-quality
  picker. Per-episode/show source enumeration is deferred — tracked in
  [#68](https://github.com/justin13888/beam/issues/68).
- **FR-506**: Switching between file versions during the source-selection scenario MUST result in
  direct-play of the newly selected file; the server MUST NOT perform any transcoding or format
  conversion to service the switch.
- **FR-507**: The server MUST track and persist per-user, per-file playback position ("resume point")
  as the client reports progress during playback.
- **FR-508**: The server MUST expose an endpoint returning the current user's in-progress items
  ordered by most-recently-watched, to back a "continue watching" feature.
- **FR-509**: The web client's player (Vidstack-based) MUST support seeking, keyboard shortcuts,
  visible buffering state, fullscreen, and Picture-in-Picture.
- **FR-510**: On resuming a previously started title, the web client MUST seek playback to the
  last-reported resume position (per FR-507) rather than starting from the beginning, subject to user
  override.

## FR-6xx — Administration

- **FR-601**: The server MUST provide an admin-only API surface for library root management (create,
  list, remove) and MUST reject these requests from non-admin authenticated users with an
  authorization error.
- **FR-602**: The server MUST provide an admin-only endpoint to trigger a library rescan, gated by
  admin role per FR-601's authorization behavior.
- **FR-603**: The server MUST provide an admin-only endpoint to trigger metadata re-enrichment
  (FR-308), gated by admin role.
- **FR-604**: The server MUST provide an admin-only endpoint or SSE stream exposing current scan
  progress, enrichment status, and recent system/admin log entries.
- **FR-605**: The web client MUST provide an admin area, visible only to users with the admin role,
  exposing library management, scan progress, enrichment status with a manual re-enrich control, and
  a log viewer.
- **FR-606**: The web client MUST NOT render admin-only navigation entries or controls to
  non-admin users, in addition to server-side authorization enforcement per FR-601–FR-604.
- **FR-607**: All admin-only mutating endpoints (library CRUD, rescan trigger, re-enrich trigger)
  MUST require both a valid authenticated session and the admin role — authentication alone MUST NOT
  be sufficient.

## FR-7xx — Client Behavior (Resume, Search, Player)

- **FR-701**: The web client MUST display a "continue watching" row on the home page, populated from
  the endpoint in FR-508, and MUST omit items that have been completed or explicitly cleared by the
  user.
- **FR-702**: The web client MUST periodically report playback position to the server during active
  playback (per FR-507) at a bounded interval, so that resume state survives a browser refresh or
  crash without an explicit "save" action.
- **FR-703**: The web client's instant search MUST debounce keystrokes before issuing a request, to
  avoid one request per keystroke against the search endpoint (FR-404).
- **FR-704**: The web client MUST present a source-quality picker in the player UI whenever a title
  has more than one indexed file version (per FR-505), and MUST default to the highest-quality
  version when no prior selection exists.
- **FR-705**: The web client MUST provide an explicit download action, distinct from the play action,
  that invokes the full-download endpoint (FR-502) rather than the streaming endpoint.
- **FR-706**: The web client MUST reflect real-time scan and enrichment progress (admin area) by
  consuming the SSE streams from FR-208/FR-309, and MUST NOT implement this via polling.
