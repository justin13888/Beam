# Configuration

All Beam configuration is environment variables. `beam-server/src/config.rs` is the single
authority for server variable names, defaults, and semantics — this document mirrors it; if they
ever disagree, `config.rs` wins and this file has a bug.

`beam-server` loads a `.env` file if present (via `dotenvy`) and then the process environment.
In compose deployments, variables flow from the repo-root `.env` through the list-form
`environment:` block in `compose.beam.yaml`. That list form is deliberate: an *unset* variable
must arrive inside the container as genuinely absent, not as an empty string — for optional
settings like `BEAM_OIDC_ISSUER`, an explicitly empty value would parse as "set to empty" and,
for example, make the server believe OIDC is configured against an empty issuer URL. Leave
optional variables unset/commented rather than blank.

## beam-server

| Variable | Default | Meaning |
|---|---|---|
| `BEAM_BIND_ADDRESS` | `0.0.0.0:8000` | Address the HTTP server binds. |
| `BEAM_SERVER_URL` | `http://localhost:8000` | Externally-visible base URL of the server. Drives the OIDC redirect URL (`<BEAM_SERVER_URL>/v1/auth/callback`) and the cookie-`Secure` heuristic. Use the public HTTPS URL in production. |
| `BEAM_DATABASE_URL` | `postgres://beam:password@localhost:5432/beam` | Postgres connection string. |
| `BEAM_AUTO_MIGRATE` | `true` | Apply pending schema migrations at startup. Set `false` for operator-managed migrations via the `beam-migration` CLI. |
| `BEAM_VIDEO_DIR` | `./videos` | Read-only root of the media library. Must exist at startup; libraries are created at paths under it. |
| `BEAM_DATA_DIR` | `./data` | Server-writable state directory (created if missing). Treat as data worth backing up, not a disposable cache. |
| `BEAM_ENABLE_METRICS` | `false` | Expose Prometheus metrics. |
| `BEAM_HASH_UNKNOWN_FILES` | `true` | Hash files with unknown extensions during indexing so duplicate detection covers every file; disable to save scan IO. |
| `BEAM_SCAN_INTERVAL_SECS` | `3600` | Interval between periodic full library rescans (backstop for anything the watcher missed). |
| `BEAM_WATCH_ENABLED` | `true` | Run the inotify filesystem watcher for near-real-time index updates. |
| `BEAM_WATCH_DEBOUNCE_MS` | `2000` | Debounce window for watcher events on the same path. |
| `BEAM_ENRICH_INTERVAL_SECS` | `300` | Interval between metadata-enrichment sweeps (new titles are also swept immediately when queued). |
| `BEAM_TMDB_API_TOKEN` | unset | TMDB read-access token for `cameo` enrichment. Absent → TMDB-eligible titles are left un-enriched (never fails a scan). |
| `BEAM_ANILIST_ENABLED` | `true` | Toggle AniList-sourced enrichment (needs no token). |
| `BEAM_OIDC_ISSUER` | unset | OIDC issuer URL. All three `BEAM_OIDC_*` values are required together; until then login is disabled with a clear error (not a crash). |
| `BEAM_OIDC_CLIENT_ID` | unset | OIDC client id registered with the IdP. |
| `BEAM_OIDC_CLIENT_SECRET` | unset | OIDC client secret. Secret — never logged (startup config logging redacts it). |
| `BEAM_OIDC_SCOPES` | `openid profile email` | Space-separated scopes requested at login. |
| `BEAM_WEB_URL` | `http://localhost:5173` | Web client origin: OIDC success redirect target and an implicitly allowed CSRF Origin. |
| `BEAM_EXTRA_ALLOWED_ORIGINS` | unset | Comma-separated extra Origins accepted on state-changing requests. |
| `BEAM_ADMIN_EMAILS` | unset | Comma-separated, case-insensitive emails granted admin on OIDC login. An unverified email is never granted admin. |
| `BEAM_COOKIE_SECURE` | unset (derived) | Whether auth cookies are marked `Secure`. Unset → derived from `BEAM_SERVER_URL`'s scheme. If other configured origins imply HTTPS while cookies would resolve insecure and this is unset, **the server refuses to start**; set it explicitly (`true` for TLS-terminating proxies in front of a plain-HTTP origin, `false` only if you genuinely want insecure cookies — loudly warned). |
| `BEAM_SESSION_IDLE_DAYS` | `14` | Session idle timeout (slides forward on activity, capped by the absolute lifetime). |
| `BEAM_SESSION_MAX_DAYS` | `60` | Absolute session lifetime. |
| `RUST_LOG` | (tracing default) | Standard `tracing` filter, e.g. `beam_server=info`. |

Enrichment batch size and match-confidence thresholds are compile-time constants today; exposing
them (plus a metadata-language knob) is tracked in
[#71](https://github.com/justin13888/beam/issues/71).

## beam-web (build-time)

Vite inlines these into the static bundle at build time (compose passes them as build args):

| Variable | Default | Meaning |
|---|---|---|
| `C_APP_TITLE` | `Beam` | Application title shown in the UI. |
| `C_STREAM_SERVER_URL` | `http://localhost:8000` | URL the **browser** uses to reach the API server; match `BEAM_SERVER_URL` as seen from outside. |

The `C_` prefix is deliberate and must stay distinct from `BEAM_`: Vite inlines **every**
environment variable matching its configured client prefix into the public JavaScript bundle
(`beam-web/vite.config.ts`, `beam-web/src/env.ts`). If the client shared the `BEAM_` prefix, a
build machine with `BEAM_OIDC_CLIENT_SECRET` in its environment would ship that secret to every
browser.

## Compose-only variables

Read by the compose files, not by application code:

| Variable | Default | Meaning |
|---|---|---|
| `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB` | `beam` / `password` / `beam` | Postgres bootstrap credentials; keep in sync with `BEAM_DATABASE_URL`. Change the password in production. |
| `POSTGRES_HOST_PORT` | `5432` | Host port for Postgres. |
| `BEAM_SERVER_HOST_PORT` | `8000` | Host port for the API server. |
| `WEB_HOST_PORT` | `8080` | Host port for the web app. |
| `DEX_HOST_PORT` | `5556` | Host port for the dev-only Dex IdP. |
| `TRAEFIK_HTTP_PORT` / `TRAEFIK_HTTPS_PORT` / `TRAEFIK_DASHBOARD_PORT` | `80` / `443` / `8888` | Traefik entrypoints (dashboard is loopback-only). |
| `HOST_VIDEO_DIR` | `server_videos` named volume | Host path mounted read-only at `BEAM_VIDEO_DIR`. |
| `HOST_DATA_DIR` | `server_data` named volume | Host path mounted at `BEAM_DATA_DIR`. |

## Validation

Run [`verify-config.sh`](../../verify-config.sh) from the repo root after editing `.env`: it
checks required variables, the all-or-none `BEAM_OIDC_*` rule, and mirrors the server's
cookie-Secure startup refusal, without echoing secret values.
