# Kynos Migration Readiness

This was the acceptance contract for replacing Salvo with Kynos. The migration shipped on Kynos
0.1.0 from crates.io ([#118](https://github.com/justin13888/beam/issues/118)), so this page is now
the record of what was demanded and what was delivered. The requirement text is unchanged --
a contract edited to match its outcome records nothing -- and each section carries the status of the
gate it states.

See [ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md) for the decision, the API-style rationale,
and the outcomes section listing what shipped differently from the plan.

| Gate | Status |
|---|---|
| Contract and OpenAPI 3.2 | Met |
| HTTP runtime | Met |
| Server-Sent Events | Met |
| Ranged media delivery | Met |
| Cookies, rate limiting, and testing | Met, differently: Beam supplies the token bucket through Kynos's policy seam, and the route/document conformance test is obsolete by construction |
| Browser toolchain | **Not met.** `openapi-typescript` cannot read a 3.2 document, and the fix upstream is a dependency bump rather than the fork this page mandated |
| `spargen` | Met at 0.4.0 |
| Coupling boundary | Met |

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

**Status: met.** `routes::create_router` is the single declaration; `Router::build` dispatches from
it and `Router::openapi_as(SpecVersion::V3_2)` describes it, so the two cannot disagree. The
exported document is OpenAPI **3.2.0** — 3.1 has no way to describe a sequential body, and Kynos
gates its whole streaming subtree on the `openapi32` feature rather than describe a stream
inaccurately. `build` refuses a router it cannot describe, including two interceptors contributing
the same status or response header, so `main` fails before it binds a listener rather than serving
an undescribable surface. `examples/export_openapi.rs` builds the router with no database, no
listener and no initialized service. Authentication is declared by taking `SessionAuth` /
`AdminAuth` in a handler signature, so enforcing it and documenting it are one act. The `openapi32`,
`server`, `docs` and `test-util` features are selected individually in `Cargo.toml`.

One deviation, and it is Kynos's: `Router::describe` unions the router's and the group's tag scopes
and never reads an endpoint's own tag, so a route-level `tag = ...` is accepted by the macro and
silently dropped — every tag vanished from the document the first time the port ran. The tags are
declared on group scopes, where Kynos does read them, and the bug is filed upstream (getkono/kynos#94). The route
attributes keep theirs as the statement of intent. Per AGENTS.md this is not a workaround: it is a
different supported API, not a post-processed document.

The one thing that did *not* survive byte-for-byte is naming. Operation IDs are now camelCase
(`getAdminEvents`, not `beam_server.routes.admin.get_admin_events`) and schema keys are bare type
names (`MediaMetadata`, not `beam_server.models.media.MediaMetadata`). Paths, methods and status
semantics are unchanged, which is what ADR-0010 preserves; the generated clients are regenerated
rather than kept.

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

**Status: met.** `AppState` is injected through `Inject<T>` rather than looked up, which deleted an
entire class of handler: every `depot.obtain::<T>()` miss and the 500 it rendered are now compile
errors, and `beam-auth`'s `MissingDependency` marker is gone with them. `Interceptor` covers early
rejection (`EnforceSameOrigin`, `RateLimit`) and `Observer` covers post-response observation
(`HttpMetrics`), the latter outside the interceptor chain so a rate-limited `429` and a same-origin
`403` are counted structurally rather than by installing the metrics hoop outermost. `Route` hands
middleware the matched `paths` key, which retired `classify_route` — a hand-maintained mirror of the
`/v1` route table that Salvo forced on us because it never exposed the matched pattern. `Server`
supplies `graceful_shutdown`, `shutdown_timeout` and `max_connections`, and `prepare` binds before
it serves, so "address already in use" fails at startup rather than after the process has claimed
to be up.

Rejection rendering is not application-supplied in the way this bullet imagined, and the outcome is
better than the requirement. Kynos renders every failure — its own extractor rejections included —
as an RFC 9457 problem document, and offers no hook for a second envelope. That collapsed the four
error enums rendering three body shapes recorded in
[#123](https://github.com/justin13888/beam/issues/123) into one family in `routes/api_error.rs`,
each operation naming the narrowest type covering what it can actually answer with.

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

**Status: met**, and it is the gate that forced the whole document to 3.2. `GET
/v1/admin/events/stream` returns `Sse<S>`, which describes itself with `itemSchema` and its `data`
payload with `contentMediaType`/`contentSchema`; under Salvo that operation's `200` carried no
content type and no schema at all. `AdminAuth` is an extractor, so a 401 or 403 resolves *before*
the stream is committed rather than arriving after a 200 is on the wire. Retention policy stays
Beam's: a `RecvError::Lagged` is logged and skipped, because a dashboard that died because it
blinked is worse than one that missed a row.

One rough edge: Kynos does not re-export the stream trait its `Sse` response is generic over, so
`beam-server` depends on `futures-core` directly. Filed upstream.

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

**Status: met.** `routes/stream.rs` serves both endpoints through a Kynos `ByteSource`, which is
what let roughly a thousand lines of hand-rolled range parsing, conditional-request evaluation and
header assembly go. `HEAD /v1/files/{file_id}/stream` and `HEAD /v1/files/{file_id}/download` are
now declared operations rather than an undocumented side effect of a GET handler. Authorization,
domain lookup and library-root containment stay in Beam, and one status was corrected on the way
through: a file resolving outside its library root is a `403`, not the `401` the Salvo
implementation collapsed it into (#123).

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

**Status: met, differently, on two of these.**

*The limiter.* Kynos ships one, and Beam does not use it: its `Quotas::check` reads
`SystemTime::now()` inline with no clock seam, and `beam_domain::services::Clock` is this
workspace's one canonical time seam. The test that proves a bucket *refills* — rather than merely
that a full one refuses — can only be written by moving time. So Beam implements Kynos's
`RateLimitPolicy` instead, which is the seam provided for exactly this, and keeps its own token
bucket below it unchanged; the clock arrives on `AppState` because `check` receives it. What the
framework does own is the rendering: a `429` is now an RFC 9457 problem document carrying
`X-RateLimit-Limit`, `-Remaining` and `-Reset`, where Salvo's hoop returned
`{"error": "Rate limit exceeded"}`. The requirement wanted the bucket from Kynos and got the policy
seam instead. That is not a gap to file: `RateLimitPolicy` exists precisely so an application can
replace the algorithm, and Kynos's own documentation points at it. The seam is the part that
mattered.

*The conformance test.* It is obsolete by construction, which is the outcome the requirement was
reaching for. `routes/contract_tests.rs` parsed Salvo's `Router` `Debug` output to prove the route
table and the merged document agreed, because under Salvo they were two passes that could diverge.
Kynos derives both from one walk of `create_router`, so there is no second artifact to reconcile
and `build` rejects a router that cannot describe itself. A test asserting the two agree could no
longer fail.

*Everything else.* `kynos::test::TestClient` dispatches in process without binding a port, and the
subcutaneous suite (`*_tests.rs` beside each route) builds the real router over `AppState` wired to
in-memory fakes, exactly as before. `WithHeaders` puts `Set-Cookie` in an operation's declared
response headers, which the Salvo implementation never did.

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

**Status: not met, and superseded in shape.** `openapi-typescript` 7.13.0 cannot read the document
at all — not the SSE model, the whole file. Its own version gate accepts 3.2, but it delegates to
`@redocly/openapi-core` 1.34.8, whose `detectSpec` knows only 3.0 and 3.1 and throws
`Unsupported OpenAPI version: 3.2.0` from both `lint` and `bundle`; no ruleset or severity override
reaches it. Redocly 2.x speaks 3.2 natively and `openapi-typescript` has not bumped to it, so the
work upstream is a dependency bump plus whatever the 3.2 SSE model needs on top — far smaller than
the fork this section mandated. Fork only if that bump is refused; the mandate stands as a fallback,
not as the plan.

`beam-web` is therefore **not regenerated**. `api.gen.ts` is stale — every schema key it names still
carries the old dotted prefix — and `codegen:openapi`'s client step, plus the `ts:typecheck` and
`ts:test` gates, are switched off with a `#118-followup` TODO naming this paragraph. That is a
deliberate stand-down rather than a mechanical rename, because `beam-web` is being rewritten
shortly and renaming ~55 references across 20 files twice buys nothing. `ts:check` stays on:
`biome.json` excludes the generated file and does not type-check.

### `spargen`

Before any Rust-core client consumes the contract, `spargen` MUST:

- Parse OpenAPI 3.2 sequential media and `itemSchema`.
- Decode the typed SSE envelope and JSON `data`.
- Generate a cancellable `Stream<Item = Result<AdminEvent, StreamError>>`.
- Support cookies, reconnect delay, event IDs, and replay headers.
- Pass the same Kynos-generated conformance fixtures as the browser fork.

Client generation remains outside Kynos: Kynos emits the contract; the fork and `spargen` consume
it.

**Status: met at 0.4.0**, the version `beam-client-core` already pins — this half of the gate needed
no work. `codegen:openapi` now copies the export to `beam-client-core/api/openapi.json` and
`codegen:openapi:check` diffs both copies, so the native client's contract cannot silently drift
from the server's. The two files had been byte-identical by habit rather than by construction, and
only the `beam-web` copy was ever checked.

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

**Status:** Kynos supplies more of this list than the section assumed. `Docs::scalar()` serves the
UI at `/openapi` and the document at `/api-doc/openapi.json`, `Cors` is a Kynos interceptor,
`TrustedProxies` is a dispatch-time policy on the built service, and `Shutdown::signals()` registers
the OS signals. Beam keeps the CSRF check (`EnforceSameOrigin`, an interceptor that declares the
headers it reads and the 403 it answers with) and the Prometheus recorder and its exposition route.
Both docs paths — `/openapi` and `/api-doc/openapi.json` — appear in the document as operations
themselves, so the served surface has no unlisted members.

## Coupling boundary

The significant Rust dependencies, excluding Tokio, remain ordered by architectural coupling:

| Rank | Dependency | Use and containment |
|---:|---|---|
| 1 | SeaORM | Entity models, migrations, and production repositories; kept behind domain repository traits. |
| 2 | Kynos | Routing, HTTP adaptation, OpenAPI, streaming, and subcutaneous tests; confined to `beam-server`. |
| 3 | Serde / serde_json | Wire DTOs, configuration, and persisted structured values; retained as the serialization foundation. |
| 4 | `async-trait` | Object-safe asynchronous service and repository traits used through `Arc<dyn Trait>`. |
| 5 | UUID / chrono | Domain identifiers and timestamps spanning persistence and wire contracts. |
| 6 | `ffmpeg-next` | Index-time media probing only, isolated in `beam-index` and feature-gated for local builds. |
| 7 | `openidconnect` / reqwest | Real OIDC discovery and exchange, isolated behind the auth client trait. |
| 8 | `cameo` | Metadata enrichment adapter, isolated behind `EnrichmentProvider`. |
| 9 | tracing / metrics | Cross-cutting observability facades without business-logic ownership. |

The Kynos migration MUST remove framework dependencies from `beam-auth`; HTTP adapters and wire
DTOs belong in `beam-server`. Domain and service layers MUST remain transport-independent.

**Status: met.** Salvo is gone from `Cargo.toml` and `Cargo.lock`. `kynos` is declared in
`[workspace.dependencies]` and depended on by `beam-server` alone. `beam-auth` lost its `server`
feature — its OIDC HTTP adapter now lives in `beam-server/src/routes/auth.rs`, and what remains in
the crate is transport-independent by construction rather than by convention. Its default feature is
`oidc`, because there is no transport layer left to enable.

## Migration entry gate

Implementation may begin only when:

1. Every mandatory Kynos capability has automated conformance coverage at a pinned revision.
2. The browser fork and `spargen` pass the shared OpenAPI 3.2/SSE fixtures.
3. The migration can preserve `/v1` and the current generated-client contract.
4. Kynos can run Beam's complete in-memory vertical-slice suite without external infrastructure.

This gate was overtaken. Kynos reached 0.1.0 on crates.io with the framework capabilities above,
which made a pinned revision unnecessary — the dependency is a released version, not a git pin — and
the migration proceeded on gates 1, 3 and 4. Gate 2 passed for `spargen` and failed for the browser
toolchain, and the migration went ahead anyway with `beam-web` explicitly stood down: holding the
whole server on a TypeScript generator's dependency bump, for a client that is about to be
rewritten, would have kept the document and the router two artifacts for longer than the problem
warranted. `/v1` is preserved in path and method; the generated-client contract is not, because
operation IDs and schema keys were renamed, and regenerating is the accepted cost.

What that trade-off owes: `beam-web` is regenerated, and `ts:typecheck` and `ts:test` come back on,
before the rewrite can claim to consume this contract.
