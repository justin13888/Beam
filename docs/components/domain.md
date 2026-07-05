# Component: `beam-domain`

Status: describes the target module layout. Where current code differs, that is called out
explicitly as "today." See [docs/architecture/overview.md](../architecture/overview.md) and
[ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md) (the codec-representation
consequence) and [ADR-0006](../architecture/decisions/ADR-0006-cameo-enrichment.md) (the
`EnrichmentProvider` trait).

## Role

`beam-domain` is the framework-agnostic core of the system: domain models, repository traits,
provider traits, and small pure utility functions. Per CLAUDE.md's domain-isolation rule, it must
never depend on a web framework (no Salvo types), never build SeaORM queries directly (only trait
signatures live here — query-building is a repository-implementation concern in `beam-index`/
`beam-auth`), and — new for this push — never depend on `ffmpeg-next` or any FFmpeg FFI type. This
crate is the thing that lets services be tested purely against in-memory fakes.

## The FFmpeg dependency is being removed

Today, `beam-domain` depends directly on `ffmpeg-next` and leaks its types into public domain
structures:

- `utils/media.rs`'s `CodecId` enum has an `Other(ffmpeg::ffi::AVCodecID)` variant — a raw FFI type
  escaping into what is supposed to be a pure domain enum — plus `From<ffmpeg::codec::Id>` and
  `From<ffmpeg::media::Type>` conversions.
- `utils/format.rs` defines `Resolution`, `SampleFormat`, `ChannelLayout`, `Disposition` with
  `From<ffmpeg::...>` conversions, including an `unsafe` block calling
  `av_channel_layout_describe` directly.
- `utils/color.rs` defines a `PixelFormat` enum mirrored from `ffmpeg_next`'s pixel format list.
- The crate's `vendored-ffmpeg` Cargo feature exists purely to let `ffmpeg-next` statically vendor
  FFmpeg for local dev without system `.pc` files.

All of this moves out. `CodecId::Other(AVCodecID)` is fixed by making codecs plain strings/enums with
no FFI payload (mirroring how `beam_domain::models::stream::MediaStream.codec` is already a plain
`String` today — the FFI leak is specifically in the currently-unused-by-the-DB-model `utils/media.rs`
helper types). The FFmpeg-backed probing logic that produces this data moves to `beam-index`'s
`prober` module (see [indexer.md](indexer.md)), which becomes the only crate in the workspace linking
`ffmpeg-next`. `beam-domain` drops the `ffmpeg-next` dependency, the `vendored-ffmpeg` feature, and
the FFI-shaped utility types entirely.

## Module layout

- **`models/`** — plain domain structs and their `#[cfg(feature = "entity")] impl From<beam_entity::X::Model>`
  conversions (the `entity` feature is optional so `beam-domain` can be compiled/tested without
  pulling in `beam-entity`/`sea-orm` at all). Existing: `Movie`/`MovieEntry` (`movie.rs`), `Show`/
  `Season`/`Episode` (`show.rs`), `MediaFile`/`FileStatus`/`MediaFileContent` (`file.rs`), `Library`
  (`library.rs`), `MediaStream`/`StreamType`/`VideoStreamMetadata`/`AudioStreamMetadata`/
  `SubtitleStreamMetadata` (`stream.rs`), `AdminLog`/`AdminLogLevel`/`AdminLogCategory`
  (`admin_log.rs`). New this push: `EnrichmentState` and `ExternalMediaRef` types (mirroring the new
  `metadata_enrichment` table's status/provider/attempt fields — see
  [data-model.md](../architecture/data-model.md)), and a `Genre` model (the `genres` table exists
  today but has no corresponding `beam-domain` model or repository — see below).
- **`repositories/`** — one trait per aggregate, each with an `in_memory` submodule providing an
  `InMemory*Repository` fake (per CLAUDE.md's fakes-over-mocks rule) and, where exercised via
  `mockall::automock` behind `test-utils`, a generated mock for strict contract tests. Existing:
  `MovieRepository`, `ShowRepository`, `FileRepository`, `LibraryRepository`,
  `MediaStreamRepository`, `AdminLogRepository`. New this push: `GenreRepository` (the `genres`/
  `movie_genres`/`show_genres` tables already exist in `beam-migration`/`beam-entity` but are
  unused — nothing in `beam-domain` currently models them as a repository) and
  `EnrichmentStateRepository` (backing `metadata_enrichment`).
- **`providers/`** — external-system abstractions. Today this is `metadata.rs`'s `MetadataProvider`
  trait: dead scaffolding — nothing in the codebase constructs a real implementation, and its
  `InMemoryMetadataProvider` test fake is exercised only by its own unit tests. This push replaces it
  with `EnrichmentProvider` (same shape of responsibility — search/get movie and show metadata by
  external ID, resolve image URLs — but the name reflects that it now actually drives the enrichment
  pipeline in `beam-index`). Ship an `InMemoryEnrichmentProvider` fake (test-utils) and a
  `NoopEnrichmentProvider` — a production-safe default that always returns "not found" without making
  any network call, so a `beam-server`/`beam-index` build can run with enrichment disabled without a
  panic or a `dyn` object that isn't provided. The concrete `cameo`-backed TMDB/AniList implementation
  lives in `beam-index`, not here — `beam-domain` stays free of any specific external SDK dependency
  per CLAUDE.md's trait-based-abstraction rule.
- **`utils/`** — shared pure helpers with no FFmpeg types. `hash.rs` (`XXH3Hash`, `compute_hash` —
  unchanged). `file.rs` (`FileType` — unchanged). New this push: filename-parsing helpers backing
  `beam-index`'s `parser` module (kept here if reusable/pure; kept in `beam-index` if provider-shaped
  — the parsing *algorithm* is a pure-function candidate for `beam-domain`, while the *pipeline* that
  invokes it as part of a scan stays in `beam-index`). `format.rs`/`color.rs`/the FFmpeg-derived
  portions of `media.rs` are deleted; whatever of `media.rs`'s `MediaType`/`Discard` concepts are
  still needed become plain enums with no `ffmpeg_next` import.

## Testing expectations

Every repository trait and the `EnrichmentProvider` trait must be usable in tests without a database
or network call — the `in_memory`/`test_utils` submodules already established for `MovieRepository`
and `MetadataProvider` are the pattern to follow for the new `GenreRepository`,
`EnrichmentStateRepository`, and `EnrichmentProvider` work. `#[cfg(any(test, feature = "test-utils"))]`
gates every fake so `cargo build --release` never pulls test-only code into a production binary.
