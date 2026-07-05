# Component: `beam-index`

Status: describes the target module layout. Where current code differs, that is called out
explicitly as "today." See [docs/architecture/overview.md](../architecture/overview.md) for how this
crate fits into the system and
[ADR-0006](../architecture/decisions/ADR-0006-cameo-enrichment.md) for the enrichment design.

## Role

`beam-index` owns everything about turning files on disk into rows in the catalog: scanning,
change detection, technical metadata probing, filename parsing, movie/show classification, and
metadata enrichment from external providers. It is a **library-only crate** in the target state —
called in-process by `beam-server` via the `IndexService` trait — with no standalone binary and no
RPC layer.

## What is being deleted

Today, `beam-index` is built as a second binary (`src/main.rs`) that runs its own gRPC server
(`src/grpc.rs`, `IndexServiceGrpc`) exposing a single `ScanLibrary` RPC, defined via a `.proto` file
compiled at build time with `tonic-build`/`prost` (`build.rs`), and consumed by `beam-stream` over
`tonic` (`services/grpc_index.rs::GrpcIndexService`). All of that — the `beam-index` binary target,
`src/grpc.rs`, the `proto/` definitions and `build.rs`, and the `tonic`/`prost`/`tonic-build`
dependencies — is deleted. `beam-index`'s `src/lib.rs` no longer exposes a `proto` module. Instead,
`IndexService` (already the trait `beam-server` should have been calling all along) is called
directly as an `Arc<dyn IndexService>`, constructed in `beam-server`'s `state.rs`.

## Module layout (target)

Today's `services/index.rs` is a single large module that does scanning, walking, regex-based
`SxxEyy` episode detection, and movie/show find-or-create in one place. This push splits it into
focused sub-systems:

- **`scanner`** — `WalkDir`-based directory traversal (today's scan loop in `services/index.rs`,
  extracted). Discovers candidate files under a library root and hands them to the reconciler.
- **`watcher`** — unchanged in spirit, already reasonably isolated today: `services/watcher.rs`
  defines the `FsWatcher` trait, the production `NotifyFsWatcher` (the `notify` crate, inotify on
  Linux) plus its debouncer (`PathDebouncer`), and `InMemoryFsWatcher`/`MockFsWatcher` for tests. Feeds
  live filesystem change events (`FsEvent`/`FsEventKind`) that trigger targeted rescans between full
  scan cycles.
- **`prober`** — ffmpeg-next-based technical metadata extraction (resolution, codec, duration,
  frame rate, bit rate, channel layout, HDR/color info). This is today's `services/media_info.rs`
  (`MediaInfoService`/`LocalMediaInfoService`, wrapping `utils/metadata.rs`'s `VideoFileMetadata`),
  renamed/reframed as the prober. **This is where `beam-domain`'s FFmpeg usage moves to** — see
  [domain.md](domain.md): `beam-domain` drops `ffmpeg-next` this push, and `beam-index` becomes the
  only crate in the workspace linking it (for probing at index time; never at stream time, per
  [ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md)). The `vendored-ffmpeg` Cargo
  feature (statically compiling FFmpeg from source for local dev without system `.pc` files, versus
  dynamically linking a system-provided FFmpeg in CI/container builds — see
  [ADR-0007](../architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md)) moves fully to
  `beam-index`; it no longer needs to forward through `beam-domain/vendored-ffmpeg` since
  `beam-domain` no longer links `ffmpeg-next` at all.
- **`parser`** (new) — a hand-rolled scene-filename parser extracting a clean title and year from a
  release filename, replacing today's bare `EPISODE_REGEX` (`(?i)S(\d+)E(\d+)`) in
  `services/index.rs`, which only detects season/episode numbers and does no title/year cleanup at
  all.
- **`classifier`** — movie-vs-episode classification and movie/show find-or-create logic (today
  embedded in `services/index.rs`'s scan loop, using `MovieRepository`/`ShowRepository`). Pulled out
  into its own module so scanning and classification can be tested and reasoned about independently.
- **`enrichment`** (new) — the `EnrichmentProvider` trait consumer: a `cameo`-backed adapter hitting
  TMDB and AniList, matching/scoring logic to pick the best candidate for a title, and an async
  worker with retry/backoff driven off the new `metadata_enrichment` queue table (see
  [data-model.md](../architecture/data-model.md)). This replaces the dead `MetadataProvider` scaffold
  in `beam-domain` (see [domain.md](domain.md)) — the trait moves/is renamed to `EnrichmentProvider`
  in `beam-domain::providers`, and the concrete cameo-backed implementation lives here in
  `beam-index`, since `beam-domain` must stay free of any specific external SDK dependency.
- **`reconciler`** — mtime/size/hash-based change detection on rescans (today's
  `read_fs_meta`/`FileStatus` handling in `services/index.rs`, using the `files.mtime` column added in
  `m20260522_000001_add_file_mtime`) plus content-hash dedup (XXH3, via `services/hash.rs`'s
  `HashService`). Decides, for each discovered path, whether it's new, unchanged, changed, or a
  duplicate of an existing row.

`services/admin_log.rs` and `services/notification.rs` (`AdminLogService`, `NotificationService`/
`AdminEvent`) are retained as-is — they back the admin log and the SSE progress endpoint that
`beam-server` exposes.

## Repositories

`beam-index/src/repositories/` holds the SeaORM-backed implementations of `beam-domain`'s repository
traits: `SqlLibraryRepository`, `SqlFileRepository`, `SqlMovieRepository`, `SqlShowRepository`,
`SqlMediaStreamRepository`, `SqlAdminLogRepository`. Per CLAUDE.md's trait-based-abstraction and
fakes-over-mocks rules, every one of these has a corresponding `InMemory*` fake defined alongside the
trait in `beam-domain::repositories` (used here and in `beam-server`'s tests) — `beam-index` itself
should not need `mockall` for these, only `test-utils`-gated `InMemory*` construction. This push adds
two new repositories to this list, backing the new domain traits described in
[domain.md](domain.md): a `GenreRepository` implementation and an `EnrichmentStateRepository`
implementation (backing the new `metadata_enrichment` table).

## Testing expectations

FFmpeg usage is confined entirely to the `prober` module. Every other module (`scanner`, `parser`,
`classifier`, `reconciler`, `enrichment`'s matching/scoring logic) must be testable without a real
video file or a real FFmpeg build — feed them synthetic `VideoFileMetadata`/filenames and drive them
against `InMemory*` repositories, per CLAUDE.md's zero-infrastructure testing rule. The `watcher`
module's `InMemoryFsWatcher::emit` already demonstrates the intended pattern: synthetic events in,
no real filesystem, no real inotify.
