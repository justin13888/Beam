# ADR-0018: Kubernetes through a single-replica Helm chart

## Status

Accepted. Settles [#76](https://github.com/justin13888/beam/issues/76). Builds on
[ADR-0001](ADR-0001-modular-monolith.md), whose modular monolith it deploys unchanged, and on
[ADR-0009](ADR-0009-release-engineering.md), whose release train now versions the chart.

## Context

Compose on one host was the only supported deployment, and #76 recorded Kubernetes as a long-term
aspiration with two prerequisites: published container images, and "a story for the migrate-on-boot
single-writer assumption". #72 shipped the images. The second prerequisite is the substance.

`beam-server` applies pending migrations at startup, and nothing coordinated two processes doing so
against one database. On Kubernetes two processes overlap routinely -- a rollout, a pod rescheduled
while its predecessor is still terminating, an operator running `beam-migration up` beside a live
pod -- and two migrators that both read an empty ledger both run the same DDL; the loser exits with a
duplicate-object error.

Migrations are not the only single-writer assumption. The background indexer and the enrichment
worker run in-process with no leader election, and the rate limiter's buckets and the admin event
stream live in memory. A second replica would double every scan and enrichment pass and split that
state, whatever the migrator did.

## Decision

**Migrations serialise on a Postgres advisory lock.** `up_all_or_nothing` -- the one path every
migrator takes (startup, the CLI, the `pg-integration` tier) -- takes
`pg_advisory_xact_lock(MIGRATION_LOCK_KEY)` inside its batch transaction before reading the ledger.
A second migrator waits, then reads what the first committed and finds nothing pending. The lock is
transaction-scoped, so a migrator that dies cannot leave it held. Nothing runs DDL before the lock:
sea-orm's ledger `CREATE TABLE IF NOT EXISTS` is not safe against a concurrent creator (the second
fails with a unique violation on `pg_type`), so the server's startup count of pending migrations is
read inside the locked batch (`beam_migration::apply_pending`), and `beam-migration status` reads the
ledger read-only instead of creating it. This is what makes migrate-on-boot safe on Kubernetes, and
it is covered by deterministic `pg-integration` tests that fail without it: two migrators racing,
and the server's exact startup sequence starting beside an in-flight `beam-migration up` on a fresh
database.

**The chart runs exactly one server replica, with the Recreate strategy.** There is no
`replicaCount` value at all -- the schema rejects one. Recreate means the old pod is gone before the
new one starts, so two indexers never overlap and a `ReadWriteOnce` data volume is never wanted by
two pods. Horizontal scaling waits for leader election of the background work (after #181's scan
serialisation) and for shared rate-limit and event state; it is not a chart setting.

**Postgres is external.** The chart takes `database.url` (stored in a Secret it creates) or
`database.existingSecret`, and refuses to render with neither. Running a database well is the job of
an operator such as CloudNativePG, not of an application chart.

**Libraries are read-only by construction.** Each entry in `libraries` is any Kubernetes volume
source, mounted at `/videos/<name>` with `readOnly: true`; a `persistentVolumeClaim` or `nfs` source
is forced read-only as well. `BEAM_VIDEO_DIR` and `BEAM_DATA_DIR` are fixed, not values, so no
override can move a library outside `/videos` or the state onto a library. This is FR-202's promise,
kept on Kubernetes the way `check:compose-invariants` keeps it on Compose.

**Probes follow what each can fix.** A startup probe on `/v1/health` allows five minutes for the
server's own database retry plus migrations. Readiness is `/v1/health`, which answers 503 while the
database is down, taking the pod out of the Service. Liveness is a TCP check: `/v1/health` there
would turn a database outage into a restart loop that fixes nothing.

**The pod is hardened by default.** Non-root (the image's uid 1000), read-only root filesystem,
every capability dropped, no privilege escalation, `RuntimeDefault` seccomp, and no service-account
token -- Beam never calls the Kubernetes API.

**The ingress exposes the API only.** `/v1` routes to the server and, when the web client is
enabled, `/` to it. `/metrics` and the `/openapi` docs are never routed; `metrics.enabled` annotates
the pod for an in-cluster scraper instead. With an ingress, the rate limiter trusts
`X-Forwarded-For` by default, since every peer is the ingress controller.

**The web client is opt-in and needs an operator-built image.** The published `beam-web` image
bakes its API origin in at build time as `http://localhost:8000`, which cannot serve a cluster.
`web.enabled` therefore requires `web.image.repository` and `web.image.tag`, with no default.

**The chart is versioned with the product.** `charts/beam/Chart.yaml`'s `version` and `appVersion`
are release-please extra-files, and the image tag defaults to `v<appVersion>`.

**Three gates, over rendered output.** `helm:lint` (strict, schema included), `helm:template`
(kubeconform `-strict` against the oldest supported and a current Kubernetes), and
`check:chart-invariants`, which asserts the decisions above over what the chart renders for every
scenario in `charts/beam/ci/`. All three run in `mise run ci`, CI's `helm-chart` job, and the
pre-push hook.

## Consequences

- Kubernetes is a supported topology, with the same single-server shape as Compose. Nothing in
  `beam-server` changed except the migration lock, which Compose deployments get too.
- The advisory-lock key shares Postgres's one advisory-lock keyspace with anything else on that
  database. It is a documented constant and must never change: two releases that disagree on it
  would not exclude each other during an upgrade.
- An upgrade has a short outage while the old pod stops and the new one starts. That is the price of
  Recreate, and the same outage a Compose `up -d` has.
- No release has been cut yet, so the default image tag does not exist until one is. Until then an
  operator builds and pushes the image and sets `image.tag`.
- The chart is not yet published to a repository; it installs from a checkout.

Deferred, each a follow-up rather than part of this decision: publishing the chart as an OCI
artifact from `release.yml`; a runtime-configurable API origin for `beam-web` so the published image
works on any host; Gateway API `HTTPRoute` alongside `Ingress`; a local kind + Skaffold
development loop (scope the issue's owner added; the user guide ships an example Argo CD
`Application` for the chart, but no dev-loop tooling); and multi-replica operation, which needs
leader election first.
