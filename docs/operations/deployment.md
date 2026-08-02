# Deployment

Beam's supported deployment is a single host running Podman/Docker Compose. Distributed and
Kubernetes-native topologies are out of scope — tracked in
[#76](https://github.com/justin13888/beam/issues/76); the modular-monolith design
([ADR-0001](../architecture/decisions/ADR-0001-modular-monolith.md)) keeps a future split
possible without a rewrite.

Each release publishes multi-arch (`linux/amd64`, `linux/arm64`) images to
`ghcr.io/justin13888/beam-server` and `ghcr.io/justin13888/beam-web`, tagged `vX.Y.Z`, `X.Y`, and
`latest` — prefer these over building locally. `compose.beam.yaml` still builds both images from the
in-repo Containerfiles, and `beam-server/Containerfile` compiles FFmpeg from source, so a local
`compose build` is slow. See
[ADR-0009](../architecture/decisions/ADR-0009-release-engineering.md) for how a release is cut.

Building the `web` image locally requires `mise run codegen:openapi` first: it takes
`beam-web/openapi.json` from the build context rather than compiling the Rust workspace to generate
it. The build fails with an explicit message if the spec is absent.

## Compose topology

`compose.yaml` is the entry point and includes two files:

```yaml
include:
  - compose.dependencies.yaml
  - compose.beam.yaml
```

`podman compose up -d` (or `docker compose up -d`) brings up everything.

### Dependency services (`compose.dependencies.yaml`)

| Service | Role |
|---|---|
| `postgres` | Postgres 18, the sole datastore: catalog, sessions, enrichment state, admin logs. Healthchecked via `pg_isready`; data in the `postgres` named volume. |
| `dex` | Dev-only OIDC IdP with static test users (`admin@beam.localhost` / `user@beam.localhost`, password `password`; see `dex/config.yaml`). Production deployments point `BEAM_OIDC_ISSUER` at a real IdP (Keycloak, Authentik, Authelia, or a hosted provider) instead. Note: the server *container* cannot currently reach the bundled Dex — exercising the OIDC flow in dev means running `beam-server` on the host; the fully containerized topology is tracked in [#73](https://github.com/justin13888/beam/issues/73). |
| `traefik` | TLS termination and `Host()`-rule routing (`server.beam.localhost` → server, `beam.localhost` → web), HTTP→HTTPS redirect, HTTP/3, dashboard bound to loopback. The server router disables response buffering (`flushInterval: "-1"`) for low-latency media delivery. The bundled setup uses self-signed certs for `*.beam.localhost`; bring your own domain/certificate configuration for production. |

### Application services (`compose.beam.yaml`)

| Service | Role |
|---|---|
| `server` | The `beam-server` binary (built from `beam-server/Containerfile`): HTTP API, OIDC auth, in-process indexing/enrichment, direct-play streaming. Mounts the media library read-only at `BEAM_VIDEO_DIR` and server-writable state at `BEAM_DATA_DIR` (host paths via `HOST_VIDEO_DIR`/`HOST_DATA_DIR`, or the `server_videos`/`server_data` named volumes by default). Healthchecked via `GET /v1/health`. Depends only on `postgres`. |
| `web` | The `beam-web` SPA (built from `beam-web/Containerfile`, which generates the typed client from the `beam-web/openapi.json` supplied in the build context), served as static files by Caddy. Depends on a healthy `server`. |

## Database migrations

`beam-server` applies pending migrations at startup (`BEAM_AUTO_MIGRATE`, default `true`), so a
container-only deployment needs no separate migration step. The supported topology runs exactly
one server process against one Postgres, so there is no concurrent-migrator coordination to worry
about. Set `BEAM_AUTO_MIGRATE=false` to manage schema out-of-band with the `beam-migration` CLI
(`cargo run -p beam-migration -- up|down|status` with `DATABASE_URL` set).

## Deploying on a real server

1. Copy `.env.example` to `.env` and edit it (full variable reference:
   [`configuration.md`](configuration.md)). Run [`verify-config.sh`](../../verify-config.sh) to
   preflight the result before starting anything.
2. **Security**
   - Change `POSTGRES_PASSWORD` from its default and reflect it in `BEAM_DATABASE_URL`.
   - Set `BEAM_SERVER_URL` and `BEAM_WEB_URL` to your public HTTPS domains — not `localhost`.
     `C_STREAM_SERVER_URL` is what the *browser* uses to reach the API and should match
     `BEAM_SERVER_URL` from that perspective.
   - Terminate TLS in Traefik (or your own reverse proxy); never expose `beam-server` directly.
     If TLS terminates in front of a plain-HTTP `BEAM_SERVER_URL`, the server refuses to start
     until you either use the public HTTPS URL or set `BEAM_COOKIE_SECURE` explicitly (see
     [`configuration.md`](configuration.md)).
   - Configure `BEAM_OIDC_ISSUER`/`BEAM_OIDC_CLIENT_ID`/`BEAM_OIDC_CLIENT_SECRET` against a real
     OIDC provider; register `<BEAM_SERVER_URL>/v1/auth/callback` as the redirect URI. Dex's
     static users are for local development only.
   - Set `BEAM_ADMIN_EMAILS` deliberately — it is evaluated on every login and directly controls
     admin capability.
3. **Storage**
   - `HOST_VIDEO_DIR` → your media library (mounted read-only).
   - `HOST_DATA_DIR` (backing `BEAM_DATA_DIR`) → server-owned state worth backing up, not a
     disposable cache.
   - Postgres data lives in the `postgres` named volume; point it at dependable storage and back
     it up (`pg_dump` on a schedule is the minimum viable story).
4. **Metadata enrichment** — set `BEAM_TMDB_API_TOKEN` for TMDB-sourced enrichment; without it,
   TMDB-eligible titles are left un-enriched while AniList titles still enrich.
5. **Operations** — set `BEAM_ENABLE_METRICS=true` for monitoring (Prometheus text exposition at
   `GET /metrics`, top-level and unauthenticated — scrape it over the internal network and do not
   forward it through the reverse proxy) and keep `RUST_LOG` at `info` or `warn`; structured logs
   back the admin log viewer.

Then `podman compose up -d`, and verify: `https://<your-domain>/v1/health` returns OK, the web
app loads, login round-trips through your IdP, and an admin user can create a library pointing at
a path under `BEAM_VIDEO_DIR` and trigger a scan.
