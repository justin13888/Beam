# API Architecture

Beam exposes exactly one client-facing API: a domain-specific REST API, versioned under `/v1`,
specified via Salvo's OpenAPI integration, with real-time admin events over SSE. There is no
GraphQL endpoint — see [ADR-0002](decisions/ADR-0002-rest-only-api.md) for why REST-only.

## Surface

All routes live under `/v1` and (except `/v1/health` and the OIDC login/callback pair) require the
`beam_session` cookie (see `security.md`). Admin routes additionally require the resolved admin
role.

| Route | Method(s) | Purpose |
|---|---|---|
| `/v1/health` | GET | Deep health check (public): probes the database and returns `200` `{status:"healthy"}` or `503` `{status:"degraded"}` with per-dependency `checks` and process `uptime_secs` |
| `/v1/media` | GET | Browse/search catalog (cursor pagination, filters, sort) |
| `/v1/media/{id}` | GET | Full metadata for one movie or show |
| `/v1/media/{id}/sources` | GET | Playable/downloadable source files for a movie, with probed per-stream codecs |
| `/v1/libraries`, `/v1/libraries/{id}`, `/v1/libraries/{id}/files` | GET | Library listing and contents |
| `/v1/files/{fileId}/stream` | GET | Direct-play byte-range streaming (see `streaming.md`) |
| `/v1/files/{fileId}/download` | GET | Full-file download (attachment) |
| `/v1/files/{fileId}/progress` | PUT | Report playback position |
| `/v1/continue-watching` | GET | Resume list for the current user |
| `/v1/auth/login`, `/v1/auth/callback` | GET | OIDC login redirect and callback |
| `/v1/me` | GET | Current user |
| `/v1/logout`, `/v1/logout-all` | POST | End this session / all sessions |
| `/v1/sessions`, `/v1/sessions/{id}` | GET, DELETE | List / revoke own sessions |
| `/v1/admin/libraries`, `/v1/admin/libraries/{id}`, `/v1/admin/libraries/{id}/scan` | POST, DELETE, POST | Library management and scan trigger |
| `/v1/admin/media/{id}/refresh` | POST | Re-trigger enrichment for a title |
| `/v1/admin/logs`, `/v1/admin/logs/count` | GET | Admin log view |
| `/v1/admin/events` | GET | Recent admin events (JSON) |
| `/v1/admin/events/stream` | GET | Admin event stream (SSE) |

`GET /v1/media/{id}/sources` reports the real probed codec of each stream, mapped to API-visible
values (`hevc`/`h264`/`av1` → `H265`/`H264`/`AV1`; `aac`/`opus`; anything unrecognized is
`UNKNOWN`). It supports movies only; show/episode sources are deferred — tracked in
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
- **Errors:** every non-2xx response uses one uniform JSON body, `{"error": "<message>"}`, with the
  HTTP status code (`400`, `401`, `403`, `404`, `500`) carrying the semantics. Handlers return
  `500` (never panic) when injected state is missing.

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
[ADR-0002](decisions/ADR-0002-rest-only-api.md).
