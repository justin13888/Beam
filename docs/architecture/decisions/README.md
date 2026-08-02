# Architecture Decision Records

This directory records significant, settled architectural decisions for Beam, in
context/decision/consequences format. Each ADR is numbered permanently at creation time. **ADR
numbers never change or get reused**, even when a later decision supersedes an earlier one — a
superseding decision is recorded as a new ADR that references and supersedes the old one, which stays
in place (marked superseded) for historical record rather than being renumbered or deleted.

| ADR | Status | Decision |
|---|---|---|
| [ADR-0001](ADR-0001-modular-monolith.md) | Accepted | A single `beam-server` binary absorbs `beam-index` and `beam-auth` in-process (the gRPC/tonic indexer process was deleted); internal crate modularity is preserved, not abandoned. |
| [ADR-0002](ADR-0002-rest-only-api.md) | Superseded by ADR-0010 | One REST, OpenAPI-first API versioned `/v1`, with progress delivered over SSE; the GraphQL stack was deleted entirely. |
| [ADR-0003](ADR-0003-oidc-bff-auth.md) | Accepted | OIDC Authorization Code + PKCE via the backend-for-frontend pattern replaced username/password auth; the browser holds only a session cookie, never IdP tokens. |
| [ADR-0004](ADR-0004-never-transcode.md) | Accepted | Never transcode or remux media server-side; deliver via full download, direct-play, or selection among pre-existing indexed file versions. |
| [ADR-0005](ADR-0005-sessions-in-postgres.md) | Accepted | Sessions live in a Postgres `sessions` table behind a `SessionStore` trait; Redis/Valkey was dropped from the stack entirely. |
| [ADR-0006](ADR-0006-cameo-enrichment.md) | Accepted | Indexed titles are enriched via the `cameo` crate (TMDB + AniList) behind a provider-agnostic `EnrichmentProvider` trait, run as an async post-scan worker. |
| [ADR-0007](ADR-0007-vendored-ffmpeg-local-dev.md) | Accepted | A `vendored-ffmpeg` Cargo feature (LGPL-only, static) makes `cargo test --workspace` hermetic locally; CI and container images keep dynamically linking system FFmpeg. |
| [ADR-0008](ADR-0008-image-cdn-direct.md) | Accepted | Poster/backdrop images are served as direct TMDB/AniList CDN links; no server-side image proxy or cache. |
| [ADR-0009](ADR-0009-release-engineering.md) | Accepted | `mise.toml` is the single source of truth for tools and commands, invoked by both CI and the `hk` git hooks; Conventional Commits drive one product version, and release-please cuts `chore: release` pull requests that publish multi-arch GHCR images. |
| [ADR-0010](ADR-0010-openapi-3-2-kynos.md) | Accepted | Retain REST, adopt OpenAPI 3.2 and typed SSE, and replace Salvo with Kynos only after its documented readiness gates pass. |
