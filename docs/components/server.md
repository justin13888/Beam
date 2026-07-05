# Component: `beam-server`

Status: describes the target module layout for this crate, which is renamed from `beam-stream` as
part of this push. Where current `beam-stream` code differs, that is called out explicitly as
"today." See [docs/architecture/overview.md](../architecture/overview.md) for how this crate fits
into the rest of the system and [ADR-0001](../architecture/decisions/ADR-0001-modular-monolith.md)
for why it is a single binary rather than several services.

## Role

`beam-server` is the one deployable backend binary. It owns:

- The HTTP API (Salvo), REST/OpenAPI-only.
- Auth wiring: OIDC BFF (browser-facing routes/middleware), delegating the actual OIDC client and
  session logic to the `beam-auth` library crate.
- Sessions: Postgres-backed (via `beam-auth`), not Redis/Valkey — see
  [ADR-0005](../architecture/decisions/ADR-0005-sessions-in-postgres.md).
- The media API: browse/search/detail, admin-gated library management, playback-progress /
  continue-watching.
- Streaming: direct-play byte-serving over HTTP Range requests with ETag support. No transcoding.
- Admin: library CRUD, operational logs, an SSE endpoint for scan/enrichment progress.

It depends on `beam-domain`, `beam-entity`, `beam-auth` (library), and `beam-index` (library) as
in-process Rust dependencies — there is no separate `beam-index` process or RPC boundary in the
target state.

## What is being deleted

Today's `beam-stream` carries a GraphQL API alongside REST, and a transcoding subsystem. Both are
removed entirely in this push, not deprecated in place:

- **GraphQL, in full.** The `async-graphql` dependency, `src/graphql/` (`schema/admin`,
  `schema/library`, `schema/media`, `guard.rs`, `resolver_tests.rs`, `auth_tests.rs`), the
  `routes/graphql.rs` and `routes/graphql_ws.rs` handlers, and the `src/bin/export_schema.rs` binary
  are all deleted. REST/OpenAPI is the only API surface going forward.
- **Transcoding, in full.** `services/transcode.rs` (`LocalTranscodeService`, `LocalMp4Generator`),
  `utils/stream/hls.rs`, `utils/stream/mp4.rs`, and the `ffmpeg` CLI shell-out are deleted, along with
  the `stream_cache` table (see [data-model.md](../architecture/data-model.md)). `beam-server` drops
  its `ffmpeg-next` dependency entirely — see
  [ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md). FFmpeg only lives in `beam-index`
  now (technical metadata probing at index time, never at stream time).
- **The gRPC client to `beam-index`.** `services/grpc_index.rs` (`GrpcIndexService`, a `tonic`
  client) is deleted. `beam-index`'s `IndexService` trait is called directly, in-process, as an
  `Arc<dyn IndexService>` constructed from a concrete `beam-index` implementation.
- **Redis-backed sessions.** `RedisSessionStore` is no longer wired into `AppServices`; sessions come
  from `beam-auth`'s Postgres-backed `SessionStore` implementation instead. `compose.dependencies.yaml`
  drops the Redis/Valkey service accordingly (see CLAUDE.md's workflow rule about not adding external
  service dependencies without an in-memory fake — the removal direction doesn't need one, but any
  future dependency added back would).

## Module layout

Even though this is a single crate/binary, module boundaries are treated as strict ownership
boundaries — a new contributor should be able to reason about each directory in isolation:

- **`routes/`** — the HTTP layer. Thin: parses the request, calls into a service trait, renders the
  response. Per CLAUDE.md's domain-isolation rule, Salvo's `Request`/`Response`/extractor types must
  never leak past this layer — services take and return plain domain/DTO types. Expected contents:
  `health.rs` (unchanged), `stream.rs` (Range/ETag byte-serving — retained, this is the core
  streaming path), an `auth.rs` mounting `beam_auth::server::auth_routes()`, a `media.rs` (browse/
  search/detail), an `admin.rs` (library CRUD, logs, SSE progress), and a `playback.rs`
  (progress/continue-watching). `graphql.rs` and `graphql_ws.rs` are removed.
- **`services/`** — business logic, trait-based per CLAUDE.md's trait-based-abstraction rule. Each
  service is defined as a trait with a concrete production implementation and (per the project's
  zero-infrastructure testing rule) an `InMemory*`/fake counterpart for tests. Expected owners:
  `admin_log.rs` (`AdminLogService`), `hash.rs` (`HashService`), `library.rs` (`LibraryService`,
  `PathValidator`), `metadata.rs` (renamed/refocused to the media browse/search/detail API,
  `MetadataService` → media API service, now backed by `beam-index`'s enriched catalog rather than
  the currently-dead `MetadataProvider`), `notification.rs` (`NotificationService` / `AdminEvent`,
  reused from `beam-index` today — retained, since it already backs the target SSE progress
  endpoint), and a new `playback_progress.rs` (resume/continue-watching against the new
  `playback_progress` table). `grpc_index.rs` and `transcode.rs` are removed.
- **`state.rs`** — dependency-injection wiring. `AppServices` holds `Arc<dyn Trait>` for every
  service; `AppState`/`AppContext` carry per-request context (e.g. the authenticated user). This is
  the one place allowed to construct concrete (Postgres-backed) implementations and hand them out as
  trait objects — see CLAUDE.md's dependency-injection rule.
- **`config.rs`** — `ServerConfig` (env-driven via `confique`). Loses `redis_url` and
  `beam_index_url`/`GRPC_PORT`-equivalent fields (no more separate `beam-index` process to point at);
  gains OIDC issuer/client configuration for `beam-auth`.
- **`models/`** — REST-facing DTOs and view models distinct from `beam-domain`'s core types (today's
  `models/media/{codec,format,movie,show,stream}.rs`, `models/library/`, `models/file.rs`). These stay
  API-shape-focused; they should map from `beam-domain` types, not duplicate FFmpeg-derived types —
  `beam-domain` drops its `ffmpeg-next` dependency in this push (see
  [domain.md](domain.md)), so these models should not reintroduce it either.

## Testing expectations

Per CLAUDE.md, `beam-server`'s test suite must exercise complete request/response slices without any
of the services in `compose.dependencies.yaml` running: build the Salvo router with `AppServices`
wired to `InMemory*`/fake implementations of every trait (auth, library, metadata/media, notification,
admin log, playback progress), and drive it with Axum/Salvo test helpers. Edge cases that would
otherwise require manual verification — a missing file on disk, a malformed Range header, an admin
action attempted by a non-admin session — should be codified by configuring an injected fake to return
the relevant error, not by standing up Postgres.
