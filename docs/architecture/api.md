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

| Route | Method(s) | Purpose |
|---|---|---|
| `/v1/health` | GET | Deep health check (public): probes the database and returns `200` `{status:"healthy"}` or `503` `{status:"degraded"}` with per-dependency `checks` and process `uptime_secs` |
| `/v1/media` | GET | Browse/search catalog (cursor pagination, filters, sort) |
| `/v1/media/{id}` | GET | Full metadata for one movie or show |
| `/v1/media/{id}/sources` | GET | Playable/downloadable source files for a movie or an episode, with probed per-stream codecs |
| `/v1/genres` | GET | Every genre in the catalog, for filter chips |
| `/v1/artwork/{kind}/{id}/{variant}` | GET, HEAD | Poster, backdrop or thumbnail art, fetched from the provider once and served from Beam's cache. `kind` is `movie`/`show`/`season`/`episode`; `variant` is `poster`/`backdrop`/`thumbnail` |
| `/v1/libraries`, `/v1/libraries/{id}`, `/v1/libraries/{id}/files` | GET | Library listing and contents (file paths are relative to the library root, NFR-108) |
| `/v1/files/{fileId}/stream` | GET, HEAD | Direct-play byte-range streaming (see `streaming.md`) |
| `/v1/files/{fileId}/download` | GET, HEAD | Full-file download (attachment) |
| `/v1/files/{fileId}/progress` | PUT | Report playback position |
| `/v1/continue-watching` | GET | Resume list for the current user |
| `/v1/history` | GET | Watch history for the current user (limit/offset paged) |
| `/v1/telemetry/playback` | POST | Report a batch of playback starts, start failures, rebuffers and source switches (at most 50 events). Counted as daily aggregates under the named files' coarse dimensions, never the file or the user; `409` `playback-telemetry-disabled` unless the operator enabled it, `422` `validation-failed` with per-pointer `errors` ([ADR-0019](decisions/ADR-0019-telemetry-posture.md)) |
| `/v1/auth/login`, `/v1/auth/callback` | GET | OIDC login redirect and callback |
| `/v1/auth/device` | POST | Start a device login (RFC 8628) for a client with no browser: user code, verification URI, opaque device handle. `501` when the IdP does not offer the grant ([ADR-0017](decisions/ADR-0017-device-authorization-grant.md)) |
| `/v1/auth/device/token` | POST | Poll a device login once: `202` while waiting (`authorization_pending` / `slow_down`), `200` with the `beam_session` value on approval |
| `/v1/me` | GET | Current user |
| `/v1/logout`, `/v1/logout-all` | POST | End this session / all sessions |
| `/v1/sessions`, `/v1/sessions/{id}` | GET, DELETE | List / revoke own sessions |
| `/v1/admin/status` | GET | Dashboard snapshot: version, uptime, counts, enrichment progress, each metadata provider's configuration (`configured`, `not_configured`, `unavailable`, FR-307), recent scans, and the filesystem watcher's per-library mode (native, polling and why, unwatched) with the watch limit |
| `/v1/admin/users` | GET | User accounts (limit/offset paged) |
| `/v1/admin/users/{id}` | PATCH | Block or unblock an account |
| `/v1/admin/libraries`, `/v1/admin/libraries/{id}` | POST, DELETE | Library management; deleting a library cancels its scan and waits for it to stop (at most 30 s on the injected clock) before its rows go, then forgets its latest scan job |
| `/v1/admin/libraries/{id}/scan` | POST, GET | Start a scan: `202` with the scan job, queued, and the scan runs in the background; `409` `library-scan-in-progress` while one is queued or running. `GET` reads the latest job since the server started (`404` `scan-not-found` before one). `{id}` is a UUID (`format: uuid`); a malformed one is the path extractor's `400` `about:blank` |
| `/v1/admin/media/{id}/refresh` | POST | Queue a title for another enrichment pass, keeping its match (`204`) |
| `/v1/admin/enrichment` | GET | Titles by enrichment status (`status`, `kind` filters), newest change first, with the last error (FR-303): a `MediaEnrichmentConnection` paged by `first`/`after` with `total` |
| `/v1/admin/media/{id}/enrichment` | GET | One title's enrichment: status, match, pin and who set it, locked fields |
| `/v1/admin/media/{id}/match-candidates` | GET | The configured providers' candidates for a title, best first, scored as the worker scores them (`query`, `year` override the title's own); `409` `provider-not-configured`, `502` `enrichment-provider-error` |
| `/v1/admin/media/{id}/match` | POST, DELETE | Fix a title's match: `{"external_ref": "tmdb:603"}` pins it as the administrator's (outranks and is never replaced by an NFO, FR-312) and queues it: `202` with the title; `422` `validation-failed`, `409` `provider-not-configured` / `external-ref-taken`. `DELETE` clears an administrator's pin back to the NFO's pin or to a search, and queues the title (`202`) |
| `/v1/admin/media/{id}/enrichment/locks` | PUT | Lock exactly the listed fields, so enrichment leaves them alone; `422` for a show's `release_date`/`runtime` |
| `/v1/admin/media/refresh`, `/v1/admin/libraries/{id}/refresh` | POST | Queue every title, or one library's, for another pass (FR-308): `202` `{queued_count}` |
| `/v1/admin/logs`, `/v1/admin/logs/count` | GET | Admin log view |
| `/v1/admin/events` | GET | Recent admin events (JSON) |
| `/v1/admin/events/stream` | GET | Admin event stream (SSE) |
| `/v1/admin/telemetry/library` | GET | The anonymous library report, and its exact OTLP request body, as it would be sent now -- sends nothing ([ADR-0019](decisions/ADR-0019-telemetry-posture.md)) |
| `/v1/admin/telemetry/playback` | GET | Playback telemetry counts summed over `from`..`to` (UTC days, inclusive; default the last 30, at most 366, else `400` `invalid-date-range`) -- operator-local, never sent anywhere |

Three routes sit outside `/v1` and outside the client contract: `GET /metrics` (Prometheus text
exposition, tagged `internal` — see `../operations/deployment.md`), `GET /openapi` (the Scalar UI)
and `GET /api-doc/openapi.json` (the document). They are described operations rather than hidden
handlers: Kynos routes and describes from one declaration, so the alternative to describing them
would be waiving the whole document's authority.

`GET /v1/media/{id}/sources` reports the real probed codec of each stream, mapped to API-visible
values (`hevc`/`h264`/`av1` → `H265`/`H264`/`AV1`; `aac`/`opus`; anything unrecognized is
`UNKNOWN`). It accepts a movie id or an episode id; a show id is rejected with 400, since shows
have no files of their own. Episode sources landed in
[#102](https://github.com/justin13888/beam/pull/102), closing
[#68](https://github.com/justin13888/beam/issues/68).

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

- **Versioning:** all routes are prefixed `/v1`. A future breaking change gets a `/v2` prefix
  rather than mutating `/v1` in place.
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
    Mixing the two directions, or a size outside 1-100, is `400 #invalid-pagination`. A backward
    page is still returned in display order.
  - Forwards, `has_next_page` says whether another page follows and `has_previous_page` whether
    `after` was given; backwards, the mirror. Pass `end_cursor` as `after` for the next page and
    `start_cursor` as `before` for the previous one.
  - A cursor is opaque base64url JSON holding the sort it was issued under and the boundary
    title's sort key, kind and id (`services/cursor.rs`). It is a **position, not an offset**:
    a page after a title that has since left the listing starts where that title was. A cursor
    that is not the server's, or was issued under another `sort_by`/`sort_order`, is
    `400 #invalid-cursor`. It is unsigned: everything in it is either public or checked.
  - Every `sort_by` (`title`, `year`, `rating`, `date_added`, `runtime`) orders in the database in
    both directions, a title with no value for the field sorting **last** either way, ties broken
    by id, then kind. A show has no runtime and sorts with the films that lack one; `date_added` is
    when the title was first indexed. Title order is `lower(title)` under the database collation.
    A `query` keeps the requested sort rather than ranking by relevance. A `query` holding a NUL
    character, which no title can contain, is `400 #invalid-search-query`.
  - Filters apply to movies and shows alike, `min_rating` included; `genre` matches by name or
    slug. Only titles with a present file are listed.
  - One page is one catalogue statement plus a fixed number of reads by id to hydrate the page
    (titles, genres, season and episode counts), whatever the library's size (NFR-301). The
    statement is a `UNION ALL` over `movies` and `shows` in which each branch filters, seeks,
    orders and limits itself, and the outer query merges the two. For `title` and `date_added`
    each branch reads its own index in order (`(lower(title), id)`, `(created_at, id)`), checking
    liveness and the filters per row. Unfiltered or filtered only by `media_type`, it stops at the
    page size. A selective `genre`, `query` or `min_rating` filter can leave most rows it reads
    unmatched, so such a page may read up to the whole index: its cost is bounded by the catalogue,
    not the page. `year`, `rating` and `runtime` are unindexed and sort every matching title. A
    browsed show carries `season_count`/`episode_count` and no `seasons`; its detail carries both.
  - A database failure is `500 #internal`, on browse and on detail — never an empty page, and
    never a `404` for a title that could not be read.

  `GET /v1/admin/enrichment` ([#185](https://github.com/justin13888/beam/issues/185)) is the
  same shape forwards only (`first`, 1-100, default 20, and `after`), plus `total`. Its order is
  fixed -- the most recently changed row first, `updated_at` then row id, both descending -- so its
  cursor (`services/enrichment_cursor.rs`) holds just that pair, at full precision, and is valid
  under any `status`/`kind` filter. A page is one keyset statement on
  `idx_metadata_enrichment_list`, one count and one read by ids per kind to name the titles.
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
  published resolved to a 404. Branch on `type`, or on status; `detail` stays human-facing,
  non-contractual, and often an interpolated internal message.

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

  Statuses in use: `400`, `401`, `403`, `404`, `416` (stream/download range), `429` (rate limiter,
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

**Step 3 is currently switched off, and `api.gen.ts` is stale.** `openapi-typescript` 7.13.0 cannot
read a 3.2 document: it delegates to `@redocly/openapi-core` 1.34.8, whose `detectSpec` knows only
3.0 and 3.1 and throws `Unsupported OpenAPI version: 3.2.0`. Redocly 2.x speaks 3.2; the generator
has not bumped to it. Since `beam-web` is being rewritten shortly, it is stood down rather than
worked around: the client codegen step and the `ts:typecheck`/`ts:test` gates carry a
[#146](https://github.com/justin13888/beam/issues/146) TODO, and the schema keys in `api.gen.ts`
still use the pre-migration dotted names. While that holds, the TypeScript compiler is **not** a
contract check for `beam-web`; the Rust side of the contract is checked by `codegen:openapi:check`
and by the router refusing to build if it cannot describe itself.

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
