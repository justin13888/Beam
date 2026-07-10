# Beam Domain

Core domain models and trait abstractions shared by `beam-index` and `beam-server`. This crate
is deliberately framework- and infrastructure-agnostic: no `salvo`, no `sea-orm` (the optional
`entity` feature bridges to `beam-entity` for conversions, but the models themselves don't depend
on it), no `ffmpeg`. See [`docs/architecture/components.md`](../docs/architecture/components.md) for the
current architecture.

## Structure

- **`models`** -- plain data structs: `Library`, `MediaFile`, `Movie`/`MovieEntry`, `Show`/
  `Season`/`Episode`, `Genre`, `AdminLog`, `PlaybackProgress`, `MediaStream`, and the enrichment
  queue's row type.
- **`repositories`** -- one trait per aggregate (`LibraryRepository`, `FileRepository`,
  `MovieRepository`, `ShowRepository`, `GenreRepository`, `PlaybackProgressRepository`,
  `MediaStreamRepository`, `AdminLogRepository`, `EnrichmentStateRepository`), each with an
  `InMemory*` fake alongside the trait definition. `beam-index`/`beam-server` provide the
  sea-orm-backed implementations.
- **`providers`** -- `EnrichmentProvider`: a provider-agnostic trait for external metadata lookup
  (search/enrich movies, shows, seasons; invalidate a cached match), returning `beam-domain`'s own
  DTOs rather than leaking any specific provider's types. `beam-index`'s `cameo` adapter is the
  only production implementation; an `InMemoryEnrichmentProvider` fake exists under `test-utils`.
- **`utils`** -- pure helper functions, notably the scene-release filename parser (strips bracket
  groups, detects `SxxEyy`, applies the rightmost-standalone-year rule for titles like "Blade
  Runner 2049 2017").

## Testing

Every repository trait ships with a hand-written `InMemory*` fake (real stateful behavior --
insert/find/update/delete against an in-memory map -- not a mock), so services that depend on
`Arc<dyn Trait>` can be tested with zero infrastructure. The `test-utils` feature additionally
generates `mockall::automock` mocks for strict contract verification where that's the more
appropriate tool. See [`docs/testing.md`](../docs/testing.md).
