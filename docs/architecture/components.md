# Components

One section per crate/app in the workspace: what it owns, how its modules are laid out, the
boundaries it must respect, and how it is tested. System-level context lives in
[overview.md](overview.md); schema detail in [data-model.md](data-model.md); decision rationale in
the [ADRs](decisions/).

This page describes deployed components. Kynos is a ratified future replacement for Salvo, subject
to the [migration readiness contract](kynos-migration-readiness.md); it is not yet a workspace
dependency.

## Server (`beam-server`)

The single deployable backend binary
([ADR-0001](decisions/ADR-0001-modular-monolith.md)). It owns:

- The HTTP API (Salvo) under `/v1`, REST/OpenAPI-only
  ([ADR-0010](decisions/ADR-0010-openapi-3-2-kynos.md)): media browse/search/detail,
  playback-progress/continue-watching, admin-gated library CRUD, operational logs, and an SSE
  endpoint for scan/enrichment progress.
- Auth wiring: mounts `beam-auth`'s OIDC BFF routes; sessions are Postgres-backed via `beam-auth`
  ([ADR-0005](decisions/ADR-0005-sessions-in-postgres.md)).
- Streaming: direct-play byte-serving over HTTP Range requests with ETag support; no transcoding
  ([ADR-0004](decisions/ADR-0004-never-transcode.md)). `/media/{id}/sources` reports the real
  probed codecs for each file version.
- Process wiring: constructs concrete implementations, spawns `beam-index`'s background indexing
  and enrichment workers in-process via `beam_index::runtime`, and applies `beam-migration`
  migrations at startup (`BEAM_AUTO_MIGRATE`, default true).

Configuration is env-driven via `confique` (`config.rs`), all variables `BEAM_`-prefixed — see
[../operations/configuration.md](../operations/configuration.md). Startup validates configuration
hard (e.g. a cookie-Secure misconfiguration is a startup error) and logs redact secrets.

### Module layout

- `routes/` — the HTTP layer (`health.rs`, `media.rs`, `stream.rs`, `playback.rs`, `admin.rs`,
  `middleware.rs`, `api_error.rs`). Thin: parse the request, call a service trait, render the
  response. Salvo `Request`/`Response`/extractor types never leak past this layer; a handler that
  finds injected state missing returns a 500, never panics.
- `services/` — business logic behind traits (`library.rs`, `metadata.rs`, `playback.rs`,
  `admin_log.rs`, `notification.rs`, `hash.rs`, `media_info.rs`), each with a production
  implementation and an in-memory fake for tests.
- `state.rs` — dependency-injection wiring. `AppServices` holds `Arc<dyn Trait>` for every
  service; this is the only place that constructs concrete (Postgres-backed) implementations.
- `config.rs`, `logging.rs` — configuration and tracing/log setup.
- `models/` — REST-facing DTOs, mapped from `beam-domain` types; API-shape-focused, no FFmpeg
  types.

**Testing:** subcutaneous end-to-end slices — build the real Salvo router with `AppServices` wired
to in-memory fakes and drive it with `salvo::test::TestClient` (`*_tests.rs` beside each route);
error paths (missing files, malformed Range headers, non-admin sessions) are codified by
configuring fakes to fail. Zero external infrastructure.

## Indexer (`beam-index`)

Owns everything that turns files on disk into catalog rows: scanning, change detection, technical
metadata probing, filename parsing, movie/show classification, and metadata enrichment. A
library-only crate — `beam-server` calls it in-process (there is no separate indexer process or
RPC boundary); `runtime.rs` exposes `spawn_background_indexing` and `spawn_enrichment_worker`.

### Module layout

- `probe/` — ffmpeg-next-based technical metadata extraction (resolution, codecs, duration, HDR/
  color info). This is the only place in the workspace that links FFmpeg — probing happens at
  index time, never at stream time ([ADR-0004](decisions/ADR-0004-never-transcode.md)). The
  `vendored-ffmpeg` Cargo feature statically compiles FFmpeg for local dev
  ([ADR-0007](decisions/ADR-0007-vendored-ffmpeg-local-dev.md)).
- `services/` — `index.rs` (scan loop: WalkDir traversal, mtime/size/hash change detection,
  XXH3 content-hash dedup, movie-vs-episode classification, find-or-create; it also emits a
  non-fatal admin warning when two renditions of the same movie/episode have runtimes that diverge
  past `DivergencePolicy`'s relative+absolute thresholds, a likely misnamed/mismatched file);
  `watcher.rs`
  (`FsWatcher` trait, production `NotifyFsWatcher` — inotify on Linux — with debouncing, plus
  `InMemoryFsWatcher` for tests); `enrichment/` (queue-driven async worker with retry/backoff and
  candidate matching/scoring); `media_info.rs`, `hash.rs`, `clock.rs`, `admin_log.rs`,
  `notification.rs` (the latter two back the admin log and SSE progress events).
- `providers/cameo.rs` — the `cameo`-backed `EnrichmentProvider` implementation hitting TMDB and
  AniList ([ADR-0006](decisions/ADR-0006-cameo-enrichment.md)).
- `repositories/` — the SeaORM-backed implementations of every `beam-domain` repository trait
  (library, file, movie, show, stream, genre, enrichment, playback-progress, admin-log).

**Testing:** FFmpeg is confined to `probe/`; every other module is tested with synthetic
metadata/filenames, `InMemory*` repositories, and `InMemoryFsWatcher::emit` — no real video files,
filesystem watches, or database.

## Auth (`beam-auth`)

Library crate owning authentication: the OIDC Authorization Code + PKCE flow (BFF pattern — see
[ADR-0003](decisions/ADR-0003-oidc-bff-auth.md) and [security.md](security.md)), the
Postgres-backed session store, the single-use pending-auth store for in-flight logins, and the
admin allowlist. Users are provisioned JIT from the IdP on first login; there is no self-service
registration.

Feature-gated so consumers pull only what they need: `utils` (stores, models, repository traits),
`oidc` (real discovery/token exchange via `openidconnect` + `reqwest`), `server` (Salvo routes and
session middleware, mounted by `beam-server`), `test-utils` (`FakeOidcClient` and fakes, no HTTP
client). Layout mirrors the features: `src/utils/` (`oidc.rs`, `session_store.rs`,
`pending_auth_store.rs`, `admin_claim.rs`, `models.rs`, `repository.rs`) and `src/server/`
(`oidc_routes.rs`).

**Testing:** route-level tests drive the full login/callback/logout flow against `FakeOidcClient`
and in-memory stores — no real IdP, network, or database.

## Domain (`beam-domain`)

The framework-agnostic core: domain models, repository and provider traits, and pure utility
helpers. It never depends on a web framework, never builds SeaORM queries (trait signatures only —
query-building lives in `beam-index`/`beam-auth`), and never links FFmpeg or leaks FFI types.
This crate is what lets services be tested purely against in-memory fakes.

### Module layout

- `models/` — plain domain structs (`movie.rs`, `show.rs`, `file.rs`, `library.rs`, `stream.rs`,
  `genre.rs`, `enrichment.rs`, `playback_progress.rs`, `search.rs`, `admin_log.rs`) with
  `#[cfg(feature = "entity")] impl From<beam_entity::X::Model>` conversions — the `entity` feature
  is optional so the crate compiles and tests without `sea-orm`.
- `repositories/` — one trait per aggregate (movie, show, file, library, stream, genre,
  enrichment, playback-progress, admin-log), each with an `InMemory*` fake and, behind
  `test-utils`, a `mockall` mock for strict contract tests.
- `providers/` — `EnrichmentProvider`: search/get movie and show metadata by external ID, resolve
  image URLs. Ships `InMemoryEnrichmentProvider` (test-utils) and `NoopEnrichmentProvider` (a
  production-safe "not found" default). Concrete provider SDKs live in `beam-index`, never here.
- `utils/` — pure helpers: `hash.rs` (XXH3), `file.rs` (`FileType`), `filename.rs` (scene-filename
  title/year/episode parsing used by the indexer's scan pipeline).

**Testing:** every trait is usable without a database or network; all fakes are gated
`#[cfg(any(test, feature = "test-utils"))]` so release builds never include test-only code.

## Persistence (`beam-entity` + `beam-migration`)

`beam-migration` is the source of truth for the database schema: every `sea-orm-migration`
migration in chronological order, defining the ENUM types, tables, indexes, and constraints that
exist in Postgres. `beam-entity` is a mirror — one SeaORM `DeriveEntityModel` struct per table,
updated to match the latest migration state. **`beam-entity` never drives schema changes; it
follows them.** Neither crate contains business logic; only repository implementations (in
`beam-index` and `beam-auth`) query through `beam-entity`. Column-level detail lives in
[data-model.md](data-model.md).

Conventions:

- Migrations: one file per change, named `m<YYYYMMDD>_<NNNNNN>_<description>.rs`, registered in
  order in `Migrator::migrations()`. Additive and ordered — never edit a landed migration. Every
  `up()` has a reversing `down()`, including custom Postgres ENUM types. Identifiers are
  `#[derive(DeriveIden)]` enums local to each migration file. `main.rs` is the standard
  `sea-orm-migration` CLI (`migrate up`/`down`/`status`); in normal operation `beam-server`
  auto-applies migrations at startup.
- Entities: one file per table, a `Model` with `DeriveEntityModel`, a `Relation` enum, and an
  `ActiveModelBehavior` impl; `src/lib.rs` re-exports each `Entity` under a short name. UUID
  primary keys are application-generated (`Uuid::new_v4()` in repository `create()`), and Postgres
  ENUM columns map to `DeriveActiveEnum` Rust enums. `beam-domain` converts from `beam-entity`
  models, never the reverse — `beam-entity` has no dependency on `beam-domain`.

**Testing:** these crates are pure data-shape/DDL layers; behavior is verified through the
repository implementations and the in-memory fakes that stand in for them.

## Web client (`beam-web`)

The reference client: a Vite + React 19 + TanStack Router single-page app styled with Tailwind 4
and shadcn/ui, talking exclusively to `beam-server`'s REST API. The typed client is generated from
the server's OpenAPI spec via `openapi-typescript` into `src/api.gen.ts` and wrapped by
`openapi-fetch` in `src/lib/apiClient.ts`; TanStack Query handles caching/loading state. Both CI and
the Containerfile regenerate the typed client from `beam-web/openapi.json`, which `mise run
codegen:openapi` exports from the Rust types.

- **Routes** (`src/routes/`, file-based via `createFileRoute`): `index.tsx` (home /
  continue-watching), `libraries.tsx` / `libraries.$id.tsx`, `media.$id.tsx` (detail + player),
  `explore.tsx` (search), `admin.tsx` (library CRUD, logs, SSE scan/enrichment progress via
  `useAdminEventStream`), `profile.tsx`, `login.tsx` (a sign-in button that redirects to the
  server's OIDC start endpoint).
- **Auth:** `hooks/auth.tsx` is a thin context that calls `GET /v1/auth/me` on mount and relies
  entirely on the httpOnly session cookie — client-side JS never reads, stores, or sends any
  token, and nothing auth-related touches `localStorage`.
- **Player:** built on Vidstack (`components/VideoPlayer.tsx`) with a source-quality picker across
  a title's file versions, resume-from-position, and periodic progress beacons
  (`usePlaybackBeacon`). Transport is plain HTTP Range requests against the streaming route — no
  manifests, since the server never transcodes
  ([ADR-0004](decisions/ADR-0004-never-transcode.md)); artwork loads directly from the provider
  CDN ([ADR-0008](decisions/ADR-0008-image-cdn-direct.md)).
- **Tooling:** Biome (repo-root `biome.json`) is the sole lint/format tool.

**Testing:** vitest + MSW (`src/test/`) + Testing Library in jsdom — the REST boundary is mocked,
so the suite runs with zero backend, mirroring the Rust side's zero-infrastructure rule.

## Client core (`beam-client-core`)

The logic every native client would otherwise reimplement, owned once in Rust and exposed to
Kotlin (and later Swift) over UniFFI 0.32 —
[ADR-0012](decisions/ADR-0012-native-client-rust-core.md). It deliberately depends on none of
`beam-domain`, `beam-index`, or `beam-server`: `beam-domain` takes a non-optional `sea-orm`
dependency and would drag a Postgres driver into an Android `.so`, and `beam-index` links FFmpeg,
which does not cross-compile to Android.

- **Generated REST client** (`api/openapi.json` → `$OUT_DIR` via spargen in `build.rs`): all 31
  operations, never hand-written. The spec is exported from `beam-server`'s own handler
  annotations by `mise run codegen:openapi:core`.
- **`capability.rs`** — the reason a native client exists. Matches each source against a
  `DeviceProfile` built from `MediaCodecList`, and returns a per-source verdict (hardware,
  software, or unplayable with a reason) plus a ranking. Unplayable sources are returned with
  their reason rather than hidden, because under direct play
  ([ADR-0004](decisions/ADR-0004-never-transcode.md)) that is a permanent property the viewer may
  need to act on.
- **`tls.rs`** — installs the rustls `ring` provider and implements trust-on-first-use. The public
  trust store is consulted first and its acceptance is final; only on failure is a user-accepted
  certificate considered, and only when the whole-certificate SHA-256 matches, the SANs cover the
  host, and it has not expired.
- **`transport.rs`** — attaches the `beam_session` cookie below the generated code, as a spargen
  middleware, and notices a mid-session 401 in one place rather than at every call site.
- **`upnext.rs`, `progress.rs`, `paging.rs`, `session.rs`, `servers.rs`, `catalog.rs`** — next
  playable episode across season boundaries, the 15-second progress throttle with a durable retry
  queue, Relay cursor state, the auth state machine, the multi-server registry, and the mapping
  from generated wire types to the vocabulary a UI renders.

**Testing:** 152 tests, all pure or against an `InMemoryKeyValueStore`. Certificates in the TLS
tests are generated with `rcgen` rather than hand-built, so they cannot disagree with reality.

## Android client (`beam-android`)

A Compose app for phone and tablet, on top of `beam-client-core`. Multi-module with convention
plugins in `build-logic/`: `core/{model,ffi,designsystem,ui,media,testing}` and
`feature/{auth,home,libraries,explore,detail,player,downloads,history,settings,admin}`.

- **`core:ffi`** is the only module that sees the generated UniFFI Kotlin or loads the `.so`.
  Everything above it depends on a Kotlin interface, which is what lets every screen be tested on
  the JVM with no native library and no JNA on the classpath.
- **Navigation** is Navigation 3: the back stack is ordinary observable state, so the tabs, the
  detail pages and the player share one history. Destinations are typed keys carrying their own
  arguments, so a navigation call cannot lose or misspell one.
- **`core:media`** owns Media3. Playback and the API client are handed the same credential and
  trust decision by the core, because a mismatch surfaces as apparently corrupt media rather than
  as an auth error. Downloads go through Media3's `DownloadManager` so their bytes land in a cache
  ExoPlayer can read directly.
- **Auth** lifts the `beam_session` cookie from an in-app WebView, which is the only flow the
  server currently supports — see NFR-605 and
  [ADR-0012](decisions/ADR-0012-native-client-rust-core.md).

**Testing:** 129 JVM tests plus Roborazzi screenshot references, run under Robolectric. No emulator
runs in CI.

## Docs site (`beam-docs`)

The user- and operator-facing documentation **site**: a marketing landing page plus end-user and
operator guides, built with Astro + Starlight and deployed to Cloudflare Pages via `wrangler` at
<https://beam.justinchung.net>. It is deliberately separate from the root `docs/` tree, which is
the engineering source of truth for contributors and agents; `beam-docs` is polished public-facing
content and must not substitute for keeping root `docs/` accurate.

The division is by *kind of fact*, not by topic. **Task knowledge** — how a person installs Beam,
adds a library, signs in, plays or downloads something, or interprets an error — is written out in
full on the site and has no counterpart in this tree. **Contract facts** — environment variable
names, defaults and semantics, the data model, the API surface, requirement numbers, ADR rationale
— have their single source of truth here, and the site links to them rather than restating them.
The one deliberate exception is the short list of variables an operator cannot boot without, which
the install guide names inline (names and purpose only, never defaults) beneath a link to
[`operations/configuration.md`](../operations/configuration.md).

The site serves a custom Astro landing page at `/` (`src/pages/index.astro`, outside the Starlight
collection) and Starlight docs for every other route, grouped as Start here / Using Beam / Running
Beam / Reference / Help — the middle two map to the two personas in
[`../requirements/product.md`](../requirements/product.md). The error reference
(`reference/errors`) is the only page coupled to server behavior: it catalogues the statuses and
response bodies produced by `ApiError`, `FileDeliveryError`, `OidcCallbackError`, `OidcAuthError`,
the rate limiter, the same-origin hoop, and the health endpoint. **A change to any of those is not
complete until both [`api.md`](api.md)'s error bullet and that page are updated.**

Content lives under `src/content/docs/` (Starlight collection schema in `src/content.config.ts`,
stock `docsSchema()` with no extension); the sidebar is declared explicitly in `astro.config.mjs`
rather than autogenerated, so the whole information architecture is reviewable in one place.
Tailwind styles only the pages under `src/pages/`, reaching them through
`src/layouts/LandingLayout.astro` — the single module that imports `src/styles/landing.css`. That
import site *is* the isolation mechanism: Astro does not link a stylesheet into pages that never
import it, so Tailwind's Preflight cannot reach Starlight's theme and no
`@astrojs/starlight-tailwind` shim is needed.

Biome lints and formats the site's `.ts` files, but deliberately **not** its `.astro` files: Biome
2.2.4 parses only an `.astro` file's frontmatter script, so it reports every component import and
prop as unused and offers to remove the imports — which the `--write` pre-commit hook would act on.
Revisit at Biome 2.4, which reworked `.astro` handling. `astro check` (`bun run typecheck`)
validates content; there is no separate test suite, and `mise run docs:build` is the only gate that
meaningfully exercises the site. Deployment is `astro build && wrangler pages deploy`, run from
`release.yml` on a release tag; both that job and CI's check out full git history because
Starlight's `lastUpdated` reads it and renders wrong dates rather than failing on a shallow clone.
