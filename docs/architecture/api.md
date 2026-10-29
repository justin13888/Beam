# API Architecture

Beam exposes exactly one client-facing API: a domain-specific REST API, versioned under `/v1`,
described by one **OpenAPI 3.2** document, with real-time admin events over SSE. There is no
GraphQL endpoint — see [ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md) for why REST-only.

The HTTP runtime is Kynos ([ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md); the gate-by-gate
migration record is in [kynos-migration-readiness.md](kynos-migration-readiness.md)). The property
that matters here is that **routing and description come from one declaration**:
`routes::create_router` is walked once to build the dispatch table and once to emit the document,
and the process refuses to start on a router it cannot describe. There is no second pass to keep in
step, so an operation cannot be served without appearing in the spec, or documented without being
served.

The document is 3.2 rather than 3.1 for one reason: `GET /v1/admin/events/stream`. Only 3.2 can
describe a sequential body, and describing the SSE stream honestly is worth more than compatibility
with tooling that has not caught up — see "OpenAPI docs and codegen" for what that currently costs.

## Surface

All routes live under `/v1` and (except `/v1/health` and the OIDC login/callback pair) require the
`beam_session` cookie (see `security.md`). Admin routes additionally require the resolved admin
role.

Every route that reads or delivers a title applies the caller's access policy (issue #263; see
`security.md`): browse lists only the titles it admits, the library reads only the granted
libraries, continue-watching and history drop the rows it hides, and every route given the id of a
hidden title -- or of its season, episode, file, subtitle or art -- answers exactly as for an id
naming nothing, with the same `404` and problem type. A title's detail and browse row carry the
provider's certification as `content_rating` (absent while none is known). The `/v1/admin`
operations are not filtered.

What hangs off titles follows them (issue #328). The watchlist and favourites list and count only
admitted titles -- a hidden title's entry is kept, and listed again once the policy admits it --
and refuse a hidden id with `404` `media-not-found`. People search lists only people credited on an
admitted title, a filmography only those titles, and a person credited on none answers `404`
`person-not-found`. The collection listing holds only collections with an admitted movie, each
`movie_count` counting those alone, and a collection with none answers `404`
`collection-not-found`. The genre list holds only genres an admitted title carries. A portrait or
a collection's art is served exactly while its person or collection is. The stores filter in their
queries, so a page still holds `first` items and every `total` counts only what the caller sees.

| Route | Method(s) | Purpose |
|---|---|---|
| `/v1/health` | GET | Deep health check (public): probes the database and returns `200` `{status:"healthy"}` or `503` `{status:"degraded"}` with per-dependency `checks` (each `{status: "ok" \| "error", detail}`, `detail` the failure reason), `checked_at` and process `uptime_secs` |
| `/v1/media` | GET | Browse/search catalog (cursor pagination, filters, sort) |
| `/v1/media/{id}` | GET | Full metadata for one movie or show, with its `cast` (billing order) and key `crew` (by department and job) from enrichment, a movie's `collection` (`{id, name}`, absent when it is in none), and the caller's `user_state` -- `in_watchlist` and `in_favourites` included; the credits and the collection are absent in a browse result |
| `/v1/collections` | GET | The provider collections (TMDB `belongs_to_collection`, e.g. a franchise) that at least one of the library's movies is in, by name ignoring ASCII case: a `CollectionConnection` paged by `first` (1-100, default 20) and `after`, with `total`; each with its `movie_count` and artwork paths |
| `/v1/collections/{id}` | GET | One collection (`CollectionDetail`) and its `movies` as browse results with the caller's state, in release order (year, then release date, a movie with neither last); `404` `collection-not-found` for an unknown id or a collection no movie is in any more |
| `/v1/people` | GET | People search: the cast and crew credited in the library whose name contains `query` (ignoring case; every credited person without it), by name. A `PersonConnection` paged by `first` (1-100, default 20) and `after`, with `total`; shares the `search` rate-limit class with `/v1/media` |
| `/v1/people/{id}` | GET | One person (`PersonDetail`) and their filmography: `cast` and `crew` credits in the library, newest title first; `404` `person-not-found` for an unknown id or a person no title credits any more |
| `/v1/media/{id}/sources` | GET | Playable/downloadable source files for a movie or an episode, primary first, each with its video, audio and subtitle tracks |
| `/v1/genres` | GET | Every genre a title the caller may see carries, for filter chips: a `GenreConnection`, never paged |
| `/v1/artwork/{kind}/{id}/{variant}` | GET, HEAD | Poster, backdrop or thumbnail art, people's portraits and collections' art, fetched from the provider once and served from Beam's cache. `kind` is `movie`/`show`/`season`/`episode`/`person`/`collection`; `variant` is `poster`/`backdrop`/`thumbnail`/`profile` (a person has only `profile`, a collection `poster` and `backdrop`) |
| `/v1/libraries`, `/v1/libraries/{id}`, `/v1/libraries/{id}/files` | GET | Library listing (a `LibraryConnection`, never paged) and contents: a `LibraryFileConnection` paged by `first` (1-500, default 100) and `after`, by stored path then id. File paths are relative to the library root, in the listing and inside its cursor (NFR-108); another library's cursor is `400 #invalid-cursor` |
| `/v1/files/{fileId}/stream` | GET, HEAD | Direct-play byte-range streaming (see `streaming.md`) |
| `/v1/files/{fileId}/download` | GET, HEAD | Full-file download (attachment) |
| `/v1/files/{fileId}/subtitles/{subtitleId}` | GET, HEAD | A subtitle file beside the video, as stored, Range-capable; `404` `subtitle-not-found` for an id this file's sources do not list ([ADR-0020](decisions/ADR-0020-text-subtitle-delivery.md)) |
| `/v1/files/{fileId}/subtitles/{subtitleId}/webvtt` | GET, HEAD | The same SubRip or WebVTT file as WebVTT, for a browser's `<track>`; `404` `subtitle-rendition-unavailable` for ASS, SSA or a file over 8 MiB |
| `/v1/files/{fileId}/trickplay/{sheet}` | GET, HEAD | One scrub-preview sheet of the file (`image/jpeg`, Range-capable), from Beam's data directory ([#265](https://github.com/justin13888/beam/issues/265)); `404` `trickplay-not-found` while the file has no sheets for its current content, when it gives none, or past its last sheet. The source's `trickplay.sheet_urls` names exactly the sheets this answers |
| `/v1/media/{id}/progress` | GET, DELETE | The caller's state for a movie or an episode (`UserTitleState`; all zeros if never played) / forget its resume position, keeping it played. `400` `progress-not-available-for-show` for a show or season id |
| `/v1/media/{id}/watched` | PUT, DELETE | Mark a movie, episode, season or show watched (each episode of a season or show, idempotently) / unwatched (forgotten entirely); `204` |
| `/v1/episodes/{id}`, `/v1/seasons/{id}` | GET | One episode (`EpisodeDetail`: its season, show and the episodes either side by next-up's rules: `(season, episode)` order, playable episodes only, the next after the run its primary file holds, and never back from a numbered season into season 0) or one season (`SeasonDetail`), with the caller's state |
| `/v1/files/{fileId}/progress` | PUT | Report playback position; kept for the file's title and answered as its `UserTitleState`. `404` `file-not-found` for a file that is no title's, `422` `validation-failed` for a negative position, a non-positive duration or a position past the end (FR-507) |
| `/v1/continue-watching` | GET | One row per movie to resume and per show at its next-up episode, newest first, with display fields: a `ContinueWatchingConnection` of at most `first` (1-50, default 20) rows, with no cursors: `page_info.has_next_page` says whether candidates were left unexamined -- the rows filled first, or the load reached its 1,000-title ceiling (FR-508) |
| `/v1/continue-watching/{id}` | DELETE | Hide a title (an episode or season id hides its show) until it is next played (FR-701); `204` |
| `/v1/history` | GET | Every movie and episode the caller played or marked, newest first: a `HistoryConnection` paged by `first`/`after` with `total` |
| `/v1/watchlist`, `/v1/favourites` | GET | The caller's watchlist or favourites, newest addition first: a `UserListConnection` of `{added_at, media}` items (`media` a browse result, with the caller's state for a movie) paged by `first` (1-100, default 50) and `after`, with `total`. Each list's cursor names it, so the other list's is `400 #invalid-cursor` |
| `/v1/watchlist/{id}`, `/v1/favourites/{id}` | PUT, DELETE | Put a movie or show on the list / take it off; `204` either way, idempotently (a title already there keeps its place). `400` `not-a-title` for a season or episode id, `404` `media-not-found` for an id naming nothing |
| `/v1/telemetry/playback` | POST | Report a batch of playback starts, start failures, rebuffers and source switches (at most 50 events). Counted as daily aggregates under the named files' coarse dimensions, never the file or the user; `409` `playback-telemetry-disabled` unless the operator enabled it, `422` `validation-failed` with per-pointer `errors` ([ADR-0019](decisions/ADR-0019-telemetry-posture.md)) |
| `/v1/auth/login`, `/v1/auth/callback` | GET | OIDC login redirect and callback |
| `/v1/auth/device` | POST | Start a device login (RFC 8628) for a client with no browser: user code, verification URI, opaque device handle. `501` when the IdP does not offer the grant ([ADR-0017](decisions/ADR-0017-device-authorization-grant.md)) |
| `/v1/auth/device/token` | POST | Poll a device login once: `202` while waiting (`authorization_pending` / `slow_down`), `200` with the `beam_session` value on approval |
| `/v1/me` | GET | Current user |
| `/v1/me/language-preferences` | GET, PUT | The caller's `LanguagePreferenceSettings` (issue #264): `audio_languages` and `subtitle_languages`, each most preferred first and at most 16, and `subtitle_mode` (`off`, `forced_only`, `on`). A user who set nothing has empty lists and `forced_only`. `PUT` replaces the whole and answers what is stored, every language as its ISO 639-2/B code (`de`, `deu`, `German` and `de-AT` are all `ger`), a repeat keeping its first place; `422` `validation-failed` for an entry naming no language (`/audio_languages/N`, `/subtitle_languages/N`). Read by `/v1/media/{id}/sources` |
| `/v1/logout`, `/v1/logout-all` | POST | End this session / all sessions; a session-store failure is `500 #internal` and leaves the cookie in place, so the client can retry |
| `/v1/sessions`, `/v1/sessions/{id}` | GET, DELETE | List (a `SessionConnection`, never paged) / revoke own sessions; an id naming none of the caller's is `404 #session-not-found`, another user's included |
| `/v1/admin/status` | GET | Dashboard snapshot: version, uptime, counts, enrichment progress, each metadata provider's configuration (`configured`, `not_configured`, `unavailable`, FR-307), recent scans, and the filesystem watcher's per-library mode (native, polling and why, unwatched) with the watch limit |
| `/v1/admin/users` | GET | User accounts, oldest first: an `AdminUserConnection` paged by `first` (1-100, default 50) and `after`, with `total` |
| `/v1/admin/users/{id}` | PATCH | Block or unblock an account |
| `/v1/admin/users/{id}/access` | GET, PUT | What one user may see (`UserAccess`): `all_libraries`, the granted `library_ids` otherwise, and `max_content_rating` (a `ContentRatingBand`, absent for none). `PUT` replaces the whole policy and answers what is stored; `422` `validation-failed` for a library id naming nothing (`/library_ids/N`) or ids beside `all_libraries: true` (`/library_ids`); `404` `user-not-found` |
| `/v1/admin/libraries`, `/v1/admin/libraries/{id}` | POST, DELETE | Library management; deleting a library cancels its scan and waits for it to stop (at most 30 s on the injected clock) before its rows go, then forgets its latest scan job |
| `/v1/admin/libraries/{id}/scan` | POST, GET | Start a scan: `202` with the scan job, queued, and the scan runs in the background; `409` `library-scan-in-progress` while one is queued or running. `GET` reads the latest job since the server started (`404` `scan-not-found` before one). `{id}` is a UUID (`format: uuid`); a malformed one is the path extractor's `400` `about:blank` |
| `/v1/admin/media/{id}/refresh` | POST | Queue a title for another enrichment pass, keeping its match, or with `?rematch=true` discarding it to be matched afresh (`204`) |
| `/v1/admin/enrichment` | GET | Titles by enrichment status (`status`, `kind` filters), newest change first, with the last error (FR-303): a `MediaEnrichmentConnection` paged by `first`/`after` with `total` |
| `/v1/admin/media/{id}/enrichment` | GET | One title's enrichment: status, match, pin and who set it, locked fields |
| `/v1/admin/media/{id}/match-candidates` | GET | The configured providers' candidates for a title, best first, scored as the worker scores them (`query`, `year` override the title's own); `409` `provider-not-configured`, `502` `enrichment-provider-error` |
| `/v1/admin/media/{id}/match` | POST, DELETE | Fix a title's match: `{"external_ref": "tmdb:603"}` pins it as the administrator's (outranks and is never replaced by an NFO, FR-312) and queues it: `202` with the title; `422` `validation-failed`, `409` `provider-not-configured` / `external-ref-taken`. `DELETE` clears an administrator's pin back to the NFO's pin or to a search and queues the title; on a title no administrator pinned it changes nothing (`204` either way) |
| `/v1/admin/media/{id}/enrichment/locks` | PUT | Lock exactly the listed fields, so enrichment leaves them alone; `422` for a show's `release_date`/`runtime` |
| `/v1/admin/media/refresh`, `/v1/admin/libraries/{id}/refresh` | POST | Queue every title, or one library's -- a title with no enrichment row yet is given one -- for another pass (FR-308), keeping each match unless `?rematch=true`: `202` `{queued_count}` |
| `/v1/admin/logs` | GET | The operational log, newest first: an `AdminLogConnection` paged by `first` (1-200, default 50) and `after`, with `total` |
| `/v1/admin/events` | GET | The most recent admin events, oldest first: an `AdminEventConnection` of at most `first` (1-1000, default 100), with no cursors; `has_next_page` says older events were left out |
| `/v1/admin/events/stream` | GET | Admin event stream (SSE) |
| `/v1/admin/telemetry/library` | GET | The anonymous library report, and its exact OTLP request body, as it would be sent now -- sends nothing ([ADR-0019](decisions/ADR-0019-telemetry-posture.md)) |
| `/v1/admin/telemetry/playback` | GET | Playback telemetry counts summed over `from`..`to` (UTC days, inclusive; default the last 30, at most 366, else `400` `invalid-date-range`) -- operator-local, never sent anywhere |

Three routes sit outside `/v1` and outside the client contract: `GET /metrics` (Prometheus text
exposition, tagged `internal` — see `../operations/deployment.md`), `GET /openapi` (the Scalar UI)
and `GET /api-doc/openapi.json` (the document). They are described operations rather than hidden
handlers: Kynos routes and describes from one declaration, so the alternative to describing them
would be waiving the whole document's authority.

`GET /v1/media/{id}/sources` is the one place tracks are described
([#189](https://github.com/justin13888/beam/issues/189)). It answers a `MediaSourceConnection`
-- `items` and a `page_info`, the connection shape `GET /v1/media` has, but never paged: `items` is
every source and neither `has_*_page` flag is ever `true` -- listing a title's sources primary first:
the default edition, then the tallest picture, the highest video bit rate, the largest file, the
lowest file id -- ranked when read, not stored. Each source carries `is_primary`, its `edition`, an
`episode_span` when the file holds a run of episodes (its duration is then the run's), its `parts`,
and its `video_tracks`, `audio_tracks` and `subtitle_tracks`. `parts` lists the files the source
plays, in order: one for a whole file, and each part of a movie split across files (`Movie (2019) -
CD1.avi`, `- CD2.avi`, [#233](https://github.com/justin13888/beam/issues/233)) otherwise, each with
its `part_number`, size, duration, subtitle tracks, `stream_url` and `download_url`. A client is
expected to play a multi-part source as a playlist of its parts' streams; the server never joins
them ([ADR-0004](decisions/ADR-0004-never-transcode.md)). The web player does
([#138](https://github.com/justin13888/beam/issues/138)); the Android app still plays the source's
top-level `stream_url`, its first part, alone. The parts of a folder are one source only as the whole run
1, 2, ... n; any other set leaves each file a source of its own. A DVD or Blu-ray folder rip is a
source too ([#234](https://github.com/justin13888/beam/issues/234), FR-222): its `parts` are the
stream files the disc's main title plays -- VOBs or `.m2ts` clips, served as they are, and across
the discs of a set of one film each disc's in disc order (decision D234-7) -- and its
`disc_structure` (`dvd` or `blu_ray`, absent for any other source, and for a set whose discs are
not all of one kind) says so, so a client that cannot
play MPEG program streams or BDAV transport streams lists it as not directly playable. The source's `size_bytes` and
`duration_secs` are all its parts'; its `file_id`, tracks and URLs are its first part's. Every track has its stream `index` and
FFmpeg's own `codec` name (`h264`, `hevc`, `eac3`, `truehd`, `subrip`, `hdmv_pgs_subtitle`); a value
the file does not state -- a frame rate, a bit rate, a sample rate -- is absent rather than a
default. Subtitle tracks are the file's own, by index, then the subtitle files beside it
(`origin: sidecar`, a `sidecar_id`, a `url`, and a `webvtt_url` for a SubRip or WebVTT file of at
most 8 MiB); `is_text` says whether a client can render a track itself. An embedded track has no
URL: Beam never extracts one ([ADR-0020](decisions/ADR-0020-text-subtitle-delivery.md)). Each
source names the tracks to start on for the caller
([#264](https://github.com/justin13888/beam/issues/264)), by their
`/v1/me/language-preferences`: `default_audio_track_index`, a position in `audio_tracks` (from 0)
-- the first preferred audio language the source carries, the file's default track within it, else
the file's default track, else the first; absent only with no audio -- and
`default_subtitle_track_index`, a position in `subtitle_tracks`, absent to start without subtitles.
`off` picks none; `forced_only` the forced track in the default audio track's language; `on` a full
(not forced) track in the first preferred subtitle language carried -- with none listed, the full
track the file marks default -- else what `forced_only` picks. A tie within one language prefers
the file's default, then not SDH, then text, then list order. Each part carries its own
`default_subtitle_track_index` into its own `subtitle_tracks`, chosen beside the source's default
audio. The choice is `beam_domain::utils::track_selection`, a pure function. Each source and
part also carries its file's `chapters` (`start_ms`, `end_ms`, `title`), the `markers` read from
the chapters titled for a skippable span (`recap`, `intro`, `credits`, `preview`; at most one of
each, by start, `beam_domain::models::chapter::markers_from_chapters`), and -- once the background
worker has made them for the file's current content -- `trickplay`, the layout of its JPEG
scrub-preview sheets and their `sheet_urls` ([#265](https://github.com/justin13888/beam/issues/265)).
`chapters` and `markers` are `default`ed, so a client generated from this document still reads an
older server. It accepts
a movie id or an episode id; a show id is rejected with 400, since shows have no files of their
own. Episode sources landed in [#102](https://github.com/justin13888/beam/pull/102), closing
[#68](https://github.com/justin13888/beam/issues/68).

`GET /v1/media/{id}` no longer carries tracks: a movie has `file_id` and `duration_secs` from its
primary source and a `source_count` (absent from browse, which reads no files), and each episode
the same, its `duration_secs` absent when its primary file spans several episodes.

`GET /v1/artwork/{kind}/{id}/{variant}` is what `poster_url`, `backdrop_url` and `thumbnail_url`
point at: those fields carry this path, never a provider URL, so a viewer's client never contacts
TMDB or AniList ([ADR-0015](decisions/ADR-0015-artwork-served-by-beam.md), NFR-501). The path is
stable across re-enrichment — a client that stored one, such as an offline download record, still
resolves it — and freshness rides on a strong `ETag` derived from the provider URL, which changes
exactly when a title's artwork does. Conditional requests are honoured, so a client that holds the
current validator gets a `304`. A title with no artwork, an id that does not exist, and a variant
that does not apply to that kind of title (a season has no backdrop, an episode no poster) are all
`404`; every client renders a placeholder for that.

## Conventions

- **Versioning:** all routes are prefixed `/v1`, and `/v1` is frozen: a breaking change gets a
  `/v2` prefix rather than mutating `/v1` in place
  ([ADR-0021](decisions/ADR-0021-v1-frozen.md)). The document's `info.version` is the release
  (`CARGO_PKG_VERSION`), not the contract's version; the release workflow regenerates both
  committed copies on the release pull request.
- **Identifiers:** resource identifiers in paths are opaque UUIDs, never filesystem paths — see
  `security.md`.
- **Actions:** operations that don't map to CRUD are sub-resource verbs, not query-string RPC
  flags — e.g. re-enrichment is `POST /v1/admin/media/{id}/refresh`.
- **Pagination:** `GET /v1/media` uses Relay-style cursor pagination
  (`first`/`after`/`last`/`before`), returning a `MediaConnection` of `items` and `page_info`
  (`has_next_page`, `has_previous_page`, `start_cursor`, `end_cursor`). Cursor pagination is used
  because indexing and enrichment continuously mutate the result set, where offset pagination
  would skip or duplicate items. The semantics
  ([#187](https://github.com/justin13888/beam/issues/187)):
  - A request pages **forwards** with `first` (default 20) and optionally `after`, or
    **backwards** with `last` (default 20) and optionally `before`; a page holds 1 to 100 items.
    Mixing the two directions is `400 #invalid-pagination`; a size outside 1-100 is refused
    before the handler by Kynos's query check, as `400 about:blank` naming the parameter. A backward
    page is still returned in display order.
  - Forwards, `has_next_page` says whether another page follows and `has_previous_page` whether
    `after` was given; backwards, the mirror. Pass `end_cursor` as `after` for the next page and
    `start_cursor` as `before` for the previous one.
  - A cursor is opaque base64url JSON holding the sort it was issued under and the boundary
    title's sort key, kind and id (`services/cursor.rs`). It is a **position, not an offset**:
    a page after a title that has since left the listing starts where that title was. A cursor
    that is not the server's, or was issued under another `sort_by`/`sort_order`, is
    `400 #invalid-cursor`. It is unsigned: everything in it is either public or checked.
  - Every `sort_by` (`title`, `year`, `rating`, `date_added`, `runtime`, `relevance`) orders in the database in
    both directions, a title with no value for the field sorting **last** either way, ties broken
    by id, then kind. A show has no runtime and sorts with the films that lack one; `date_added` is
    when the title was first indexed. Title order is `lower(title)` under the database collation.
    A `query` keeps the requested sort; `sort_by=relevance` ranks by it instead: each title's
    trigram distance from the `query` (`pg_trgm`'s `<->`), so `asc` -- the default -- lists the
    best match first and `desc` the worst, ties broken by id, then kind, as in every sort. Its
    cursor carries that distance. `sort_by=relevance` without a `query` has nothing to rank by and
    is `400 #invalid-search-query`, and so is a `query` holding a NUL character, which no title can
    contain.
  - Filters apply to movies and shows alike, `min_rating` included; `genre` matches by name or
    slug. `min_rating` is on the 0-10 scale `ratings.tmdb` is reported on, compared in the single
    precision a rating is stored in, an unrated title counting as 0. A numeric filter out of its
    range -- a year above 2147483647 (the `year` columns are signed), a `min_rating` of `NaN` --
    is `400 #invalid-filter`, never narrowed into a bound that matches everything; a `min_rating`
    outside 0-10 is refused before the handler as `400 about:blank`. Only titles with a present
    file are listed.
  - One page is one catalogue statement plus a fixed number of reads by id to hydrate the page
    (titles, genres, season and episode counts), whatever the library's size (NFR-301). The
    statement is a `UNION ALL` over `movies` and `shows` in which each branch filters, seeks,
    orders and limits itself, and the outer query merges the two. For `title` and `date_added`
    each branch reads its own index in order (`(lower(title), id)`, `(created_at, id)`), checking
    liveness and the filters per row. Unfiltered or filtered only by `media_type`, it stops at the
    page size. A selective `genre`, `query`, `min_rating` or `year`/`year_from`/`year_to` filter
    can leave most rows it reads unmatched, so such a page may read up to the whole index: its cost
    is bounded by the catalogue, not the page. `year`, `rating` and `runtime` are unindexed and sort every matching title. A
    browsed show carries `season_count`/`episode_count` and no `seasons`; its detail carries both.
  - A database failure is `500 #internal`, on browse and on detail — never an empty page, and
    never a `404` for a title that could not be read.

  `GET /v1/admin/enrichment` ([#185](https://github.com/justin13888/beam/issues/185)) is the
  same shape forwards only (`first`, 1-100, default 20, and `after`), plus `total`. Its order is
  fixed -- the most recently changed row first, `updated_at` then row id, both descending -- so its
  cursor (`services/enrichment_cursor.rs`) holds just that pair, at full precision, and is valid
  under any `status`/`kind` filter. A page is one keyset statement on
  `idx_metadata_enrichment_list`, one count and one read by ids per kind to name the titles.

  **Every list is a connection** ([#190](https://github.com/justin13888/beam/issues/190), wire
  rule R3): `items` and `page_info`, plus `total` where the list counts itself, never a bare array.
  The rest page the same way, forwards only:
  - `GET /v1/libraries/{id}/files` by `first` (1-500, default 100) and `after`, ordered by stored
    path then id, through `FileRepository::find_page_by_library` -- a keyset seek, so a page costs
    one statement whatever the library's size. Its cursor (`services/library_files_cursor.rs`)
    holds the root-relative path the page reported, never the absolute one (NFR-108), and its
    library's id, so another library's cursor is `400 #invalid-cursor`. No `total`: the library's
    `file_count` is it.
  - `GET /v1/watchlist` and `GET /v1/favourites` by `first` (1-100, default 50) and `after`, with
    `total`, newest addition first: `added_at` then entry id, both descending, as a keyset on
    `idx_user_list_entries_page`. Their cursor (`services/user_list_cursor.rs`) holds that pair at
    full precision and names its list, so the watchlist's cursor is `400 #invalid-cursor` on
    favourites.
  - `GET /v1/people` and `GET /v1/collections` (`first` 1-100, default 20) with `total`, by name
    ignoring ASCII case, through the offset cursor below.
  - `GET /v1/admin/users` (`first` 1-100, default 50) and `GET /v1/admin/logs` (`first` 1-200,
    default 50) with `total`. Both read by `LIMIT`/`OFFSET` underneath, so the offset rides inside
    an opaque cursor (`services/offset_cursor.rs`) naming its list: the other list's cursor is
    `400 #invalid-cursor`, and either can move to a keyset later without a client noticing. An
    entry logged while an admin pages shifts the log's later pages by one. The log's count is its
    `total`; there is no separate count operation.
  - `GET /v1/libraries`, `GET /v1/sessions`, `GET /v1/genres` and `GET /v1/media/{id}/sources` are
    small enough never to page: `items` is the whole list and `page_info` one complete page.
    `GET /v1/admin/events` and `GET /v1/continue-watching` are bounded shelves: `first` caps them,
    and `has_next_page` alone says more existed.
- **Errors:** every failure is an **RFC 9457 problem document** — one body shape for every status,
  on every route, including Kynos's own extractor rejections. There is no second envelope and no
  hook to render one, which closed half of [#123](https://github.com/justin13888/beam/issues/123):
  the Salvo implementation had four error enums rendering three shapes chosen by endpoint rather
  than by status, plus a framework catcher whose shape was none of them.

  `type` is a **stable, machine-readable URI** and it names the *condition*, not the status.
  `media-not-found`, `library-not-found`, `user-not-found`, `file-not-found` and
  `source-file-missing` are five codes that were one `not-found`; that last one — the catalogue has
  the file, the disk does not — is the one an operator can act on, and a client that cannot tell it
  from the others sends a viewer looking for something they cannot fix. Codes that merely restate
  the status were the remaining half of #123: they gave a client nothing beyond status plus
  endpoint, which is the complaint the issue opened with.

  Every code hangs under `ERROR_BASE` in `routes/api_error.rs`, which is **anchor-shaped** —
  `https://beam.justinchung.net/reference/errors/#` — so each `type` dereferences to the section of
  `beam-docs`' [`reference/errors`](https://beam.justinchung.net/reference/errors/) describing it.
  It was previously a path prefix, and no such path has ever been served, so every identifier Beam
  published resolved to a 404. Branch on `type`, or on status; `detail` stays human-facing and
  non-contractual. On a `500 #internal`, a `502 #enrichment-provider-error` and a
  `503 #oidc-unavailable` it is one fixed sentence per code, never the cause (NFR-108): those
  variants hold a `ServerFault`, whose constructor records the cause as a `tracing` `error` event at
  the failing site and which has no `Display` to put it back on the wire.

  The URI is written out per variant rather than derived from the variant name. Kynos can compose
  one from `#[problem(base = ...)]` plus the variant's name, but then a rename silently changes the
  published contract with no string to diff, and the same code is deliberately emitted from several
  enums — `internal` from nearly every one of them — which an implicit convention would hide.
  `routes/taxonomy_tests.rs` reads every `type` the exported document declares, asserts each starts
  with `ERROR_BASE`, and asserts the set of codes matches the set of sections on the published page,
  in both directions.

  **The codes are part of the OpenAPI document.** Every problem response a Beam code can reach
  narrows `type` to a `const`,
  and where several variants -- or an extractor's `about:blank` -- share one status, to a `oneOf` of
  const-narrowed problems. A generated client therefore sees every code, and `codegen:openapi:check`
  catches a renamed one on its own. The consequence to know: spargen lowers each narrowed `type` to
  a closed set, so a native client decodes only the codes it was generated with. A code the server
  adds later reaches an older client as an undecodable body with no status, not as the documented
  status it arrived with ([getkono/spargen#268](https://github.com/getkono/spargen/issues/268)). The
  same holds for a `429` whose body is not Beam's `rate-limited` problem — a reverse proxy's own rate
  limiter, or a server older than the code — on the three operations that declare one. Two
  framework responses stay wide on purpose: `SessionAuth`'s `403` and the range engine's `416` are
  the bare `Problem` component, so any `type` decodes there.

  The error types are a **family, one per operation shape**, not one union. Kynos derives an
  operation's `responses` from its return type, so a shared union would make `GET /v1/genres`
  advertise a `416` it cannot reach. Sharpening the codes made the split load-bearing rather than
  merely tidy: Kynos describes a status by every variant declaring it, so a shared `MutationError`
  holding both `MediaNotFound` and `LibraryNotFound` would tell a client that `deleteLibrary` can
  answer `media-not-found`. `401` and `403` are
  absent from almost all of them because they arrive from the `SessionAuth`/`AdminAuth` extractors,
  which is what makes taking the extractor and documenting the requirement one act.

  **Framework-originated statuses carry `about:blank` where the status is the whole story.** For
  the extractor `400`/`415`/`422`, `RangeRejection`'s `416`, the `404`/`405` fallback and
  `SessionAuth`'s `401` that is RFC 9457's own reading. The two framework responses with a next step
  of their own are named: the **admin `403`** is `admin-required`, which `SessionAuthenticator::authorize`
  sends through `AuthRejection::forbidden_as` and the `Admin` scope set declares as its
  `FORBIDDEN_TYPE`; the rate limiter's **`429`** is `rate-limited`, which the limiter names through
  `RateLimit::problem_type`.

  Where several components declare the same status on one operation, Kynos joins their titles into
  the response's description in the order it meets them — extractors and authenticators before the
  handler's return type — and narrows `type` to the union of their codes.

  Statuses in use: `400`, `401`, `403`, `404`, `409` and `422` (each with a named code, wire rule
  R10, except Kynos's `422` for a request body that does not decode), `416` (stream/download
  range), `429` (rate limiter,
  with `Retry-After` and `X-RateLimit-Limit`/`-Remaining`/`-Reset`), `500`, `502` (artwork only:
  `artwork-upstream-failed`, when the metadata provider answered the image fetch unusably or not at
  all — the fault is the provider's, and `internal` is documented to mean it is Beam's), `503`
  (health, `/v1/auth/login` when OIDC is unconfigured, and `/metrics` when metrics are disabled). A wrong
  method on a real path returns `405`. The `416` is declared on `RuntimeDelivery<R>`
  (`routes/delivery.rs`) — the generic delivery type file delivery and artwork both return — rather
  than on `DeliveryError` or `ArtworkError`, because `Served::deliver` resolves an unsatisfiable
  range into a problem document and returns it as an `Ok` delivery — the handler never sees an error
  to convert. `GET /v1/health`
  is the one endpoint that answers with a domain body rather than a problem document on failure: it
  renders the full `HealthStatus` JSON on both `200` and `503`, because a monitor needs the
  per-dependency `checks`. This bullet is the contract; `beam-docs`'
  [`reference/errors`](https://beam.justinchung.net/reference/errors/) is its public explanation,
  and a change to any error enum is not complete until both are updated.

## OpenAPI docs and codegen

The OpenAPI 3.2 document is served at `/api-doc/openapi.json`, with a Scalar interactive docs UI at
`/openapi`. Clients are generated, never hand-written:

1. `mise run codegen:openapi` runs `export_openapi` in `beam-server`, which builds the router with
   no database, no listener and no initialized service — `Router::openapi_as` reads the same
   declarations `Router::build` routes on — and writes the document to `beam-web/openapi.json` and
   `beam-client-core/api/openapi.json`. Both copies are committed, and
   `mise run codegen:openapi:check` (part of `mise run ci`) fails when either differs from what the
   router emits, so a wire change is a reviewable diff rather than a silent regeneration.
2. `spargen` (0.4.0, 3.2-native) generates the Rust client `beam-client-core` exposes to the native
   clients over UniFFI — see [ADR-0012](decisions/ADR-0012-native-client-rust-core.md).
3. `openapi-typescript` converts the document to `beam-web/src/api.gen.ts`, and `openapi-fetch`
   provides the thin typed client `beam-web` consumes.

`openapi-typescript` reads the 3.2 document through `@redocly/openapi-core`, whose `detectSpec`
accepts 3.2 only from 1.34.22 on; 7.13.0 asks for `^1.34.6`, so the root `package.json` raises the
floor with an `overrides` entry inside that range
([#146](https://github.com/justin13888/beam/issues/146)). Redocly reads 3.2 as 3.1, so the
generated types do not model 3.2-only keywords: the SSE operation's `itemSchema` gives
`useAdminEventStream` no typed payload, and it reads `AdminEvent` by name instead. `api.gen.ts` is
gitignored; `mise run codegen:openapi:web` regenerates it from the committed document, and
`ts:typecheck` and `ts:build:web` run that first, so the TypeScript compiler checks `beam-web`
against the contract `codegen:openapi:check` pins to the router.

### Wire conventions (frozen /v1)

`/v1` is frozen ([ADR-0021](decisions/ADR-0021-v1-frozen.md)), and these are the conventions it is
frozen in. `beam-server/src/routes/conventions_tests.rs` is the contract: it walks the exported
document and fails on any violation, with no exception list -- a schema that cannot comply is
changed, not waived ([#190](https://github.com/justin13888/beam/issues/190)). Each rule is also
shown to catch its violation in a document built for the purpose, so none passes for free. This
section describes them; when the two disagree, the test is right:

- **R1** every enum value and every `const` is `snake_case` (`^[a-z][a-z0-9_]*$`). A problem
  document's `type` is a URI by RFC 9457, not a value from a set; the taxonomy test governs it;
- **R2** no schema name ends in `Dto` or `Response` — a name is the thing on the wire. Where a wire
  type shares its name with the domain type it mirrors (`AdminEvent`, `AdminLogCategory`,
  `TitleKind`), the domain type is reached through its module;
- **R2b** no two enums carry the same set of values: one meaning is one enum (`LogLevel` serves the
  admin log and the admin event stream alike; `TitleKind` is the browse filter, the
  continue-watching and history `media_type`, and an enrichment row's `kind`);
- **R4** every identifier -- a value named `id` or `*_id` -- is `format: uuid`, in a body and in a
  path or query parameter alike, so a malformed path id is the framework's plain `400` on every
  route. `ExternalIdentifiers` is outside it: its `imdb_id`, `tmdb_id` and `tvdb_id` are other
  systems' identifiers, in their shapes;
- **R5** a value is named `*_at` if and only if it is an instant (`format: date-time`); a value
  naming a day (`date`, `*_date`, `*_on`, `*_aired`) is `format: date`, not a date-time at a
  made-up midnight; nothing is an integer epoch or named like one (`timestamp`, `*_time`,
  `*_epoch`, `*_unix`);
- **R6** a number measuring a duration, runtime, size, position or length names its unit:
  `_secs`, `_mins`, `_ms`, `_days`, `_bytes` or `_count` (`duration_secs`, `runtime_mins`,
  `size_bytes`, `file_count`);
- **R3** no response body is a bare JSON array: every list is a `*Connection` (see Pagination
  above), so it can page, or say it does not, without a breaking change;
- **R8** `info.version` is the release, `CARGO_PKG_VERSION`, in the document served and in both
  committed copies;
- **R10** a `409` or `422` names its problem `type` from the taxonomy, never leaving it open or
  `about:blank`. The one `about:blank` allowed is Kynos's own `422` for a request body that does
  not decode, on an operation that takes one.

**R9**, no single-key `oneOf` wrapper, is not enforced yet. `MediaMetadata` is the one schema it
would refuse -- `{"Movie": {...}}` rather than a `media_type` tag beside the fields -- and the
tagged form waits on a spargen release that dispatches on the tag the server sends
([getkono/spargen#403](https://github.com/getkono/spargen/issues/403),
[#300](https://github.com/justin13888/beam/issues/300)).

R4-R6 read every property of every component schema and every path and query parameter. Headers
are transport, described by Kynos rather than by Beam's types, and are not asserted
([#200](https://github.com/justin13888/beam/issues/200)); so is the SSE frame Kynos describes
around an event (its `id` is the event-stream field, not a Beam identifier); the event payload
inside it is a component and is read.

## Server-Sent Events

Real-time admin events (scan progress, enrichment outcomes, system events) are delivered over SSE at
`GET /v1/admin/events/stream`, authenticated by the same session cookie as every other request
(`EventSource` is a normal same-origin HTTP request). Each event carries a small JSON payload, and
the operation **describes it**: the `text/event-stream` response declares an OpenAPI 3.2
`itemSchema` for the SSE envelope and `contentMediaType`/`contentSchema` for the JSON in each
event's `data`. Under Salvo this operation's `200` had no content type and no schema at all.
Authentication resolves before the stream is committed — `AdminAuth` is an extractor, so a `401` or
`403` is a normal response rather than an error arriving after a `200` is already on the wire.
Standard `EventSource` reconnection semantics apply; the server does not replay missed events — a
reconnecting client re-fetches current state via `GET /v1/admin/events` or the corresponding REST
resource.

A running scan publishes `scan_progress` events (FR-208, FR-218): `started`, per-item `progress` at most
once a second, then `completed` or `failed`, each carrying a `scan` object with the job id and the
per-file counts. They are live state rather than history, so they go out on the stream only and are
never kept in the recent-event snapshot `GET /v1/admin/events` returns — one scan of a large library
would otherwise push every other event out of it. The job itself (`GET
/v1/admin/libraries/{id}/scan`) holds every file's count, unthrottled. Scan jobs live in memory, the
latest per library, and a restart forgets them.

Each title an enrichment sweep attempts publishes one `enrichment` event (FR-309): enriched, left
unmatched, to be retried, or failed, carrying an `enrichment` object with the title's id, kind,
display title, status, match and error. Live-only too, for the same reason: a sweep of a large
library would flush the snapshot, and where every title stands is `GET /v1/admin/enrichment`. A
title the sweep never reached -- it stopped for a provider's rate limit -- sends nothing. SSE was
chosen over WebSockets because the channel is strictly server-to-client — see
[ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md).
