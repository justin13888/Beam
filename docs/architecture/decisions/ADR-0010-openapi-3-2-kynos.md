# ADR-0010: REST/OpenAPI 3.2 on Kynos

## Status

Accepted, and implemented. Supersedes ADR-0002 while retaining its REST-only decision. Kynos 0.1.0
replaced Salvo in [#118](https://github.com/justin13888/beam/issues/118) and is the deployed HTTP
runtime. [Kynos migration readiness](../kynos-migration-readiness.md) carries the gate-by-gate
record; "Outcomes" below records what shipped and where it differed from the decision text, which is
left as written.

## Context

ADR-0002 removed Beam's parallel GraphQL API because the server and its controlled clients were
maintaining two schemas, two generated clients, and two authorization paths without gaining a
needed query capability. The remaining REST implementation still has avoidable duplication:
runtime routes, endpoint annotations, response rendering, and OpenAPI metadata can disagree.
Salvo types also reach into `beam-auth`, API DTOs, tests, and server startup.

The current and planned clients have finite, shared read facades:

| Facade | Shared client need |
|---|---|
| Identity | Current user and session management |
| Discovery | Media summaries with typed search, filters, sorting, and pagination |
| Detail | Movie/show metadata, with unbounded children exposed as paginated subresources |
| Playback | Source capabilities, direct play, download, progress, and history |
| Administration | Libraries, users, status, logs, and background-work state |
| Live state | One-way scan and enrichment events |

Android, Apple, TV, mobile, and web clients differ in presentation, codec support, and device
authentication. None needs client-selected traversal of the domain graph. Beam's expected traffic
also does not make GraphQL performance an architectural concern.

OpenAPI 3.2 can describe sequential media, including `text/event-stream`, through `itemSchema` and
can describe JSON encoded in an SSE event's `data` field through `contentMediaType` and
`contentSchema`. Existing client tooling does not yet implement all of that model, but Beam controls
`spargen` and can carry a fork of the TypeScript tooling.

Kynos is the intended replacement for Salvo, but it is still under development. Adopting it before
its contract, streaming, and test facilities are complete would move missing framework behavior
into Beam and defeat the purpose of the migration.

## Decision

Beam will expose one REST API under `/v1`, described by one OpenAPI 3.2 document.

- Collections return purpose-built summaries; resource endpoints return canonical details;
  unbounded relationships use paginated subresources.
- Filtering, sorting, and pagination use finite typed parameters. Beam will not grow a generic
  `fields`, `include`, or `expand` query language.
- GraphQL is not part of the architecture. A future exception requires a new ADR demonstrating a
  runtime-variable, multi-relationship read that otherwise requires client-specific endpoints or
  dependent request chains. Such an exception is read-only; mutations, authentication, files, and
  events remain REST.
- Scan and enrichment events remain SSE because the channel is one-way. The OpenAPI 3.2 response
  describes the SSE envelope and the JSON event payload, and generated clients expose a typed
  asynchronous event sequence.
- Kynos will replace Salvo only after every mandatory item in the readiness specification is
  implemented and verified at a pinned Kynos revision.
- Kynos owns the HTTP runtime and contract derivation, but its types remain in `beam-server`'s HTTP
  adapter. `beam-auth`, services, repositories, and domain crates remain framework-independent.
- The browser toolchain will use a fork of the `openapi-ts/openapi-typescript` repository first.
  Beam will pin and validate that fork before proposing its OpenAPI 3.2/SSE changes upstream.
  Beam returns to upstream releases only after those changes are released.
- `spargen` must independently support the same OpenAPI 3.2 and typed-SSE fixtures before a native
  Rust-core client depends on the new contract.

The Kynos migration preserves `/v1`. Correcting documented status codes, response schemas, security
metadata, and SSE metadata does not justify a second API version.

## Consequences

Beam retains one API style and gains a single source for routing, runtime types, and OpenAPI. SSE
remains code-generated rather than a permanent handwritten exception. Kynos becomes intentional
high coupling at the HTTP edge, but does not enter the domain or service layers.

The intentional cost is a document only 3.2-aware tooling can read. `spargen` was already
3.2-aware; the browser toolchain was not, and still is not. Beam absorbed that by standing
`beam-web` down rather than by blocking the server — see "Outcomes".

## Outcomes

What shipped, and where it differs from the decision above.

- **Kynos arrived as a crates.io release, not a pinned revision.** The decision text required a
  pinned Kynos revision because Kynos was pre-release when this was written. 0.1.0 published, so the
  dependency is a version requirement like any other and the pin is moot. That is now the standing
  policy for both first-party tools: Beam tracks `kynos` and `spargen` as crates.io version
  requirements, not git revisions. A gap still goes upstream first, but Beam consumes the fix from a
  release rather than pinning the revision that carries it -- a git pin takes an unreviewed slice of
  upstream `master` along with the one fix, which is how a dependency stops being a decision.
- **`/metrics` is described, not hidden.** Under Salvo it was a plain handler `merge_router` could
  not see, so it stayed out of the spec by accident of the framework. Kynos routes and describes
  from one declaration, and the only ways to keep it out are the `unchecked` feature — which stamps
  the *whole* document non-authoritative to hide one path — or a second listener on its own port.
  It carries an `internal` tag instead, which satisfies the readiness contract's requirement that
  internal routes be explicitly marked. It also answers `503` when `BEAM_ENABLE_METRICS=false`
  rather than 404ing, because the router's shape — and therefore the exported description — must not
  depend on deployment configuration.
- **Beam kept its own rate-limit algorithm.** Kynos's shipped limiter reads `SystemTime::now()`
  inline with no clock seam, and `beam_domain::services::Clock` is the workspace's canonical time
  seam. Beam implements Kynos's `RateLimitPolicy` — the seam provided for exactly this — over its
  existing token bucket. The framework owns the rendering: a `429` is an RFC 9457 problem document
  with `X-RateLimit-Limit`/`-Remaining`/`-Reset`.
- **One error envelope, not three.** Kynos renders every failure, its own extractor rejections
  included, as an RFC 9457 problem document, and offers no hook for a second shape. The four error
  enums rendering three body shapes recorded in
  [#123](https://github.com/justin13888/beam/issues/123) collapsed into one family, and several
  statuses that existed only at run time — the same-origin `403`, `Set-Cookie` on the auth
  operations, the SSE `200`'s content type — now appear in the document.
- **The browser fork turned out to be a dependency bump.** `openapi-typescript` 7.13.0 cannot read a
  3.2 document at all: it delegates to `@redocly/openapi-core` 1.34.8, whose `detectSpec` knows only
  3.0 and 3.1. Redocly 2.x speaks 3.2; `openapi-typescript` has not bumped. The upstream fix is far
  smaller than the fork this ADR mandated, so the fork is a fallback rather than the plan.
  `beam-web` is not regenerated in the meantime, and its `ts:typecheck`/`ts:test` gates are off — a
  deliberate stand-down for a client being rewritten, not a claim that the contract is satisfied.
- **`/v1` is preserved in path and method; names are not.** Every operation the Salvo server served
  is served, with two `HEAD` operations added for ranged delivery. Operation IDs and schema keys
  were renamed (`getAdminEvents`, `MediaMetadata`), so generated clients are regenerated rather than
  kept. Correcting the description was the point; renaming its keys is the accepted cost.
- **The router/document conformance test is obsolete by construction.** `routes/contract_tests.rs`
  parsed Salvo's router `Debug` output to prove the route table and the merged document agreed,
  because under Salvo they were two passes. Kynos derives both from one walk of `create_router`, and
  `build` refuses a router that cannot describe itself, so the test could no longer fail.
