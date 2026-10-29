# Beam Index

In-process media indexing: filesystem scanning, live filesystem watching, FFmpeg-based technical
probing, and metadata-enrichment sweeps. This runs as a set of background tasks inside the
`beam-server` binary -- there is no separate indexer process or `beam-index` binary. See
[`docs/architecture/components.md`](../docs/architecture/components.md) for the current architecture.

## Structure

- **`probe`** -- the only module in the workspace linking `ffmpeg-next` (technical media probing:
  codecs, resolution, HDR/color metadata, duration, stream flags). Confined here specifically so
  no other crate needs an FFmpeg dependency; `beam-domain`/`beam-server` don't link it at all.
- **`providers`** -- adapters behind `beam-domain`'s provider traits. `cameo` is the only module
  depending on the [`cameo`](https://crates.io/crates/cameo) SDK, so nothing else in the workspace
  sees a `cameo` type; `tvdb` is Beam's own TheTVDB v4 client, and `composite` asks cameo's
  providers and then TheTVDB (see
  [ADR-0006](../docs/architecture/decisions/ADR-0006-cameo-enrichment.md)). `artwork` fetches
  artwork images for the server's cache, and `telemetry` sends the anonymous library report.
- **`disc`** and **`library_file`** -- reading DVD/Blu-ray folder structures, and opening files
  under a library root read-only and without following links.
- **`repositories`** -- sea-orm implementations of `beam-domain`'s repository traits.
- **`runtime`** -- wires and spawns the background tasks: startup scan, filesystem watcher,
  periodic full rescan (backstop for anything the watcher missed), and the metadata-enrichment
  sweep loop.
- **`services`** -- `IndexService` (scan/classify/reconcile a library against the filesystem),
  `MetadataEnrichmentService` (matches classified titles against enrichment providers and applies
  results), plus smaller services (`HashService`, `AdminLogService`, `NotificationService`,
  `MediaInfoService`, and the trickplay sheet generator).

## Testing

`services::index`'s tests exercise scan/classify/reconcile logic against `InMemory*` repository
fakes over a real `TempDir` library, with a test double or the real FFmpeg prober as the case
needs; the filesystem watcher has its own `InMemoryFsWatcher` (synthetic events) alongside the
real `notify`-backed one, and `beam-domain`'s `Clock`/`TestClock` let enrichment-worker
backoff/retry timing be tested without real sleeps. `MockIndexService`
(`mockall`, under `test-utils`) is what `beam-server`'s route tests use to fake a scan without
touching a filesystem. See [`docs/testing.md`](../docs/testing.md).
