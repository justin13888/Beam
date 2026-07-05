# Architecture Decision Records

This directory records significant, settled architectural decisions for Beam, in
context/decision/consequences format. Each ADR is numbered permanently at creation time. **ADR
numbers never change or get reused**, even when a later decision supersedes an earlier one — a
superseding decision is recorded as a new ADR that references and supersedes the old one, which stays
in place (marked superseded) for historical record rather than being renumbered or deleted.

| ADR | Decision |
|---|---|
| [ADR-0001](ADR-0001-modular-monolith.md) | Consolidate to a single `beam-server` binary; absorb `beam-index` in-process, delete the gRPC/tonic indexer process; internal crate modularity is preserved, not abandoned. |
| [ADR-0002](ADR-0002-rest-only-api.md) | Delete the GraphQL stack entirely; serve one REST, OpenAPI-first API versioned `/v1`, with progress delivered over SSE instead of GraphQL subscriptions. |
| [ADR-0003](ADR-0003-oidc-bff-auth.md) | Replace username/password auth with OIDC Authorization Code + PKCE via the backend-for-frontend pattern; the browser holds only a session cookie, never IdP tokens. |
| [ADR-0004](ADR-0004-never-transcode.md) | Never transcode or remux media server-side; deliver via full download, direct-play, or selection among pre-existing indexed file versions. |
| [ADR-0005](ADR-0005-sessions-in-postgres.md) | Store sessions in a new Postgres `sessions` table behind a `SessionStore` trait; drop Redis/Valkey from the stack entirely. |
| [ADR-0006](ADR-0006-cameo-enrichment.md) | Enrich indexed titles via the `cameo` crate (TMDB + AniList) behind a provider-agnostic `EnrichmentProvider` trait, run as an async post-scan worker. |
| [ADR-0007](ADR-0007-vendored-ffmpeg-local-dev.md) | Add a `vendored-ffmpeg` Cargo feature (LGPL-only, static) so `cargo test --workspace` is hermetic locally; CI and container images keep dynamically linking system FFmpeg. |
| [ADR-0008](ADR-0008-image-cdn-direct.md) | Serve poster/backdrop images as direct TMDB/AniList CDN links; no server-side image proxy or cache this push. |
