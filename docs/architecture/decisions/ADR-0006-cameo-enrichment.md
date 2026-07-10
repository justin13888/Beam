# ADR-0006: cameo-based metadata enrichment via a provider-agnostic trait

## Status

Accepted.

## Context

The indexer previously created movies/shows title-only, parsed from filenames via a bare `SxxEyy`
regex plus a parent-directory-name heuristic. A `MetadataProvider` trait existed in `beam-domain`,
but it was pure dead scaffolding: zero production implementations, never wired into any service.
Every metadata column (`year`, `description`, `poster_url`, ratings, `tmdb_id`, etc.) stayed NULL
forever, and the `genres`/`movie_genres`/`show_genres` tables were never populated, because nothing
ever wrote to them. This left Beam's catalog looking like a flat file listing rather than a real
media library, which was a core product gap, not a cosmetic one.

## Decision

We added a hand-rolled scene-filename parser (title + year extraction, stripping resolution/codec/
release-group noise) replacing the bare `SxxEyy` regex, feeding a background enrichment pipeline that
runs strictly after scan/classify completes (never blocking indexing). The pipeline queries TMDB
(optional API key) and AniList (keyless, always available) through the `cameo` crate — a unified Rust
SDK for both providers, from the same author as Beam — behind a provider-agnostic
`EnrichmentProvider` trait that replaced the dead `MetadataProvider` scaffold. Tests never touch the
network: an in-memory fake `EnrichmentProvider` drives all pipeline tests. Enrichment attempts are
tracked per-title in the `metadata_enrichment` queue/status table with retry/backoff, and the
previously-dead genre tables are populated as a direct result. Poster/backdrop URLs are stored as
direct CDN links — no server-side image proxy, a documented tradeoff (see ADR-0008).
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
- `cameo`'s `cache` feature (a bundled-SQLite response cache) is disabled, discovered while wiring
  the adapter: its `rusqlite` dependency hard-pins `libsqlite3-sys` to a version range that conflicts
  with the one `sea-orm-migration`'s CLI tooling pulls in transitively via `sqlx-sqlite` (always
  compiled in, regardless of Beam's postgres-only `sea-orm` feature selection) — two crates cannot
  link the same native `sqlite3` library at two different versions in one binary. Every enrichment
  request hits TMDB/AniList directly rather than a local cache; the worker's own attempt/backoff
  schedule (`EnrichmentPolicy`) plus cameo's built-in per-provider rate limiting (TMDB 40 req/10s,
  AniList 90/min, with automatic retry) absorb the resulting extra request volume. Revisit if a
  future cameo release decouples the cache feature's sqlite version, or if sea-orm-migration's CLI
  dependency stops requiring every sqlx backend.
