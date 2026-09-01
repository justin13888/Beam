# Deployment

Beam's supported deployment is a single host running Podman/Docker Compose. Distributed and
Kubernetes-native topologies are out of scope — tracked in
[#76](https://github.com/justin13888/beam/issues/76); the modular-monolith design
([ADR-0001](../architecture/decisions/ADR-0001-modular-monolith.md)) keeps a future split
possible without a rewrite.

**No release has been cut yet** — the repository has no tags and no published releases, so no
images exist to pull today and `compose.beam.yaml` builds both from the in-repo Containerfiles.
`beam-server/Containerfile` compiles FFmpeg from source, so expect the first `compose build` to be
slow. Once a release is published, it publishes multi-arch (`linux/amd64`, `linux/arm64`) images to
`ghcr.io/justin13888/beam-server` and `ghcr.io/justin13888/beam-web`, tagged `vX.Y.Z`, `X.Y`, and
`latest`, which should be preferred over building locally. See
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

`podman compose up -d` (or `docker compose up -d`) brings up Postgres, Traefik, the server, and
the web client — but deliberately **no identity provider**. Beam is bring-your-own-IdP (FR-101),
so that is the production shape: login stays disabled with a clear error until `BEAM_OIDC_*` point
at your own provider.

The bundled dev Dex sits behind the `dev-idp` compose profile and is opt-in. `mise run dev:up`
enables the profile and supplies the matching `BEAM_OIDC_*` wiring in one step; it is the
supported way to get a working login locally, and is for development only.

### Dependency services (`compose.dependencies.yaml`)

| Service | Role |
|---|---|
| `postgres` | Postgres 18, the sole datastore: catalog, sessions, enrichment state, admin logs. Healthchecked via `pg_isready`; data in the `postgres` named volume. |
| `dex` | Dev-only OIDC IdP with static test users (`admin@beam.localhost` / `user@beam.localhost`, password `password`; see `dex/config.yaml`). **Opt-in**: it is gated behind the `dev-idp` compose profile and does not start with a bare `compose up`. Use `mise run dev:up`, which enables the profile and sets the matching `BEAM_OIDC_*`. Production deployments point `BEAM_OIDC_ISSUER` at a real IdP (Keycloak, Authentik, Authelia, or a hosted provider) instead. |
| `traefik` | TLS termination and `Host()`-rule routing (`server.beam.localhost` → server, `beam.localhost` → web), HTTP→HTTPS redirect, HTTP/3, dashboard bound to loopback. The server router disables response buffering (`flushInterval: "-1"`) for low-latency media delivery. The bundled setup uses self-signed certs for `*.beam.localhost`; bring your own domain/certificate configuration for production. |

### Application services (`compose.beam.yaml`)

| Service | Role |
|---|---|
| `server` | The `beam-server` binary (built from `beam-server/Containerfile`): HTTP API, OIDC auth, in-process indexing/enrichment, direct-play streaming. Mounts the media library read-only at `BEAM_VIDEO_DIR` and server-writable state at `BEAM_DATA_DIR` (host paths via `HOST_VIDEO_DIR`/`HOST_DATA_DIR`, or the `server_videos`/`server_data` named volumes by default). Healthchecked via `GET /v1/health`. Depends only on `postgres` — deliberately not on `dex`, because a `depends_on` naming a profile-gated service makes the profile-less project invalid outright; `mise run dev:up` sequences Dex first instead. |
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
     static users are for local development only — never enable the `dev-idp` profile in
     production.
   - Grant admin via your IdP, not the server: configure the IdP to release a claim (e.g. a Dex/
     Keycloak group) and set `BEAM_OIDC_ADMIN_CLAIM` (e.g. `groups`) and, for a value/array match,
     `BEAM_OIDC_ADMIN_VALUE` (e.g. `beam-admin`). Admin is recomputed on every login — granting
     **and** revoking. Leaving `BEAM_OIDC_ADMIN_CLAIM` unset means nobody is admin.
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

   With `BEAM_ENABLE_METRICS=false` the route does **not** disappear: no recorder is installed, and
   `GET /metrics` answers `503` with an RFC 9457 problem document rather than `404`. The router's
   shape — and therefore the OpenAPI document it exports — must not depend on deployment
   configuration, or the description stops covering every deployment it claims to
   ([ADR-0010](../architecture/decisions/ADR-0010-openapi-3-2-kynos.md)). A scrape configuration
   that treats `503` as "target down" is reading it correctly; there is nothing to collect.
   `/metrics` is a described operation tagged `internal`, so it appears in the document and is
   deliberately outside the `/v1` client contract — keeping it out of the reverse proxy is still
   the control, since it carries no authentication of its own.

Then `podman compose up -d`, and verify: `https://<your-domain>/v1/health` returns OK, the web
app loads, login round-trips through your IdP, and an admin user can create a library pointing at
a path under `BEAM_VIDEO_DIR` and trigger a scan.
