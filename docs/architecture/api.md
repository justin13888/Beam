# API Architecture

Beam exposes exactly one client-facing API: a domain-specific REST API, versioned under `/v1`,
specified via Salvo's OpenAPI integration, with real-time admin events over SSE. There is no
GraphQL endpoint — see [ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md) for why REST-only.

The deployed implementation remains Salvo. [ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md)
ratifies OpenAPI 3.2 and Kynos as its replacement, but migration is blocked until the
[Kynos readiness contract](kynos-migration-readiness.md) is satisfied. This page continues to
describe current runtime behavior until that migration lands.

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
| `/v1/files/{fileId}/stream` | GET | Direct-play byte-range streaming (see `streaming.md`) |
| `/v1/files/{fileId}/download` | GET | Full-file download (attachment) |
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
- **Errors:** there are **no machine-readable error codes**. The HTTP status carries the semantics
  and the body carries a free-form, unstable message — often an interpolated internal error — so
  clients branch on status plus endpoint, never on message text. **Three body shapes exist**, chosen
  by endpoint rather than by status. The `/v1` REST routes (`media`, `genres`, `libraries`,
  `continue-watching`, `history`, `files/{id}/progress`, `admin/*`) and the rate limiter render JSON
  `{"error": "<message>"}` (`ApiError`/`ApiErrorBody` in `routes/api_error.rs`, `RateLimiter` in
  `routes/rate_limit.rs`). The file-delivery routes (`FileDeliveryError`, `routes/stream.rs`), the
  OIDC routes (`OidcCallbackError`/`OidcAuthError`, `beam-auth/src/server/oidc_routes.rs`) and the
  same-origin hoop (`routes/middleware.rs`) render `text/plain`. `GET /v1/health` renders the full
  `HealthStatus` JSON on both `200` and `503`. Statuses in use: `400`, `401`, `403`, `404`, `416`
  (stream/download range), `429` (rate limiter, with `Retry-After`), `500`, and `503` (health, and
  `/v1/auth/login` when OIDC is unconfigured). On the file-delivery path a forbidden condition is
  reported as `401`, not `403` (`routes/stream.rs`). A request matching no route at all is answered
  by Salvo's default catcher, whose shape (nested `{"error":{"code",…}}`, or HTML by `Accept`) is
  none of the three above; a wrong method on a real path returns `405`. Handlers return `500` (never
  panic) when injected state is missing. This bullet is the contract; `beam-docs`'
  [`reference/errors`](https://beam.justinchung.net/reference/errors/) is its public explanation,
  and a change to any error enum is not complete until both are updated.

## OpenAPI docs and codegen

The generated OpenAPI 3.x spec is served at `/api-doc/openapi.json`, with a Scalar interactive docs
UI at `/openapi`. The TypeScript client is generated, never hand-written:

1. `cargo run --example export_openapi` (in `beam-server`) builds the router without a database and
   serializes the spec to `beam-web/openapi.json` — the spec is derived from handler annotations,
   not runtime introspection.
2. `openapi-typescript` converts it to `beam-web/src/api.gen.ts`, the single source of truth for
   wire types.
3. `openapi-fetch` provides the thin, fully-typed HTTP client `beam-web` consumes.

A server-side API change that isn't reflected in the regenerated client types fails TypeScript
compilation — the compiler is the contract check.

## Server-Sent Events

Real-time admin events (scan progress, enrichment outcomes, system events) are delivered over SSE at
`GET /v1/admin/events/stream`, authenticated by the same session cookie as every other request
(`EventSource` is a normal same-origin HTTP request). Each event carries a small JSON payload.
Standard `EventSource` reconnection semantics apply; the server does not replay missed events — a
reconnecting client re-fetches current state via `GET /v1/admin/events` or the corresponding REST
resource. SSE was chosen over WebSockets because the channel is strictly server-to-client — see
[ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md).
