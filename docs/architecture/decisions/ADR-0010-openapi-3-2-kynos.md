# ADR-0010: REST/OpenAPI 3.2 on Kynos

## Status

Accepted. Supersedes ADR-0002 while retaining its REST-only decision. The deployed implementation
remains Salvo until every prerequisite in
[Kynos migration readiness](../kynos-migration-readiness.md) is satisfied.

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

The migration is deliberately blocked on work in three repositories: Kynos, the TypeScript client
fork, and `spargen`. Until those gates pass, Salvo remains the deployed framework and the existing
generated clients remain authoritative. This ADR and the readiness specification are architectural
documentation only; accepting them does not claim that any prerequisite is already implemented.
