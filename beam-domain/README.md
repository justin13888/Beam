# Beam Domain

Core domain models and trait abstractions shared by `beam-index` and `beam-server`. This crate
is deliberately framework-agnostic: no web framework (`kynos` lives in `beam-server` alone), no
sea-orm query building, no `ffmpeg`. It does depend on `sea-orm`, for one reason: the repository
traits spell their error as `sea_orm::DbErr`. The optional `entity` feature bridges to
`beam-entity` for row conversions; the models themselves don't depend on it. See
[`docs/architecture/components.md`](../docs/architecture/components.md) for the current
architecture.

## Structure

- **`models`** -- plain data structs: libraries, files, movies, shows, seasons and episodes,
  genres, people, collections, lists, watch state, streams, chapters, access policies, language
  preferences, playback telemetry, and the enrichment queue's row type.
- **`repositories`** -- one trait per aggregate (`repositories/mod.rs` re-exports every one), each
  with an `InMemory*` fake alongside the trait definition and, in `repositories/contract/` (one file per
  aggregate, fixtures in `mod.rs`), the shared behavioural contract its implementations are bound to. `beam-index` provides the
  sea-orm-backed implementations.
- **`providers`** -- `EnrichmentProvider`: a provider-agnostic trait for external metadata lookup
  (search/enrich movies, shows, seasons; invalidate a cached match), returning `beam-domain`'s own
  DTOs rather than leaking any specific provider's types. `beam-index` implements it with a
  `cameo` adapter (TMDB, AniList), a TheTVDB adapter, and a composite over the two; an
  `InMemoryEnrichmentProvider` fake exists under `test-utils`. `ArtworkFetcher` is the seam for
  fetching artwork images, and `TelemetrySink` the one for sending the anonymous library report.
- **`services`** -- the canonical seams for time (`Clock`: `RealClock`, `TestClock`) and
  identifiers (`IdGenerator`: `UuidGenerator`, `SequentialIdGenerator`).
- **`utils`** -- pure helper functions, notably the scene-release filename parser (strips bracket
  groups, detects `SxxEyy`, applies the rightmost-standalone-year rule for titles like "Blade
  Runner 2049 2017").

## Testing

Every repository trait ships with a hand-written `InMemory*` fake (real stateful behavior --
insert/find/update/delete against an in-memory map -- not a mock), so services that depend on
`Arc<dyn Trait>` can be tested with zero infrastructure. The `test-utils` feature additionally
generates `mockall::automock` mocks for strict contract verification where that's the more
appropriate tool. See [`docs/testing.md`](../docs/testing.md).
