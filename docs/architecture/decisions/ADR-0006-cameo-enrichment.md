# ADR-0006: cameo-based metadata enrichment via a provider-agnostic trait

## Status

Accepted.

## Context

Today, the indexer creates movies/shows title-only, parsed from filenames via a bare `SxxEyy` regex
plus a parent-directory-name heuristic. A `MetadataProvider` trait exists in `beam-domain`, but it is
pure dead scaffolding: zero production implementation, never wired into any service. Every metadata
column (`year`, `description`, `poster_url`, ratings, `tmdb_id`, etc.) stays NULL forever, and the
`genres`/`movie_genres`/`show_genres` tables are never populated, because nothing ever writes to
them. This leaves Beam's catalog looking like a flat file listing rather than a real media library,
which is a core product gap, not a cosmetic one.

## Decision

Add a hand-rolled scene-filename parser (title + year extraction, stripping resolution/codec/
release-group noise) to replace the bare `SxxEyy` regex, feeding a background enrichment pipeline that
runs strictly after scan/classify completes (never blocking indexing). The pipeline queries TMDB
(optional API key) and AniList (keyless, always available) through the `cameo` crate — a unified Rust
SDK for both providers, from the same author as Beam — behind a new, provider-agnostic
`EnrichmentProvider` trait that replaces the dead `MetadataProvider` scaffold. Tests never touch the
network: an in-memory fake `EnrichmentProvider` drives all pipeline tests. Enrichment attempts are
tracked per-title in a new `metadata_enrichment` queue/status table with retry/backoff, and the
previously-dead genre tables get populated as a direct result. Poster/backdrop URLs are stored as
direct CDN links — no server-side image proxy this push, a documented tradeoff (see `security.md`).
Re-enrichment of a title is available as a user-triggerable admin action.

## Consequences

**Positive:**
- Turns Beam's catalog from title-only rows into an actually-populated media library (posters,
  descriptions, ratings, genres, air dates) without the server needing its own metadata-matching
  logic — that logic lives in `cameo`, a maintained dependency rather than bespoke code.
- Enrichment running strictly post-scan, asynchronously, means a large library scan is never slowed
  down by network calls to TMDB/AniList, and a provider outage cannot block indexing.
- The `EnrichmentProvider` trait plus an in-memory fake keeps the enrichment pipeline's tests fully
  offline, consistent with the project's zero-infrastructure testing requirement.
- AniList's keyless availability means enrichment works out of the box for anime-heavy libraries
  without any operator configuration; TMDB is a strict quality improvement for operators who supply a
  key, not a hard requirement.

**Negative / accepted cost:**
- Enrichment is inherently best-effort and asynchronous: newly-scanned titles are briefly visible in
  the catalog with only filename-derived metadata before enrichment catches up, which is a real,
  user-visible (if short-lived) state the UI needs to represent (e.g. a "pending enrichment"
  indicator).
- Scene-filename parsing and provider matching (title/year fuzzy matching against TMDB/AniList
  search results) is inherently heuristic and will occasionally mismatch or fail to match, especially
  for obscure titles, non-English releases, or unusually-formatted filenames — the retry/backoff and
  admin re-enrich action exist specifically because this is expected to happen, not as a
  belt-and-suspenders extra.
- No server-side image proxy means poster/backdrop rendering depends on TMDB/AniList's CDNs being
  reachable from the end user's browser, and leaks the viewing user's IP to those third parties on
  every poster render — an accepted, documented tradeoff rather than an oversight.
