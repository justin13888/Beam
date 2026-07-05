# API Architecture

Status: target architecture. See [ADR-0002](decisions/ADR-0002-rest-only-api.md) for the decision
this document details.

## One API, REST, OpenAPI-first

Beam exposes exactly one client-facing API: a domain-specific REST API, versioned under `/v1`,
specified via Salvo's OpenAPI integration. This replaces today's dual-stack setup entirely — the
async-graphql schema/resolvers/guards, its WebSocket subscription handler, and the `export_schema`
binary are all deleted. There is no GraphQL endpoint, no GraphQL Playground, in the target
architecture.

**Changed from today:** today, browse/media/admin operations go through async-graphql (with GraphQL
subscriptions over WebSocket for real-time updates) while auth and streaming go through a separate
Salvo REST/OpenAPI surface — two parallel stacks, each separately codegen'd to TypeScript. This push
collapses everything onto the REST/OpenAPI stack, which already exists for the auth/streaming slice
today and is extended to cover what GraphQL used to serve, rather than inventing a new pipeline.

## Resource naming and versioning

- All routes are prefixed `/v1/...`. A future breaking change gets a `/v2` prefix rather than
  mutating `/v1` in place; this is already the repo's convention and is kept as-is.
- Resources are plural nouns: `/v1/libraries`, `/v1/movies`, `/v1/shows`, `/v1/files`, `/v1/sessions`.
  Nested resources reflect real containment: `/v1/shows/{showId}/seasons/{seasonNumber}/episodes`.
- Resource identifiers in paths are opaque IDs (UUIDs), never filesystem paths — see `security.md`.
- Actions that don't map cleanly to CRUD on a resource are modeled as a sub-resource verb, not a
  query-string RPC flag: e.g. re-triggering enrichment is `POST /v1/movies/{movieId}/enrichment`
  (creates a new enrichment attempt), not `POST /v1/movies/{movieId}?action=reenrich`.

## Pagination

List endpoints (`GET /v1/movies`, `GET /v1/shows`, `GET /v1/search`, etc.) use **cursor-based
pagination**, chosen over offset/limit because the underlying result sets change during normal
operation (indexing and enrichment continuously insert/update rows) and offset pagination is prone to
skipped/duplicated items under concurrent writes.

Request shape: `GET /v1/movies?limit=50&cursor=<opaque-cursor>`. Response shape:

```json
{
  "items": [ /* resource objects */ ],
  "nextCursor": "<opaque-cursor-or-null>"
}
```

The cursor is an opaque, server-generated token (in practice, an encoded `(sort-key, id)` pair) —
clients must not attempt to construct or interpret it, only pass it back verbatim. `limit` has a
server-enforced maximum regardless of what the client requests.

## Error response shape

Every non-2xx response uses one consistent JSON error shape:

```json
{
  "error": {
    "code": "NOT_FOUND",
    "message": "Movie 3fa8...  was not found.",
    "details": { }
  }
}
```

- `code` is a stable, machine-readable, SCREAMING_SNAKE_CASE identifier — clients should branch on
  `code`, never on `message` text or HTTP status alone. Stable across API versions within `/v1`.
- `message` is a human-readable string, safe to log or show in a debug context, not guaranteed
  translated/localized.
- `details` is an optional structured object for machine-actionable extra context (e.g. field-level
  validation errors); omitted when there's nothing to add.
- The HTTP status code still carries the usual semantic weight (`404`, `409`, `422`, `401`, `403`,
  `429`, `5xx`) — `code` refines it, it doesn't replace it.

## OpenAPI docs exposure

The generated OpenAPI 3.x spec is served at `/openapi.json` (or equivalent, per Salvo's OpenAPI
integration), and a Scalar-based interactive documentation UI is mounted at `/openapi` — this is kept
as-is from today, since it already works and there is only one API surface to document now instead of
two.

## Codegen pipeline

The TypeScript client is generated, never hand-written, from the same OpenAPI spec the server
exposes — a pipeline that already exists and is proven for today's REST slice (auth/streaming), and
is simply extended to cover the full API surface once GraphQL is gone:

1. `beam-server` exposes an `export_openapi`-style path (a small binary or a build/test-time routine)
   that constructs the router and serializes its OpenAPI spec to `openapi.json`, without needing a
   running database or any live infrastructure — the spec is derived from route/handler annotations,
   not from runtime introspection.
2. `openapi-typescript` converts `openapi.json` into a generated `.d.ts` file of request/response
   types, one source of truth for the wire shape.
3. `openapi-fetch` provides a thin, fully-typed HTTP client built against those generated types,
   consumed directly by `beam-web`.
4. This pipeline runs in CI (or as a pre-commit/build step) so that a server-side API change that
   isn't reflected in the client types fails the build — the TypeScript compiler is the contract
   check, not a manually maintained changelog.

This is the **only** client contract in the target architecture — there is no separate GraphQL
schema/codegen path to keep in sync with it.

## Server-Sent Events for progress

Real-time progress (library scan progress, per-title enrichment progress) is delivered over
**Server-Sent Events**, replacing what would previously have been a GraphQL subscription over
WebSocket.

- **Endpoint shape:** a single `GET` endpoint the client subscribes to via `EventSource` (or an
  SSE-aware fetch wrapper), e.g. `GET /v1/libraries/{libraryId}/scan-events` or
  `GET /v1/enrichment/events`. Authenticated the same way as any other request — the session cookie
  is attached automatically since `EventSource` is a normal same-origin HTTP request.
- **Why SSE over WebSocket here:** the progress channel is strictly server-to-client (the client
  never needs to push messages back over the same channel — actions like "cancel scan" are ordinary
  REST calls on a different endpoint), so SSE's simpler unidirectional model is a better fit than a
  full-duplex WebSocket, and it composes more simply with the REST/OpenAPI-first design (SSE is just
  an HTTP response with a special content type, not a protocol upgrade requiring separate
  auth/handshake handling).
- **Event framing:** each SSE event carries a small JSON payload identifying what changed (e.g.
  `{"libraryId": "...", "filesScanned": 120, "filesTotal": 480, "phase": "hashing"}` for scan
  progress; `{"movieId": "...", "status": "succeeded", "provider": "tmdb"}` for enrichment). The
  stream closes (or the client stops listening) once the operation reaches a terminal state.
- **Reconnection:** standard `EventSource` reconnection semantics apply; the server does not need to
  replay missed events for this push's use cases, since a reconnecting client can always re-fetch
  current state via the corresponding REST resource (e.g. `GET /v1/libraries/{libraryId}` reflects
  the latest `last_scan_*` columns even if an SSE event was missed).
