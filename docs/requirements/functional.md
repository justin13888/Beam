# Beam — Functional Requirements

Requirements use RFC 2119 keywords (MUST, MUST NOT, SHOULD, SHOULD NOT, MAY) to indicate normative
strength. Each requirement is independently testable. See `product.md` for narrative context and
`non-functional.md` for cross-cutting quality attributes.

## FR-1xx — Authentication & Session

- **FR-101**: The server MUST support authentication exclusively via OpenID Connect (OIDC). No
  password-based login, registration, or "forgot password" flow exists
  ([ADR-0003](../architecture/decisions/ADR-0003-oidc-bff-auth.md)). The identity provider is
  always the operator's own; Beam ships no IdP for deployment
  ([ADR-0016](../architecture/decisions/ADR-0016-bring-your-own-idp.md)).
- **FR-102**: The server MUST implement the OIDC Authorization Code flow with PKCE, performed
  entirely server-side. The browser MUST NOT receive, store, or handle ID tokens, access tokens, or
  refresh tokens at any point. The same holds for a native client signing in by the device
  authorization grant (FR-111): no client ever receives an IdP token.
- **FR-103**: On successful OIDC authentication, the server MUST establish a session identified by an
  opaque session credential, `beam_session`. A browser receives it as a cookie, which MUST be
  `httpOnly` and MUST use `SameSite=Lax`; a native client signing in by FR-111 receives the same
  opaque value in the response body and presents it as that cookie. Either way it is one session
  row with the same idle and absolute expiry.
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
  `/v1/me`, and MUST treat a non-2xx response as "not authenticated."
- **FR-109**: The server MUST provide a logout endpoint that invalidates the server-side session and
  clears the session cookie.
- **FR-110**: For local development, the identity provider MUST be satisfiable by Dex running via
  `compose.dependencies.yaml`, configured with static test users, requiring no external network
  dependency. Dex MUST be opt-in rather than part of the default stack — Beam is
  bring-your-own-IdP (FR-101), so a default `compose up` MUST start no identity provider. When
  enabled, the bundled Dex MUST be reachable at one issuer URL that is valid both from the browser
  and from inside the server container.
- **FR-111**: The server MUST let a client with no browser sign in by the OAuth 2.0 device
  authorization grant (RFC 8628) against the configured IdP, run server-side:
  `POST /v1/auth/device` returns a user code, a verification URI and an opaque device handle, and
  `POST /v1/auth/device/token` polls with the handle, reaching the IdP at most once per poll interval
  and minting an FR-103 session on approval through the same admin (FR-106), provisioning (FR-105)
  and disabled-account rules as the browser callback. The IdP's device code MUST NOT leave the
  server, and only a hash of the handle MUST be stored. When the IdP's discovery document names no
  `device_authorization_endpoint`, `POST /v1/auth/device` MUST answer `501` so a client can fall
  back to the browser flow. See [ADR-0017](../architecture/decisions/ADR-0017-device-authorization-grant.md).

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
  Classification MUST read the file's path relative to its library root, folders included:
  - a season folder is one whose name carries a season word and number anywhere (`Season 01`,
    `Series 2`, `Saison`, `Staffel`, `Temporada`, `Breaking Bad Season 1`, `Season 1 (2008)`), a
    lone `S01` with no episode after it (`S01`, a season pack's `Show.S02.1080p`), or `Specials`
    (season 0); a folder carrying a range of seasons (`S01-S05`, `S01-05`, `Season(s) 1-5`,
    `Seasons 1 to 5`, optionally after `Complete`) is a multi-season pack, not a season folder, and
    the range ends the title it names (`Breaking.Bad.S01-S05.1080p` names *Breaking Bad*); a
    folder that is only a range (`Season 1-10`) names no title and is a pack inside its parent,
    which names the show as a season folder's parent does;
  - a show in a season folder is named by its series folder (the season folder's parent), unless
    the season folder's own text before its season token names another title or there is no
    series folder, when that text names it; else by the filename; a series folder that is the
    filename's show followed only by box-set words (`the`, `complete`, `series`, `collection`),
    or is only box-set words (`The Complete Series`), names the filename's show;
  - a show outside a season folder is named by the file's parent folder, unless the filename names
    another title (compared after identity normalisation), when the filename names it; with
    neither, `Unknown Show`;
  - an episode is numbered by an `SxxEyy`, `S01.E01` or `1x02` marker (of several, the last one
    before the first release-noise token, merged into a range with the markers written back to back
    before it for the same season with rising episodes), by an air date (`2024-03-01`: season =
    year, episode = `MMDD`), or by an absolute number (`Show - 012`, `E12`) only where the layout is
    unambiguous (a season folder, or a folder naming the same title); the `<title> - <n>` form also
    needs a season folder or a number of two or more digits; a year-shaped number (1900-2099) is an
    episode only in a season folder of the show the title names; inside a season folder only,
    `Episode N` / `Ep N` and a three- or four-digit number whose leading digits are the folder's
    season (`501`) number it too; a marker contradicting its season folder wins;
  - an episode's title is the text after its marker, else `Episode N`; a multi-episode file
    (`S01E01E02`, `S01E01-E03`, `S01E01.S01E02`) MUST attach to its first episode and record the
    last;
  - a movie is identified by title and year, taking its parent folder's year when the filename
    names the same title without one, and its parent folder's title when the filename's is empty or
    only release noise (never the library root's); each edition (a `{edition-...}` tag, or edition
    words such as `Director's Cut` or `Extended` after the title or its parenthesised year) of it in
    a library MUST be one `movie_entries` row however many copies exist;
  - a file in a season folder with no episode number, a file in a range-only folder with no season
    and episode marker, a `<title> - <n>` name no folder names as a show, and a fractional
    `<title> - <n>.<d>` (`Show - 12.5`), MUST be indexed without a title
    (status `unknown`) and reported through the admin log, never guessed into a movie.

  Every row MUST record the version of these rules that classified it, and a scan MUST reclassify a
  row classified by an older version from its path -- keeping its id, hash and probe results -- so a
  change to the rules reaches files indexed before it.
- **FR-205**: The server MUST support multiple indexed file versions (distinct `files` rows) under a
  single logical movie or episode entry, to support the source-selection delivery scenario.
- **FR-206**: The server MUST detect and de-duplicate files that have already been indexed, based on
  filesystem identity and modification-time change detection, without re-processing unchanged files
  on subsequent scans.
- **FR-207**: The server MUST support triggering a library scan from an admin action in the web
  client; no manual out-of-band process invocation is required.
- **FR-208**: The server MUST emit scan progress events (started, per-item progress, completed,
  failed) over Server-Sent Events (SSE) for consumption by the web client. Each MUST name its scan
  job and carry the job's per-file counts; per-item progress MAY be throttled (it is sent at most
  once a second), and progress events MUST NOT displace other events from the recent-event log.
- **FR-209**: The server MUST support adding and removing library root paths via an admin-facing API,
  without requiring a server restart or manual configuration file edit.
- **FR-210**: A library scan MUST complete (or fail) independently of metadata enrichment; enrichment
  MUST NOT block or extend the scan's completion.
- **FR-211**: A file the indexer can no longer find on disk MUST be soft-deleted rather than
  removed: its row is kept, stamped `missing_since`, and excluded from browse, search, detail
  sources, streaming, continue-watching and history (including the history total); if the path
  reappears the row MUST be restored under the same id, so its playback progress survives a
  transient absence (an unmounted NAS, USB disk or bind mount). A row MUST be purged only by a scan
  that was not refused and found the file still missing after the configurable grace period
  (`BEAM_MISSING_FILE_GRACE_DAYS`, measured with the injected `Clock`). A walk error shields rather
  than vetoes: a row beneath a path the walk could not read -- a directory it could not list, or a
  listed entry it could not stat for any reason other than "not found" -- MUST be left untouched
  (neither marked nor purged), and a walk error with no path MUST leave every row untouched; rows
  elsewhere in the same scan are marked and purged as normal. Every walk error MUST be reported
  through the admin log. A watcher event whose path cannot be statted for any reason other than
  "not found", or a watcher removal while the library root is unavailable, MUST change nothing.
- **FR-212**: Library roots MUST be pairwise disjoint and disjoint from `BEAM_DATA_DIR`: registering
  a root that is, contains, or lies inside an existing library root MUST be rejected with
  `library-path-overlaps-library` (409), and one that overlaps the data directory with
  `library-path-overlaps-data-dir` (400), compared by whole components after canonicalization. At
  startup the server MUST refuse to start if `BEAM_DATA_DIR` overlaps a stored library root, and
  MUST log a warning, not refuse, for stored library roots that overlap each other. The
  indexer and the watcher MUST NOT follow symbolic links beneath a library root; a link is not a
  library file, so a row whose path has become one is treated as missing (FR-211).
- **FR-213**: A library whose root is on a network filesystem, or whose native watch hit the OS
  watch limit, MUST be polled every `BEAM_WATCH_POLL_INTERVAL_SECS` instead of relying on native
  events, with no configuration switch, and MUST be scanned once when it starts being polled so
  changes made before polling began are not missed (for a library polled from startup, the startup
  scan is that scan: the background indexer MUST register every watch before the startup scan
  starts, and MUST schedule no single-library scan alongside it); a library MUST be watched as soon
  as it is created -- registered in the background, so creating it does not wait on a walk of its
  tree -- and stop being watched as soon as it is deleted, with the periodic maintenance cycle as
  the backstop for a registration that failed or landed after the delete; the admin status MUST report each library's watch mode and whether the watch limit has
  been reached. Every scan of a library is serialised (FR-218).
- **FR-214**: The indexer MUST match a file to an existing movie or show by an identity key derived
  from the filename parse -- the normalised title and year, and for a show its series folder --
  stored apart from the display title and never changed by enrichment, so a title enrichment renamed
  still receives its later files. Finding or creating a title by its key MUST be atomic: files of
  one new title indexed concurrently MUST resolve to one title. A key MUST record the version of
  the classification rules (FR-204) that derived it; before a scan reclassifies any file, a key an
  older version derived MUST be re-derived from the title's files, in place (the title keeping its
  id and enrichment), and two titles the current rules key alike MUST be merged into one, keeping
  the one a provider matched, else the older. Every path that reclassifies -- the scan of every
  library, the administrator's scan of one, a watcher event -- MUST first run the identity-key
  backfill and re-derivation if they have not succeeded in the process, and while they have not
  (they failed, and are retried by the next such path) MUST NOT reclassify a file an older version
  classified, still indexing new and changed files; a failed pass MUST be reported through the
  admin log. The passes MUST run while no scan or watcher reconcile classifies a file, so a key they
  move is never taken by a file indexed in between.
- **FR-215**: A movie or show with no present file MUST be excluded from browse and search as soon as
  its last file is soft-deleted (FR-211), while remaining resolvable by id; it MUST be deleted,
  with its enrichment state, only by a scan whose walk read the whole library and only once no file
  row -- present or soft-deleted -- is left for it.
- **FR-216**: The indexer MUST index only video files. It MUST NOT index, hash or probe hidden files
  or anything under a hidden folder; NAS and operating-system housekeeping folders (`@eaDir`,
  `#recycle`, `$RECYCLE.BIN`, `System Volume Information`, `lost+found`); DVD and Blu-ray disc
  structures (`VIDEO_TS`, `AUDIO_TS`, `BDMV`, `CERTIFICATE`, at any depth: playing one as its title
  is #189's); extras folders below the top level of a library (`Extras`, `Featurettes`, `Behind The
  Scenes`, `Deleted Scenes`, `Interviews`, `Sample(s)`, `Bonus`), and the extras folder names that
  can also name a category (`Scenes`, `Shorts`, `Trailers`, `Other`) only inside a title's folder --
  one naming a title and year, a season folder, or any folder below the top level of a library;
  files named as extras (`-trailer`, `-sample`, `.sample`, `_sample`, `-featurette`,
  `-behindthescenes`, `-deleted`, `-interview`, or exactly `sample`/`trailer`); or paths matching an
  administrator's `BEAM_SCAN_IGNORE` glob patterns, which live in server configuration, never in
  files written into a library (FR-202). An invalid pattern MUST fail startup. The full scan and the
  watcher MUST decide identically; a row for a path that is no longer indexed MUST be soft-deleted
  (FR-211).
- **FR-217**: A library root whose walk found video files -- including ones FR-216 excludes by name
  or by an ignore pattern matching the file -- MUST NOT be refused as unmounted by the empty-root
  guard; only a root with no such video files, under a library with indexed video files, is refused.
  The walk never descends into a folder FR-216 excludes (hidden, housekeeping, disc structure,
  extras, or matched by an ignore pattern), so video files under one are not counted: a root holding
  only those is refused. This errs toward refusing -- the safe side, since a refused scan changes no
  rows.
- **FR-218**: A library scan MUST be a job: requesting one MUST answer once the job is registered
  (`202` with the job) rather than when the scan finishes, and the latest job of each library MUST
  be readable. Every scan of a library -- administrator's, startup, periodic, newly polled -- and
  every watcher reconcile of it MUST be serialised: a request while a scan is queued or running MUST
  be refused with `library-scan-in-progress` (409), the periodic rescan MUST skip such a library,
  and a watcher event MUST be deferred and retried rather than wait. A file MUST NOT be hashed until
  it has gone `BEAM_SCAN_SETTLE_SECS` without a write (measured with the injected `Clock`), and a
  file whose probe failed MUST be probed again on each visit and classified when a probe succeeds;
  when a file's content changes and its probe fails, the old content's probe results and streams
  MUST be cleared rather than kept; when its size or modification time moved but its content did
  not, the new size and modification time MUST be recorded so it is not hashed again on the next
  visit. At most one `files` row MAY exist per path. Deleting a library MUST first stop anything
  from starting on it -- a scan of any trigger, or a watcher reconcile -- then fail a queued scan of
  it as cancelled at once and wait (bounded) for a running one to stop; if the delete itself then
  fails, the library MUST be scanned and reconciled again as before.
- **FR-221**: A file's `files` row MUST follow the file's content within its library. A path whose
  content hash (non-zero) and size match a row of the same library whose own content has left its
  path -- the path is gone, the row is marked missing (FR-211) and its path not walked, or the path
  now holds different content -- MUST be relinked to that row, whether the path is new or already
  has a row of its own. This covers a move or a rename, two files swapping names, a rotation among
  several, and a rename onto the path of a row already missing. A relinked row keeps its id and so
  its playback progress, its movie or episode, and its streams, and MUST NOT be probed or classified
  again. A path whose content matches no such row keeps the existing behaviour: an indexed path's
  row has changed content, a new path gets a new row, and a file whose original is still at its path
  is a copy with a row of its own. A full scan MUST decide every path it walked at once,
  deterministically -- every path gets at most one row and every row at most one path -- and MUST
  apply the relinks atomically, one row per path holding when they are done. Among several rows for
  one path, and several paths for one row, the pairing with the same file name wins, then the same
  directory, then the row played most recently by anyone (a row never played last), then the lowest
  row id, then the lowest path. One pairing is declined, a replace-by-rename: when the row a path
  has -- present or missing -- would otherwise be displaced (its own content is at no path the scan
  hashed, or that path went to a better pairing), has been played by anyone, and the row whose
  content is now at that path never has, the path MUST keep its own row, its new content a change to it as for any other
  path, and the other row MUST NOT take the path; that row is paired elsewhere if the scan finds its
  content elsewhere, and otherwise is marked missing like any gone file. Every other case follows
  the order above. A row whose path a relink takes and whose content is at no path the
  scan hashed MUST NOT be deleted: it is soft-deleted beside its old path, where it can still be
  relinked by content and is purged after the grace period like any missing row. A watcher event
  MUST relink a new path to a row whose file is gone, whichever of a move's two events is reconciled
  first; one whose content matches a row of the library that may have moved -- marked missing, or
  its path not holding what the row recorded -- and that it cannot relink without displacing a row
  MUST be left untouched for the next scan rather than guessed at. Every relink MUST be reported
  through the admin log with both paths, and a scan's progress MUST count relinked files. A watcher
  event naming a directory MUST reconcile what is beneath it: a directory renamed or moved in is
  walked and each file reconciled, and the rows beneath a removed directory whose files are gone are
  soft-deleted -- unless the library root holds no video file, which reads as an unmounted volume
  as it does for the scan (FR-211), and leaves the directory to the next scan; the watcher never
  purges.

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
- **FR-310**: The server MUST serve every piece of artwork itself — movie and show posters and
  backdrops, season posters, episode thumbnails — from `/v1/artwork/{kind}/{id}/{variant}`, fetching
  each image from the provider once and caching it on disk. `poster_url`, `backdrop_url` and
  `thumbnail_url` MUST carry that path, never a provider URL, so no client contacts TMDB or AniList
  ([ADR-0015](../architecture/decisions/ADR-0015-artwork-served-by-beam.md), NFR-501). A title with
  no artwork, an unknown id, and a variant that does not apply to that kind of title MUST all be
  `404`; clients render a placeholder.
- **FR-311**: An artwork URL MUST remain stable when enrichment refreshes a title's art, so that a
  client which stored one — a download record, rendered offline — still resolves. Freshness is
  carried by the `ETag` instead, which is derived from the provider URL and therefore changes
  exactly when the artwork does.

## FR-4xx — Browse, Search & Detail

- **FR-401**: The web client MUST provide a library browsing view listing movies and shows with
  poster art, title, and, where available, genres and rating. The browse endpoint returns each
  title's genres, rating and external identifiers, and each show's season and episode counts, so
  a tile needs no further request; it accepts a `genre` filter by name or slug
  ([#187](https://github.com/justin13888/beam/issues/187)).
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
- **FR-503**: The server MUST NOT generate or serve HLS/DASH manifests or segments. This is
  settled, not deferred: both formats are defined over segments, so producing them for a library's
  existing containers means repackaging bytes — request-time remuxing or an index-time derived
  artifact, each of which FR-501 and
  [ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md) exclude. Adaptive bitrate over
  independently indexed encodes is separately unachievable, since their keyframes do not align. See
  [ADR-0014](../architecture/decisions/ADR-0014-adaptive-streaming-rejected.md) for the full
  argument and for the client-side work that answers the compatibility and bandwidth problems
  instead.
- **FR-504**: Streaming and download endpoints MUST authenticate the request using the session
  cookie established per FR-103. The server MUST NOT accept a bearer or stream token supplied via URL
  query string.
- **FR-505**: For a title with multiple indexed file versions, the server MUST expose an endpoint
  (`/media/{id}/sources`) enumerating the available versions — including real probed per-stream
  codec information, resolution, container, and size — so the client can present a source-quality
  picker. The endpoint accepts a movie id or an episode id; a show id is rejected, since shows have
  no files of their own.
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
- **FR-511**: When the operator enables playback telemetry (`BEAM_PLAYBACK_TELEMETRY_ENABLED`), the
  server MUST accept authenticated batches of playback starts, start failures (with a reason --
  container, video codec, audio codec, network, other -- and a stage -- preflight or playback),
  mid-stream rebuffers with their duration, and source switches (manual or automatic), and MUST
  count each only as a daily aggregate under coarse dimensions derived server-side from the file it
  names (client kind, container, codecs, resolution class, bitrate class), discarding the file and
  the reporting user (NFR-503). A file it cannot resolve MUST be dropped, not refused. When
  telemetry is disabled the endpoint MUST refuse with a distinct 409 so clients stop reporting.

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
- **FR-608**: The server MUST provide an admin-only endpoint that returns the anonymous library
  report it would send now -- the report and the exact request body, byte for byte -- with whether a
  destination is configured and when the report was last and will next be sent, without sending
  anything (NFR-503).
- **FR-609**: The server MUST provide an admin-only endpoint that returns the playback telemetry
  counts (FR-511) summed over a caller-chosen range of UTC days (default the last 30, at most 366),
  whether or not collection is currently enabled, and MUST prune counts older than the configured
  retention (`BEAM_PLAYBACK_TELEMETRY_RETENTION_DAYS`) daily.

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
