# Configuration Reference

This is the complete target-state environment variable reference for Beam: what exists today, what
this push adds, and what this push removes. It supersedes any partial list in the README or in
`.env.example` comments — treat this table as the source of truth, and update `.env.example` and
`verify-config.sh` to match it as separate follow-up work (see "Validation" below).

Every variable is read by exactly one of: `beam-server` (the backend binary; today `beam-stream` +
`beam-auth`, merged this push — see `docs/architecture/overview.md`), `beam-web` (the frontend, build-
or runtime-injected via Vite), or `compose` (used only to parameterize `compose*.yaml` — host port
mappings, image tags — and not read by application code directly).

## Database

| Variable | Read by | Required / Default | Purpose |
|---|---|---|---|
| `POSTGRES_USER` | compose | Required; default `beam` | Postgres superuser/app user created in the `postgres` container. |
| `POSTGRES_PASSWORD` | compose | Required; default `password` (change in production) | Password for `POSTGRES_USER`. |
| `POSTGRES_DB` | compose | Required; default `beam` | Database name created in the `postgres` container. |
| `POSTGRES_HOST_PORT` | compose | Optional; default `5432` | Host-side port mapping for Postgres. |
| `DATABASE_URL` | `beam-server` | Required | Full Postgres connection string used by sea-orm; must match the `POSTGRES_*` values above when using the compose Postgres instance. |

## Server binding & URLs

| Variable | Read by | Required / Default | Purpose |
|---|---|---|---|
| `BIND_ADDRESS` | `beam-server` | Required; default `0.0.0.0:8000` | Address/port the server binds its HTTP listener to. |
| `SERVER_URL` | `beam-server` | Required; default `http://localhost:8000` | Public/canonical URL of the server, used for constructing absolute URLs (e.g. OIDC redirect URIs). |
| `BEAM_WEB_URL` | `beam-server` | **New this push.** Required | Canonical URL of the `beam-web` frontend. Used to construct the post-login redirect target and as an implicit trusted `Origin` for CSRF/CORS validation (NFR-104). |
| `BEAM_EXTRA_ALLOWED_ORIGINS` | `beam-server` | **New this push.** Optional; default empty | Comma-separated list of additional `Origin` values the server accepts for CORS/CSRF validation on mutating requests, beyond `BEAM_WEB_URL`. Use for additional trusted frontends (e.g. a staging deployment) — do not use to broadly allowlist arbitrary origins. |
| `STREAM_HOST_PORT` | compose | Optional; default `8000` | Host-side port mapping for the server container. |
| `WEB_HOST_PORT` | compose | Optional; default `8080` | Host-side port mapping for the web container. |
| `C_STREAM_SERVER_URL` | `beam-web` (build + runtime) | Required; default `http://localhost:8000` | URL `beam-web` uses to reach `beam-server`'s API. |
| `C_APP_TITLE` | `beam-web` (build + runtime) | Optional; default `Beam` | Display name shown in the web UI. |

`TRAEFIK_HTTP_PORT`, `TRAEFIK_HTTPS_PORT`, and `TRAEFIK_DASHBOARD_PORT` (compose-only, unchanged this
push) control Traefik's host port mappings — see `docs/operations/deployment.md`.

## Media & data directories

| Variable | Read by | Required / Default | Purpose |
|---|---|---|---|
| `VIDEO_DIR` | `beam-server` | Required; default `/videos` | Read-only media library root mounted into the server container. The server never writes here (NFR-106). |
| `BEAM_DATA_DIR` | `beam-server` | **Renamed this push** from `CACHE_DIR`; required; default `/cache` | The server's own writable state directory — distinct from `VIDEO_DIR`. Holds server-owned state such as `cameo`'s SQLite metadata cache (FR-203). Never used for transcoded media, since none is ever produced (see [ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md)). The rename from `CACHE_DIR` clarifies that this is server state, not a disposable/rebuildable cache in the traditional sense — treat it as data you'd want to back up. |
| `HOST_VIDEO_DIR` | compose | Optional (falls back to a named volume) | Host filesystem path to bind-mount as `VIDEO_DIR` in production, in place of the default named volume. |
| `HOST_CACHE_DIR` | compose | Optional (falls back to a named volume) | Host filesystem path to bind-mount as `BEAM_DATA_DIR` in production, in place of the default named volume. |

## OIDC / authentication

| Variable | Read by | Required / Default | Purpose |
|---|---|---|---|
| `BEAM_OIDC_ISSUER` | `beam-server` | **New this push.** Required | OIDC issuer URL used for discovery (`/.well-known/openid-configuration`). Points at the local Dex instance in development and at any OIDC-compliant IdP in production. |
| `BEAM_OIDC_CLIENT_ID` | `beam-server` | **New this push.** Required | OAuth2/OIDC client ID registered with the identity provider. |
| `BEAM_OIDC_CLIENT_SECRET` | `beam-server` | **New this push.** Required | OAuth2/OIDC client secret. Treat as a secret; never commit a real value. |
| `BEAM_OIDC_SCOPES` | `beam-server` | **New this push.** Optional; sensible default (e.g. `openid profile email`) | Space-delimited OIDC scopes requested during the Authorization Code flow. |

See FR-101–FR-110 in `docs/requirements/functional.md` and
[ADR-0003](../architecture/decisions/ADR-0003-oidc-bff-auth.md) for the backend-for-frontend OIDC
design these variables configure.

## Admin allowlist

| Variable | Read by | Required / Default | Purpose |
|---|---|---|---|
| `BEAM_ADMIN_EMAILS` | `beam-server` | **New this push.** Optional; default empty (no admins) | Comma-separated list of email addresses granted the admin role on login (FR-106). Evaluated on every login, so removing an email here revokes admin status on the user's next login. |

## Session policy

| Variable | Read by | Required / Default | Purpose |
|---|---|---|---|
| `BEAM_COOKIE_SECURE` | `beam-server` | **New this push.** Optional; default `true` | Whether the session cookie is marked `Secure` (HTTPS-only). Set to `false` only for plain-HTTP local development; never in production (NFR-103). |
| `BEAM_SESSION_IDLE_DAYS` | `beam-server` | **New this push.** Optional; sensible default | Number of days of inactivity after which a session is considered expired. |
| `BEAM_SESSION_MAX_DAYS` | `beam-server` | **New this push.** Optional; sensible default | Absolute maximum lifetime of a session regardless of activity. |

Sessions are persisted in Postgres this push, not Redis — see "Removed" below and
[ADR-0005](../architecture/decisions/ADR-0005-sessions-in-postgres.md).

## Metadata enrichment (TMDB / AniList)

| Variable | Read by | Required / Default | Purpose |
|---|---|---|---|
| `TMDB_API_TOKEN` | `beam-server` | Implemented. Optional; no default | TMDB API token used by `cameo` for TMDB-sourced enrichment. If absent, TMDB-eligible titles are left un-enriched rather than failing the scan (FR-307); AniList-sourced titles still enrich without it (FR-306). |
| `ANILIST_ENABLED` | `beam-server` | Implemented. Optional; default `true` | Toggles AniList-sourced enrichment via `cameo`. |
| `ENRICH_INTERVAL_SECS` | `beam-server` | Implemented. Optional; default `300` | Poll interval for the enrichment background worker (new titles are also swept immediately on scan). |
| `ENRICH_BATCH_SIZE` | `beam-server` | Not yet wired to an env var; hardcoded to 25 (`EnrichmentPolicy::default`) | Number of pending titles processed per enrichment worker pass. |
| `ENRICH_MIN_CONFIDENCE` | `beam-server` | Not yet wired to an env var; the matcher's accept thresholds (0.70 total / 0.55 title) are compile-time constants | Minimum match-confidence threshold below which a `cameo` metadata match is discarded rather than applied. |
| `METADATA_LANGUAGE` | `beam-server` | Not yet implemented -- `cameo` requests default-language results | Preferred language for enriched titles/descriptions requested from TMDB/AniList. |

`ENRICH_BATCH_SIZE`/`ENRICH_MIN_CONFIDENCE`/`METADATA_LANGUAGE` are recorded here as the intended
target shape (making these compile-time constants configurable is a small, low-risk follow-up) rather
than removed, so this table doesn't silently lose the design intent -- but they are not live env vars
today; don't set them expecting an effect.

See [ADR-0006](../architecture/decisions/ADR-0006-cameo-enrichment.md) for the `cameo`-based
enrichment design (including the cache-feature conflict discovered while wiring the adapter) and
FR-301–FR-309 for the associated functional requirements.

## Observability / logging

| Variable | Read by | Required / Default | Purpose |
|---|---|---|---|
| `RUST_LOG` | `beam-server` | Optional; default varies by service (e.g. `beam_stream=info,tower_http=debug,axum=debug`) | `tracing` filter directive controlling log verbosity and structured log output (NFR-403). |
| `ENABLE_METRICS` | `beam-server` | Optional; default `false` | Enables metrics collection/exposition. |

## Removed this push

| Variable | Previously read by | Reason removed |
|---|---|---|
| `JWT_SECRET` | `beam-auth`/`beam-stream` | The prior JWT-based stream-token auth design (a 6-hour `?token=` query-string JWT) is removed entirely in favor of OIDC + server-side session cookies (NFR-102). No JWT signing secret is needed. |
| `REDIS_URL` | `beam-auth`/`beam-stream` | Valkey/Redis is dropped as a dependency; sessions move to Postgres (FR-104, [ADR-0005](../architecture/decisions/ADR-0005-sessions-in-postgres.md)). |

## Validation

`verify-config.sh` is the config-validation entry point: run it after `cp .env.example .env` to check
that required variables are set before `compose up` (see `docs/operations/dev-setup.md`). As of this
writing it validates the pre-push variable surface (`POSTGRES_*`, `DATABASE_URL`, `BIND_ADDRESS`,
`SERVER_URL`, `VIDEO_DIR`, `CACHE_DIR`, `C_STREAM_SERVER_URL`) and does not yet know about this push's
new OIDC/session/enrichment/`BEAM_DATA_DIR` surface — notably, it does not currently check
`JWT_SECRET` even though today's compose files hard-require it via `${JWT_SECRET:?...}`, which is a
pre-existing gap. Updating `verify-config.sh` for the new variable surface (including the `CACHE_DIR`
→ `BEAM_DATA_DIR` rename) is tracked as separate follow-up work, not done as part of this
documentation pass.
