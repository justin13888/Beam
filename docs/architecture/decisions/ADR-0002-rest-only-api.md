# ADR-0002: REST-only, OpenAPI-first API

## Status

Accepted.

## Context

Beam previously ran two parallel API stacks: async-graphql served browse/media/admin operations
(including real-time updates via GraphQL subscriptions over WebSocket), while a separate Salvo
REST/OpenAPI surface served auth and streaming. Each stack was separately codegen'd to TypeScript,
so `beam-web` consumed two differently-shaped, independently-generated clients for what is
conceptually one backend. Maintaining two API paradigms meant double the schema/resolver/guard
code, double the codegen tooling, and two different mental models (GraphQL's graph-shaped queries
vs. REST's resource-shaped endpoints) for anyone working across the boundary. The REST/OpenAPI side
already had a working, proven codegen pipeline (`openapi-typescript` + `openapi-fetch`); the
GraphQL side did not offer a capability the product actually needs that REST can't cover — Beam's
domain is a fairly
conventional resource graph (libraries, movies, shows, files), not a case with deeply nested,
client-driven query shapes that would specifically benefit from GraphQL's field-selection model.

## Decision

We deleted the entire GraphQL stack: async-graphql schema/resolvers/guards, the WS subscription
handler, and the `export_schema` binary. Beam serves one domain-specific REST API, versioned `/v1`,
documented via Salvo's OpenAPI integration, exposed through the Scalar docs UI, and codegen'd to
TypeScript via the existing `openapi-typescript` + `openapi-fetch` pipeline (extended to cover the
full surface, not reinvented). Real-time progress (scan, enrichment) is delivered over Server-Sent
Events on dedicated `GET` endpoints, replacing GraphQL subscriptions.

## Consequences

**Positive:**
- One API paradigm, one codegen pipeline, one mental model — less code overall, and one less
  category of cross-stack inconsistency (e.g. the admin-gating gap that existed on the GraphQL side)
  to keep aligned by hand.
- REST + OpenAPI's tooling (Scalar docs, generated TS types, standard HTTP semantics for caching/
  status codes) is mature and already proven in this codebase for the auth/streaming slice.
- SSE is a simpler fit than GraphQL subscriptions for what is, in practice, strictly server-to-client
  progress reporting — no bidirectional protocol or connection-upgrade handshake needed.

**Negative / accepted cost:**
- Clients that want to fetch a deeply nested object graph in one round trip (e.g. "a show, its
  seasons, their episodes, and each episode's files" in a single request) must either issue several
  REST calls or the API must grow purpose-built nested-response endpoints — REST doesn't offer
  GraphQL's client-driven field selection for free. We accept hand-designed nested endpoints
  where the client genuinely needs them, rather than a generic query layer.
- Any future consumer that specifically wants GraphQL's query flexibility (e.g. a third-party
  integration) would need a new API surface built from scratch — this decision is a bet that no such
  consumer exists today, not a claim that none ever will.
