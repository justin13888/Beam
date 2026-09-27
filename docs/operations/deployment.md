# Deployment

Beam has two supported deployments, both running exactly one `beam-server` process: a single host
running Podman/Docker Compose (below), and Kubernetes through the Helm chart in `charts/beam`
([Kubernetes](#kubernetes-helm), [ADR-0018](../architecture/decisions/ADR-0018-kubernetes-helm-chart.md)).
Running more than one server replica is not supported on either: the indexer and enrichment worker
are not leader-elected. The modular-monolith design
([ADR-0001](../architecture/decisions/ADR-0001-modular-monolith.md)) keeps a future split possible
without a rewrite.

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
| `dex` | Dev-only OIDC IdP with static test users (`admin@beam.localhost` / `user@beam.localhost`, password `password`; see `dex/config.yaml`). Only `admin@` is in the `beam-admin` group, which `mise run dev:up` maps to admin via `BEAM_OIDC_ADMIN_CLAIM=groups`, so `user@` is a non-admin. **Opt-in**: it is gated behind the `dev-idp` compose profile and does not start with a bare `compose up`. Use `mise run dev:up`, which enables the profile and sets the matching `BEAM_OIDC_*`. Production deployments point `BEAM_OIDC_ISSUER` at a real IdP (Keycloak, Authentik, Authelia, or a hosted provider) instead. Listens on `5556` on both the host and the container; that port is part of the issuer string, which the browser reaches via the host mapping and the server via the compose alias, so it has no `*_HOST_PORT` override. |
| `traefik` | TLS termination and `Host()`-rule routing (`server.beam.localhost` → server, `beam.localhost` → web), HTTP→HTTPS redirect, HTTP/3, dashboard bound to loopback. The server router disables response buffering (`flushInterval: "-1"`) for low-latency media delivery. The bundled setup uses self-signed certs for `*.beam.localhost`; bring your own domain/certificate configuration for production. |

### Application services (`compose.beam.yaml`)

| Service | Role |
|---|---|
| `server` | The `beam-server` binary (built from `beam-server/Containerfile`): HTTP API, OIDC auth, in-process indexing/enrichment, direct-play streaming. Mounts the media library read-only (`:ro`, FR-202; `check:compose-invariants` fails if it is dropped) at `BEAM_VIDEO_DIR` and server-writable state at `BEAM_DATA_DIR` (host paths via `HOST_VIDEO_DIR`/`HOST_DATA_DIR`, or the `server_videos`/`server_data` named volumes by default). Healthchecked via `GET /v1/health`. Depends only on `postgres` — deliberately not on `dex`, because a `depends_on` naming a profile-gated service makes the profile-less project invalid outright; `mise run dev:up` sequences Dex first instead. |
| `web` | The `beam-web` SPA (built from `beam-web/Containerfile`, which generates the typed client from the `beam-web/openapi.json` supplied in the build context), served as static files by Caddy. Depends on a healthy `server`. |

## Database migrations

`beam-server` applies pending migrations at startup (`BEAM_AUTO_MIGRATE`, default `true`), so a
container-only deployment needs no separate migration step. Every migrator serialises on a
Postgres advisory lock (`pg_advisory_xact_lock`, key `beam_migration::MIGRATION_LOCK_KEY`, held for
the duration of the batch): if two processes start against the same database at once -- an
overlapping restart, a Kubernetes pod replaced while its predecessor is still terminating, the CLI
run beside a live server -- the second waits, then finds nothing pending. The lock makes
migrate-on-boot safe; it does not make Beam multi-replica (the indexer is not leader-elected), so
the supported topologies still run exactly one server process. Set `BEAM_AUTO_MIGRATE=false` to manage schema out-of-band with the `beam-migration` CLI
(`cargo run -p beam-migration -- up|down|status` with `DATABASE_URL` set).

Pending migrations apply all-or-nothing, at startup and through `beam-migration up` alike: they
run in one transaction, so if any migration in the batch fails, none of them is committed and the
server exits with the error. The database stays at the schema the previous release expects, so
rolling back to the previous image is safe after a failed upgrade. Only `up` is batched this way:
`beam-migration down`, `fresh` and `refresh` still commit one migration at a time.

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
   - Grant admin via your IdP, not the server: configure the IdP to release a claim (e.g. a
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

## Kubernetes (Helm)

`charts/beam` deploys Beam on Kubernetes 1.29 or later. The decisions behind its shape are in
[ADR-0018](../architecture/decisions/ADR-0018-kubernetes-helm-chart.md); `charts/beam/values.yaml`
documents every value, and `values.schema.json` rejects unknown ones.

| Object | Role |
|---|---|
| `Deployment` (server) | One `beam-server` pod, `replicas: 1`, strategy `Recreate`. There is no replica value: the indexer and enrichment worker run in-process without leader election, and rate limits and the admin event stream are in memory. Non-root (uid 1000), read-only root filesystem, all capabilities dropped, `RuntimeDefault` seccomp, no service-account token. |
| `Service` | `ClusterIP` on port 8000. |
| `PersistentVolumeClaim` | `/data` (`BEAM_DATA_DIR`), 10Gi `ReadWriteOnce` by default, or `persistence.data.existingClaim`. Kept on uninstall (`helm.sh/resource-policy: keep`). `persistence.data.enabled: false` uses an `emptyDir`, lost whenever the pod is replaced. |
| `Secret` | Only for secrets given inline (`database.url`, `oidc.clientSecret`, `tmdb.apiToken`); each also accepts an `existingSecret`. |
| `Ingress` | Optional. Routes `/v1` to the server and, with the web client enabled, `/` to it. `/metrics` and `/openapi` are never routed. |
| `Deployment` + `Service` (web) | Optional; see below. |

**Database.** Postgres is external: set `database.url` or `database.existingSecret` (key
`existingSecretKey`, default `url`). The chart refuses to render with neither. Use the version
Compose and CI run (18); the database user needs `CREATE` in the database, because a migration
creates the `pg_trgm` extension (a trusted extension, so the owner does not need superuser).
[CloudNativePG](https://cloudnative-pg.io/) is the recommended way to run Postgres on the same
cluster.

**Migrations.** The server migrates on boot under the advisory lock described in
[Database migrations](#database-migrations), so the old pod of a rollout, a rescheduled pod, and a
`beam-migration up` run by hand cannot race it. The startup probe allows five minutes for the server's own
database retry plus migrations before liveness takes over.

**Libraries.** Each entry in `libraries` names a Kubernetes volume source and is mounted read-only
at `/videos/<name>`; create the library in the admin UI with that path. A `persistentVolumeClaim` or
`nfs` source is forced read-only as well. `BEAM_VIDEO_DIR` (`/videos`) and `BEAM_DATA_DIR` (`/data`)
are fixed by the chart and cannot be overridden through `server.env`.

**Probes.** Startup and readiness use `GET /v1/health`, which answers 503 while the database is
unreachable -- the pod leaves the Service until it recovers. Liveness is a TCP check, so a database
outage does not restart the server.

**Ingress and proxies.** `ingress.host` is required with `ingress.enabled`. With an ingress the rate
limiter keys on `X-Forwarded-For` (`rateLimit.trustForwardedFor`, which follows `ingress.enabled`
unless set); that is only safe while clients cannot reach the Service directly. Set
`server.publicUrl` to the origin clients use -- the OIDC redirect URI is derived from it.

**Web client.** `web.enabled` is off by default. The published `beam-web` image bakes its API origin
in at build time as `http://localhost:8000`, so enabling it requires an image built for your origin
and pushed to a registry the cluster can pull from:

```sh
mise run codegen:openapi
podman build -f beam-web/Containerfile \
  --build-arg C_STREAM_SERVER_URL=https://beam.example.com \
  -t registry.example.com/beam-web:beam.example.com-v0.1.0 .
```

then set `web.image.repository` and `web.image.tag`. With `web.enabled` and no `server.webUrl`, the
chart sets `BEAM_WEB_URL` to `server.publicUrl`, since both are served from the same origin.

**Metrics.** `metrics.enabled` sets `BEAM_ENABLE_METRICS` and adds `prometheus.io/*` annotations to
the pod for an in-cluster scraper.

**Images.** `image.tag` defaults to `v<appVersion>`, the tag the release that carries the chart
publishes. No release has been cut yet, so until one is, build and push `beam-server/Containerfile`
yourself and set `image.repository` and `image.tag`.

The chart is gated by `mise run helm:lint`, `helm:template` (kubeconform against Kubernetes 1.29 and
1.37), and `check:chart-invariants`, which asserts over the rendered manifests for every scenario in
`charts/beam/ci/` that the server has one `Recreate` replica, every `/videos` mount and its
PVC/NFS source is read-only, `/data` is never a library, every container is hardened, liveness does
not probe `/v1/health`, and the ingress routes the server only under `/v1`. All three run in
`mise run ci`, CI's `helm-chart` job, and the pre-push hook.
