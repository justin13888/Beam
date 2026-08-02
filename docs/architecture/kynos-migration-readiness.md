# Kynos Migration Readiness

This is the acceptance contract for replacing Salvo with Kynos. It describes the ratified target,
not the deployed server. Beam remains on Salvo until **all mandatory gates** are implemented,
covered by automated conformance tests, and available at pinned revisions. Borderline items may be
provided by Kynos, an existing middleware crate, or a small Beam adapter.

See [ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md) for the decision and API-style rationale.

## Mandatory Kynos capabilities

### Contract and OpenAPI 3.2

- One endpoint declaration MUST own the method, path, handler, extractors, request body, responses,
  security, tags, and operation ID. Runtime routing and OpenAPI MUST consume that same declaration.
- Documented handlers MUST return typed variants covering every status, content type, body, and
  response header. Undocumented/internal routes MUST be explicitly marked.
- Duplicate routes or operation IDs, mismatched path parameters, undocumented response variants,
  and unstable schema names MUST fail validation.
- OpenAPI 3.2 export MUST be deterministic and MUST NOT require a listener, database, network, or
  initialized application services.
- Schemas MUST support primitives, nullable values, arrays, maps, enums, tagged unions, `oneOf`,
  UUIDs, date-times, decimals, bounds, defaults, examples, and deprecation.
- Parameters MUST cover path, query, header, and cookie inputs. Responses MUST cover JSON, text,
  empty, redirect, binary, and sequential media bodies.
- Cookie security schemes and operation-level public/authenticated/admin requirements MUST be
  expressible.
- `text/event-stream` MUST emit an OpenAPI 3.2 `itemSchema` for the SSE envelope. JSON carried in
  `data` MUST use `contentMediaType: application/json` and a typed `contentSchema`.
- OpenAPI, SSE, ranged delivery, rate limiting, and test utilities MUST be independently
  feature-gated so Beam compiles only the modules it uses.

### HTTP runtime

- Nested routers, prefixes, static segments, and typed path parameters.
- GET, HEAD, POST, PUT, PATCH, DELETE, and OPTIONS with correct 404 and 405 behavior.
- Typed extraction for state, path, query, headers, cookies, and Serde JSON bodies.
- Configurable header/body limits with application-controlled, typed rejection rendering.
- Access to method, URI, headers, cookies, peer address, and request cancellation.
- Cloneable typed state supporting `Arc<dyn Trait>` without requiring service-location in business
  logic.
- Typed JSON, text, empty, redirect, cookie, arbitrary-header, byte-stream, and SSE responses.
- Global, subtree, route, and endpoint middleware with early rejection and post-response
  observation.
- Matched route template and operation ID available to tracing and metrics middleware.
- HTTP/1.1 keep-alive, backpressured streaming, disconnect cancellation, graceful draining, and a
  bounded shutdown handle.
- An application-supplied error/rejection renderer that uses the same response types recorded in
  OpenAPI.

### Server-Sent Events

- A typed `Sse<T>` response backed by an asynchronous stream.
- `data`, `id`, `event`, `retry`, and keepalive-comment support.
- Automatic JSON serialization and OpenAPI 3.2 envelope/content-schema generation.
- Correct content type, cache-control, and proxy-buffering headers.
- Typed authentication and error responses before committing the stream.
- Backpressure, disconnect detection, cancellation, lag/error termination, and graceful shutdown.
- Access to `Last-Event-ID`; Beam owns event retention, replay, and de-duplication policy.
- Incremental test parsing that can assert a finite number of events without waiting for the stream
  to close.

### Ranged media delivery

- A ranged-response engine over an abstract byte source, not only a filesystem path, so Beam can
  retain fake filesystem implementations.
- Incremental asynchronous reads with backpressure and cancellation; the full file MUST never be
  buffered.
- GET and HEAD plus `Range`, `If-Range`, `If-None-Match`, and `If-Modified-Since` handling.
- Correct 200, 206, 304, 400, and 416 behavior.
- Correct `Content-Length`, `Content-Range`, `Accept-Ranges`, `ETag`, `Last-Modified`,
  `Content-Type`, cache-control, and optional attachment disposition.
- Safe filename encoding. Beam continues to own authorization, domain lookup, library-root
  validation, and byte-source implementations.

### Cookies, rate limiting, and testing

- Cookie creation/removal with Path, HttpOnly, Secure, SameSite, Max-Age, and expiry attributes.
- A feature-gated in-process token bucket with injected clocks, configurable capacity/refill,
  custom client keys, route/subtree policies, exclusions, and `429`/`Retry-After` responses.
- In-process dispatch without binding a port.
- Test requests supporting methods, URIs, headers, cookies, JSON, bytes, and peer addresses.
- Test responses supporting JSON, text, bytes, headers, cookies, redirects, ranged bodies, and
  incremental SSE.
- Injected stateful fakes and deliberately failing trait implementations MUST work without special
  runtime infrastructure.
- Contract tests MUST prove that every registered public endpoint appears exactly once in the
  exported specification and that every documented response is constructible.

## Mandatory client-tool capabilities

### Browser fork first

Beam MUST fork the
[`openapi-ts/openapi-typescript`](https://github.com/openapi-ts/openapi-typescript) repository
before relying on unsupported OpenAPI 3.2 behavior. The fork MUST:

- Parse and preserve OpenAPI 3.2 `itemSchema`, `contentMediaType`, and `contentSchema`.
- Model the SSE envelope while exposing its JSON `data` as the declared payload type.
- Provide a typed subscription API yielding an asynchronous sequence of `AdminEvent`.
- Support cookie credentials, abort/cancellation, reconnection, event IDs, `Last-Event-ID`, and
  typed errors returned before stream commitment.
- Pass fixtures generated by Kynos and the Beam endpoint contract.

The later migration MUST pin an immutable fork revision. Only after Beam validates the fork in
production-shaped tests will the changes be proposed upstream. Beam MUST NOT return to upstream
packages until a release contains the required behavior.

### `spargen`

Before any Rust-core client consumes the contract, `spargen` MUST:

- Parse OpenAPI 3.2 sequential media and `itemSchema`.
- Decode the typed SSE envelope and JSON `data`.
- Generate a cancellable `Stream<Item = Result<AdminEvent, StreamError>>`.
- Support cookies, reconnect delay, event IDs, and replay headers.
- Pass the same Kynos-generated conformance fixtures as the browser fork.

Client generation remains outside Kynos: Kynos emits the contract; the fork and `spargen` consume
it.

## Borderline ownership

These behaviors are required by Beam but do not need to be owned by Kynos when an existing layer or
small adapter supplies them cleanly:

- Scalar/OpenAPI UI and specification serving.
- CORS configuration and preflight handling.
- Origin/Referer CSRF enforcement.
- Trusted-proxy parsing.
- Request IDs, access logs, tracing spans, and Prometheus integration.
- Panic recovery, timeouts, concurrency limits, and load shedding.
- JSON compression, disabled for SSE and ranged media.
- MIME inference and path-based file convenience wrappers.
- OS signal registration around Kynos's graceful-drain handle.
- HTTP/2, TLS, and reverse-proxy conveniences.
- Specification-diff and client-codegen task wrappers.

Kynos does not need GraphQL, WebSockets, an ORM, an application DI container, OIDC logic, a session
store, a metrics registry, background jobs, templates, multipart forms, general static hosting,
transcoding, HLS/DASH, or language-specific client generators.

## Coupling boundary

The significant Rust dependencies, excluding Tokio, remain ordered by architectural coupling:

| Rank | Dependency | Use and containment |
|---:|---|---|
| 1 | SeaORM | Entity models, migrations, and production repositories; kept behind domain repository traits. |
| 2 | Salvo, then Kynos | Routing, HTTP adaptation, OpenAPI, streaming, and subcutaneous tests; Kynos MUST be confined to `beam-server`. |
| 3 | Serde / serde_json | Wire DTOs, configuration, and persisted structured values; retained as the serialization foundation. |
| 4 | `async-trait` | Object-safe asynchronous service and repository traits used through `Arc<dyn Trait>`. |
| 5 | UUID / chrono | Domain identifiers and timestamps spanning persistence and wire contracts. |
| 6 | `ffmpeg-next` | Index-time media probing only, isolated in `beam-index` and feature-gated for local builds. |
| 7 | `openidconnect` / reqwest | Real OIDC discovery and exchange, isolated behind the auth client trait. |
| 8 | `cameo` | Metadata enrichment adapter, isolated behind `EnrichmentProvider`. |
| 9 | tracing / metrics | Cross-cutting observability facades without business-logic ownership. |

The Kynos migration MUST remove framework dependencies from `beam-auth`; HTTP adapters and wire
DTOs belong in `beam-server`. Domain and service layers MUST remain transport-independent.

## Migration entry gate

Implementation may begin only when:

1. Every mandatory Kynos capability has automated conformance coverage at a pinned revision.
2. The browser fork and `spargen` pass the shared OpenAPI 3.2/SSE fixtures.
3. The migration can preserve `/v1` and the current generated-client contract.
4. Kynos can run Beam's complete in-memory vertical-slice suite without external infrastructure.

Until then, no Kynos dependency, compatibility adapter, or second production router belongs in
Beam.
