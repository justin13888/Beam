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
| `/v1/libraries`, `/v1/libraries/{id}`, `/v1/libraries/{id}/files` | GET | Library listing and contents |
| `/v1/files/{fileId}/stream` | GET, HEAD | Direct-play byte-range streaming (see `streaming.md`) |
| `/v1/files/{fileId}/download` | GET, HEAD | Full-file download (attachment) |
| `/v1/files/{fileId}/progress` | PUT | Report playback position |
| `/v1/continue-watching` | GET | Resume list for the current user |
| `/v1/history` | GET | Watch history for the current user (limit/offset paged) |
| `/v1/auth/login`, `/v1/auth/callback` | GET | OIDC login redirect and callback |
| `/v1/me` | GET | Current user |
| `/v1/logout`, `/v1/logout-all` | POST | End this session / all sessions |
| `/v1/sessions`, `/v1/sessions/{id}` | GET, DELETE | List / revoke own sessions |
| `/v1/admin/status` | GET | Dashboard snapshot: version, uptime, counts, enrichment progress, recent scans |
| `/v1/admin/users` | GET | User accounts (limit/offset paged) |
| `/v1/admin/users/{id}` | PATCH | Block or unblock an account |
| `/v1/admin/libraries`, `/v1/admin/libraries/{id}`, `/v1/admin/libraries/{id}/scan` | POST, DELETE, POST | Library management and scan trigger |
| `/v1/admin/media/{id}/refresh` | POST | Re-trigger enrichment for a title |
| `/v1/admin/logs`, `/v1/admin/logs/count` | GET | Admin log view |
| `/v1/admin/events` | GET | Recent admin events (JSON) |
| `/v1/admin/events/stream` | GET | Admin event stream (SSE) |

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

## Conventions

- **Versioning:** all routes are prefixed `/v1`. A future breaking change gets a `/v2` prefix
  rather than mutating `/v1` in place.
- **Identifiers:** resource identifiers in paths are opaque UUIDs, never filesystem paths — see
  `security.md`.
- **Actions:** operations that don't map to CRUD are sub-resource verbs, not query-string RPC
  flags — e.g. re-enrichment is `POST /v1/admin/media/{id}/refresh`.
- **Pagination:** `GET /v1/media` uses Relay-style cursor pagination
  (`first`/`after`/`last`/`before`), returning a connection object with items and page info.
  Cursors are opaque server-generated tokens; cursor pagination is used because indexing and
  enrichment continuously mutate the result set, where offset pagination would skip or duplicate
  items.
- **Errors:** every failure is an **RFC 9457 problem document** — one body shape for every status,
  on every route, including Kynos's own extractor rejections. There is no second envelope and no
  hook to render one, which is what closed [#123](https://github.com/justin13888/beam/issues/123):
  the Salvo implementation had four error enums rendering three shapes chosen by endpoint rather
  than by status, plus a framework catcher whose shape was none of them.

  `type` is a **stable, machine-readable URI** under
  `https://beam.justinchung.net/reference/errors/` (`ERROR_BASE` in `routes/api_error.rs`), written
  out per variant rather than derived from the variant name, so the same failure carries the same
  code whichever operation it reaches a client through. Branch on `type`, or on status; `detail`
  stays human-facing, non-contractual, and often an interpolated internal message.

  The error types are a small family, not one union, and that is deliberate: Kynos derives an
  operation's `responses` from its return type, so a shared union would make `GET /v1/genres`
  advertise a `416` it cannot reach and turn that into dead retry logic in a generated client. Each
  operation names the narrowest type covering what it can actually answer with —
  `InternalError`, `LookupError`, `InputError`, `MutationError`, `DeliveryError`. `401` and `403`
  are absent from all of them because they arrive from the `SessionAuth`/`AdminAuth` extractors,
  which is what makes taking the extractor and documenting the requirement one act.

  Statuses in use: `400`, `401`, `403`, `404`, `416` (stream/download range), `429` (rate limiter,
  with `Retry-After` and `X-RateLimit-Limit`/`-Remaining`/`-Reset`), `500`, `503` (health,
  `/v1/auth/login` when OIDC is unconfigured, and `/metrics` when metrics are disabled). A wrong
  method on a real path returns `405`. Two statuses were corrected by the migration: a file
  resolving outside its library root is a `403` (`DeliveryError::Forbidden`), where the Salvo
  handler collapsed it into a `401`; and the same-origin check's `403` is now declared on every
  operation it covers rather than existing only at run time. `GET /v1/health` is the one endpoint
  that answers with a domain body rather than a problem document on failure: it renders the full
  `HealthStatus` JSON on both `200` and `503`, because a monitor needs the per-dependency `checks`.
  This bullet is the contract; `beam-docs`'
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
`#118-followup` TODO, and the schema keys in `api.gen.ts` still use the pre-migration dotted names.
While that holds, the TypeScript compiler is **not** a contract check for `beam-web`; the Rust side
of the contract is checked by `codegen:openapi:check` and by the router refusing to build if it
cannot describe itself.

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
resource. SSE was chosen over WebSockets because the channel is strictly server-to-client — see
[ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md).
