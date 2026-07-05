# Deployment

This document describes the current single-host, Podman/Docker Compose-based deployment target for
Beam — both as it exists today and as this push changes it. It does not cover the longer-term
distributed/Kubernetes-native architecture noted in the project README; that remains a legitimate
long-term aspiration but is explicitly out of scope here (see `docs/architecture/overview.md`,
"Deployment scale and non-goals").

## Compose topology

`compose.yaml` is the entry point and simply includes two files:

```yaml
include:
  - compose.dependencies.yaml
  - compose.beam.yaml
```

`compose.dependencies.yaml` defines infrastructure services; `compose.beam.yaml` defines the
application services built from this repository. `docker compose up -d` / `podman compose up -d`
(root `compose.yaml`) brings up both.

### Dependency services (`compose.dependencies.yaml`)

| Service | Today | Target state (this push) |
|---|---|---|
| `postgres` | Postgres 18, healthchecked via `pg_isready`, named volume `postgres:/var/lib/postgresql` | Unchanged. Sole datastore: catalog, sessions (moved here from Redis), enrichment queue, admin logs. |
| `valkey` | Valkey 8 (Redis-compatible), used for JWT/session-adjacent state | **Removed.** Sessions move to Postgres (FR-104, [ADR-0005](../architecture/decisions/ADR-0005-sessions-in-postgres.md)); no Redis-compatible store is needed anywhere in the target state. |
| `dex` | — | **New.** A Dex OIDC identity provider, preloaded with static test users (`admin@beam.localhost`, `user@beam.localhost`, password `password` for both), satisfying FR-110's requirement that local development have a self-contained IdP with no external network dependency. See `docs/operations/e2e-validation.md` for how it's used in the manual login flow. Per NFR-604/the project's workflow rules, introducing this new compose dependency is paired with an in-memory trait implementation (`FakeOidcClient`) for the test suite — see `docs/testing/strategy.md`. |
| `traefik` | Traefik v3, dashboard on loopback only, HTTP→HTTPS redirect, HTTP/3 (QUIC) on the HTTPS entrypoint | Unchanged. Role (TLS termination, routing to app services by `Host()` rule) is not affected by the app-service consolidation below. |

### Application services (`compose.beam.yaml`)

| Service | Today | Target state (this push) |
|---|---|---|
| `auth` | Standalone service, own `Containerfile` (`beam-auth/Containerfile`), own port (`8001`), depends on `postgres` + `valkey`, requires `JWT_SECRET` | **Removed.** `beam-auth` becomes a library-only crate with no standalone binary or Containerfile — its OIDC/session logic is absorbed in-process into the merged server (see `docs/architecture/overview.md`). |
| `stream` | Standalone service (`beam-stream/Containerfile`), owns `VIDEO_DIR`/`CACHE_DIR` volumes, depends on `postgres` + `valkey`, requires `JWT_SECRET` | **Renamed/merged as `server`** (binary `beam-server`, per the workspace rename). Absorbs what `beam-index` does today as a separate gRPC-communicating process — see `docs/architecture/overview.md`'s "Changed from today" note. Depends only on `postgres` (and reaches `dex` over HTTP for OIDC, not a compose `depends_on` health dependency, since Dex readiness isn't required for the server process to start). Volumes: `VIDEO_DIR` (read-only media library) and `BEAM_DATA_DIR` (renamed from `CACHE_DIR` — see `docs/operations/configuration.md`). No longer requires `JWT_SECRET` or `REDIS_URL`; requires the new `BEAM_OIDC_*`, `BEAM_WEB_URL`, `BEAM_ADMIN_EMAILS`, and session-policy variables instead. |
| `web` | `beam-web/Containerfile`, depends on `stream` being healthy, build args `C_APP_TITLE`/`C_STREAM_SERVER_URL` | Unchanged in shape; depends on `server` (the renamed `stream` service) instead of `stream`. |

Traefik's routing labels (`traefik.http.routers.*`) carry over unchanged in intent — each app service
is still reachable at its own `Host()` rule (e.g. `stream.beam.localhost` → `server.beam.localhost` if
the service is renamed, `beam.localhost` for `web`) with TLS termination and, for the streaming
endpoint, disabled response buffering (`flushInterval: "-1"`) for low-latency media delivery. Update
the router names/labels to match the renamed `server` service when the rename lands.

## Production guidance

Carried over from the current README, updated for the new environment-variable surface (see
`docs/operations/configuration.md` for the full reference):

1. **Security**
   - Change `POSTGRES_PASSWORD` from its default (`password`) to a strong, unique value.
   - Set `SERVER_URL` and `BEAM_WEB_URL` (new) to your actual public HTTPS domains — not `localhost`.
     `C_STREAM_SERVER_URL` should match `SERVER_URL` from the browser's perspective.
   - Use HTTPS end-to-end via Traefik (already the default topology) rather than exposing
     `beam-server` directly.
   - Configure `BEAM_OIDC_ISSUER`/`BEAM_OIDC_CLIENT_ID`/`BEAM_OIDC_CLIENT_SECRET` against a real,
     production-grade OIDC provider — Dex's static test users are for local development only and must
     never be used in production.
   - Set `BEAM_ADMIN_EMAILS` deliberately; it is evaluated on every login and directly controls who
     gets admin-role capabilities (FR-106).
   - Leave `BEAM_COOKIE_SECURE` at its default (`true`) in any deployment served over HTTPS.
   - There is no `JWT_SECRET` to manage anymore — it is removed this push along with the stream-token
     JWT design (NFR-102).

2. **Storage**
   - Set `HOST_VIDEO_DIR` to your actual media library location (read-only mount).
   - Size `HOST_CACHE_DIR` (backing `BEAM_DATA_DIR`) for the enrichment metadata cache and other
     server-owned state — this directory is server state you likely want to back up, not a purely
     disposable cache (see `docs/operations/configuration.md`).
   - Consider an external/managed volume for Postgres data in place of the default named volume for
     production durability and backup story.

3. **Metadata enrichment**
   - Set `TMDB_API_TOKEN` if you want TMDB-sourced enrichment; without it, TMDB-eligible titles are
     left un-enriched (surfaced in the admin area) while AniList-sourced titles still enrich (FR-306,
     FR-307).
   - Tune `ENRICH_INTERVAL_SECS`/`ENRICH_BATCH_SIZE`/`ENRICH_MIN_CONFIDENCE` to match your library
     size and desired enrichment throughput.

4. **Performance**
   - Set `ENABLE_METRICS=true` for monitoring.
   - Adjust `RUST_LOG` to `info` or `warn` in production (structured logs back the admin log viewer
     per NFR-403).

## Longer-term direction (out of scope here)

The project README's note about a fully distributed, Kubernetes-native architecture remains a stated
future direction. This document deliberately does not address it: it describes the current
single-host, Compose-based deployment target only. The modular-monolith design
([ADR-0001](../architecture/decisions/ADR-0001-modular-monolith.md)) is chosen specifically so that a
future split back into separate processes/services remains possible without a rewrite, should scale
ever demand it — but no such split is being incrementally built toward in this push.
