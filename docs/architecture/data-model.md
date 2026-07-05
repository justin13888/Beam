# Data Model

Status: target schema for this push. All tables below reflect `beam-entity`/`beam-migration` as they
will exist once this push's migrations land. Tables and columns marked **(new)** do not exist in the
codebase today; `stream_cache` is marked **(removed)**. Everything else described here matches the
current schema exactly (verified against `beam-entity/src/*.rs` and
`beam-migration/src/m2026*.rs`) and is not itself changing shape — only gaining neighbors.

Conventions used throughout: primary keys are `UUID` (application-generated v4, not
`gen_random_uuid()`, except where noted), timestamps are `TIMESTAMPTZ`, and foreign keys cascade on
delete unless noted otherwise.

## Identity / session tables

### `users`
The account record. **Destructively migrated this push** — `username` and `password_hash` are
dropped in favor of OIDC identity columns, since this is pre-alpha software with no real user data
to preserve. See [ADR-0003](decisions/ADR-0003-oidc-bff-auth.md).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `oidc_issuer` | TEXT | no | **(new)** OIDC `iss` claim |
| `oidc_subject` | TEXT | no | **(new)** OIDC `sub` claim |
| `email` | TEXT | yes | **(new)** not all IdPs release an email claim; not unique — the same email can legitimately appear under more than one issuer |
| `display_name` | TEXT | no | **(new)** OIDC `name` claim, falling back to `preferred_username` |
| `avatar_url` | TEXT | yes | **(new)** OIDC `picture` claim |
| `is_admin` | BOOLEAN | no | default `false`; recomputed from the admin email allowlist at every login, never trusted as durable state alone — see `security.md` |
| `created_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |

*Removed columns:* `username` (was unique TEXT), `password_hash` (was TEXT).

Unique constraint: `(oidc_issuer, oidc_subject)` — this is the JIT-provisioning lookup key.
Deliberately **no** unique constraint on `email`: it is nullable (absent from some IdPs' claims) and
the same email can appear under more than one issuer, so it drives admin-allowlist matching only, not
identity.

### `sessions` **(new table)**
Replaces Redis/Valkey-backed sessions entirely. See
[ADR-0005](decisions/ADR-0005-sessions-in-postgres.md).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `user_id` | UUID | no | FK → `users.id`, `ON DELETE CASCADE` |
| `token_hash` | TEXT | no | unique; SHA-256 of the opaque session token — the raw token is never stored, only ever held by the browser in the session cookie |
| `created_at` | TIMESTAMPTZ | no | |
| `last_seen_at` | TIMESTAMPTZ | no | updated on each authenticated request, up to a coalescing granularity (e.g. once/minute) to avoid a write per request; drives sliding-TTL expiry |
| `expires_at` | TIMESTAMPTZ | no | absolute expiry, extended on activity up to a maximum session lifetime |
| `ip_address` | INET | yes | best-effort, for admin visibility/audit only, not an auth decision input |
| `user_agent` | TEXT | yes | best-effort, admin visibility only |

Indexes: unique on `token_hash` (session lookup is by hash of the presented cookie value, never by
raw token); index on `user_id` (list/revoke all sessions for a user); index on `expires_at` (cheap
sweep of expired rows).

## Library / catalog tables

### `libraries`
A configured library root. Unchanged this push.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `name` | TEXT | no | |
| `description` | TEXT | yes | |
| `root_path` | TEXT | no | unique |
| `created_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |
| `last_scan_started_at` | TIMESTAMPTZ | yes | |
| `last_scan_finished_at` | TIMESTAMPTZ | yes | |
| `last_scan_file_count` | INTEGER | yes | |

### `movies`
Canonical movie title record — one row per distinct film, independent of how many library entries or
files represent it.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `title` | TEXT | no | |
| `title_localized` | TEXT | yes | |
| `description` | TEXT | yes | populated by enrichment; NULL until enriched |
| `year` | INTEGER | yes | populated by enrichment |
| `release_date` | DATE | yes | populated by enrichment |
| `runtime_mins` | INTEGER | yes | populated by enrichment |
| `poster_url` | TEXT | yes | direct CDN URL (TMDB/AniList), not proxied — see [ADR-0006](decisions/ADR-0006-cameo-enrichment.md) |
| `backdrop_url` | TEXT | yes | direct CDN URL |
| `tmdb_id` | INTEGER | yes | unique; indexed |
| `imdb_id` | TEXT | yes | unique; indexed |
| `tvdb_id` | INTEGER | yes | unique |
| `anilist_id` | INTEGER | yes | **(new)** unique; indexed — AniList's numeric media ID |
| `rating_tmdb` | FLOAT | yes | |
| `rating_imdb` | FLOAT | yes | |
| `created_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |

**Changed from today:** all nullable metadata columns above (`description`, `year`, `release_date`,
`runtime_mins`, `poster_url`, `backdrop_url`, ratings, and the various external IDs) exist in the
schema today but are always NULL in practice, because nothing populates them — the indexer creates
title-only rows and the `MetadataProvider` trait is dead scaffolding. This push wires the enrichment
pipeline so these columns are actually populated post-scan.

### `shows`
Canonical show/series record, analogous to `movies`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `title` | TEXT | no | |
| `title_localized` | TEXT | yes | |
| `description` | TEXT | yes | |
| `year` | INTEGER | yes | |
| `poster_url` | TEXT | yes | |
| `backdrop_url` | TEXT | yes | |
| `tmdb_id` | INTEGER | yes | unique; indexed |
| `imdb_id` | TEXT | yes | unique; indexed |
| `tvdb_id` | INTEGER | yes | unique |
| `anilist_id` | INTEGER | yes | **(new)** unique; indexed |
| `created_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |

### `library_movies` / `library_shows`
Many-to-many junctions linking a `libraries` row to the `movies`/`shows` rows discovered within it
(a title can in principle appear across more than one configured library root).

| Table | Columns (composite PK) | FKs |
|---|---|---|
| `library_movies` | `library_id`, `movie_id` | both `ON DELETE CASCADE` |
| `library_shows` | `library_id`, `show_id` | both `ON DELETE CASCADE` |

Each has a secondary index on the non-library side (`movie_id` / `show_id`) for reverse lookups.

### `movie_entries`
A specific *edition* of a movie within a library — e.g. a theatrical cut vs. a director's cut of the
same film, each potentially backed by its own file(s).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `library_id` | UUID | no | FK → `libraries.id`, cascade |
| `movie_id` | UUID | no | FK → `movies.id`, cascade |
| `edition` | TEXT | yes | e.g. `"Director's Cut"`; NULL for the default edition |
| `is_primary` | BOOLEAN | no | default `false` |
| `created_at` | TIMESTAMPTZ | no | |

Unique index on `(library_id, movie_id, edition)` — encodes "at most one entry per edition label per
library, per movie." Indexes on `library_id` and `movie_id` individually for lookups.

### `seasons` / `episodes`
Standard show hierarchy: a show has seasons, a season has episodes.

`seasons`: `id` (PK), `show_id` (FK → `shows.id`, cascade), `season_number` (INTEGER, not null),
`poster_url` (TEXT, nullable), `first_aired` (DATE, nullable), `last_aired` (DATE, nullable). Unique
index on `(show_id, season_number)`; index on `show_id`.

`episodes`: `id` (PK), `season_id` (FK → `seasons.id`, cascade), `episode_number` (INTEGER, not
null), `title` (TEXT, not null — filled from the scene-filename parser at index time, refined by
enrichment later), `description` (TEXT, nullable), `air_date` (DATE, nullable), `runtime_mins`
(INTEGER, nullable), `thumbnail_url` (TEXT, nullable), `created_at` (TIMESTAMPTZ, not null). Unique
index on `(season_id, episode_number)`; index on `season_id`.

## Media-file tables

### `files`
One physical file on disk. This is the table that makes multi-version delivery (delivery scenario
(c) — see `streaming.md`) possible: a single logical title can have many `files` rows, each a
distinct quality/edition/language rip.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `movie_entry_id` | UUID | yes | FK → `movie_entries.id`, cascade — polymorphic target 1 |
| `episode_id` | UUID | yes | FK → `episodes.id`, cascade — polymorphic target 2 |
| `library_id` | UUID | no | FK → `libraries.id`, cascade |
| `file_path` | TEXT | no | absolute path under the library root |
| `file_size` | BIGINT | no | |
| `mime_type` | TEXT | yes | |
| `hash_xxh3` | BIGINT | no | content hash used for change detection and dedup |
| `duration_secs` | DOUBLE PRECISION | yes | |
| `container_format` | TEXT | yes | |
| `language` | TEXT | yes | primary audio/release language tag |
| `quality` | TEXT | yes | e.g. `"1080p"`, `"480p"` — the human label the client's source-quality picker displays |
| `release_group` | TEXT | yes | |
| `is_primary` | BOOLEAN | no | default `false` — which file plays by default for the parent entry/episode |
| `scanned_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |
| `file_status` | ENUM (`file_status`) | no | `known` \| `changed` \| `unknown`; default `known` |
| `mtime` | TIMESTAMPTZ | yes | filesystem mtime; cheap change-detection gate (with `file_size`) before an XXH3 rehash; NULL on rows scanned before this column existed, treated as "suspected changed" |

**CHECK constraint** (`idx`-less, table-level): exactly one of `movie_entry_id` / `episode_id` is set
— *unless* `file_status = 'unknown'`, in which case both must be NULL (a file the indexer found but
could not classify at all). This is the load-bearing polymorphic-association invariant for the whole
media graph; see "Invariants" below.

**Unique index** on `(hash_xxh3, file_path)` — the same content hash can legitimately appear at more
than one path (e.g. a hardlinked or duplicated file), and the same path is obviously unique per hash,
but the *pair* must be unique: this is what the indexer's dedup logic keys off.

Other indexes: `movie_entry_id`, `episode_id`, `library_id`, `hash_xxh3` (each individually, for
lookups and joins).

### `media_streams`
One row per elementary stream (video/audio/subtitle track) within a `files` row, populated from
`beam-index`'s ffmpeg-based probing at index time.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `file_id` | UUID | no | FK → `files.id`, cascade |
| `stream_index` | INTEGER | no | container stream index |
| `stream_type` | ENUM (`stream_type`) | no | `video` \| `audio` \| `subtitle` |
| `codec` | TEXT | no | plain codec name string (e.g. `"h264"`, `"aac"`) — never an FFI type; see [ADR-0004](decisions/ADR-0004-never-transcode.md) |
| `language` | TEXT | yes | |
| `title` | TEXT | yes | |
| `is_default` | BOOLEAN | no | default `false` |
| `is_forced` | BOOLEAN | no | default `false` |
| `width` | INTEGER | yes | video only |
| `height` | INTEGER | yes | video only |
| `frame_rate` | DOUBLE PRECISION | yes | video only |
| `bit_rate` | BIGINT | yes | |
| `color_space` | TEXT | yes | video only |
| `color_range` | TEXT | yes | video only |
| `hdr_format` | TEXT | yes | video only |
| `channels` | INTEGER | yes | audio only |
| `sample_rate` | INTEGER | yes | audio only |
| `channel_layout` | TEXT | yes | audio only |

Unique index on `(file_id, stream_index)`. Indexes on `file_id`, `stream_type`, `language`.

### `playback_progress` **(new table)**
Resume/continue-watching state, one row per (user, file) the user has started.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `user_id` | UUID | no | FK → `users.id`, cascade |
| `file_id` | UUID | no | FK → `files.id`, cascade |
| `position_secs` | DOUBLE PRECISION | no | last reported playback position |
| `duration_secs` | DOUBLE PRECISION | yes | denormalized snapshot of the file's duration at time of last update, so "percent complete" can be computed without a join for the continue-watching row |
| `completed` | BOOLEAN | no | default `false`; set once position crosses a near-end threshold, drives removal from continue-watching |
| `updated_at` | TIMESTAMPTZ | no | |

Unique index on `(user_id, file_id)` — one progress row per user per file; a user resuming a
different quality/edition of the same title tracks progress per concrete file, not per abstract
title (deliberately simple for this push; cross-file progress carryover is not attempted).

## Enrichment tables

### `genres` / `movie_genres` / `show_genres`
Schema unchanged from today — these tables exist already but are populated by nothing, since no
enrichment pipeline runs. This push wires the `cameo`-backed enrichment worker to actually populate
them.

`genres`: `id` (PK), `name` (TEXT, unique, not null), `slug` (TEXT, unique, not null).

`movie_genres`: composite PK `(movie_id, genre_id)`, both FKs cascade; index on `genre_id`.

`show_genres`: composite PK `(show_id, genre_id)`, both FKs cascade; index on `genre_id`.

### `metadata_enrichment` **(new table)**
Per-title enrichment queue and status, mirroring the `files` table's dual-nullable-FK polymorphism
pattern (one enrichment row per movie *or* show, never both, never neither).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `movie_id` | UUID | yes | FK → `movies.id`, cascade — polymorphic target 1 |
| `show_id` | UUID | yes | FK → `shows.id`, cascade — polymorphic target 2 |
| `status` | ENUM (`enrichment_status`) | no | `pending` \| `enriched` \| `unmatched` \| `failed`; default `pending` |
| `attempts` | INTEGER | no | default `0`; incremented on each transient-failure retry |
| `next_attempt_at` | TIMESTAMPTZ | yes | backoff scheduling; NULL when not awaiting retry |
| `enriched_at` | TIMESTAMPTZ | yes | set when `status` becomes `enriched` |
| `match_confidence` | REAL | yes | matcher score (0.0–1.0) for the accepted match; NULL until matched |
| `matched_ref` | TEXT | yes | canonical `"provider:id"` string, e.g. `"tmdb:603"`; NULL until matched |
| `force_refresh` | BOOLEAN | no | default `false`; set by the re-enrich admin action, cleared once processed |
| `last_error` | TEXT | yes | most recent failure/unmatched-reason detail, for admin triage |
| `created_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |

**CHECK constraint:** exactly one of `movie_id` / `show_id` is set — same shape as the `files` table
invariant, intentionally, for consistency across the schema's polymorphic-association tables.

Unique index on `movie_id` and a separate unique index on `show_id` (both partial over non-NULL
values in effect, since only one is ever set per row) — together with the CHECK, this guarantees at
most one enrichment row per title; a rescan or refresh updates the existing row rather than creating
a duplicate, and multiple files mapping to the same movie/show share one row. Composite index on
`(status, next_attempt_at)` for the worker's due-row poll.

## Admin / log tables

### `admin_logs`
Operational event log surfaced in the admin area. Unchanged this push.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK, default `gen_random_uuid()` |
| `level` | ENUM (`admin_log_level`) | no | `info` \| `warning` \| `error` |
| `category` | ENUM (`admin_log_category`) | no | `library_scan` \| `system` \| `auth` |
| `message` | TEXT | no | |
| `details` | JSONB | yes | |
| `created_at` | TIMESTAMPTZ | no | default `now()` |

Indexes: `created_at DESC` (recent-first admin log view), `level`.

**Changed from today:** `category`'s enum will gain an `enrichment` value this push so enrichment
worker events (retries, permanent failures) are distinguishable from `library_scan` events in the
admin log view.

## Removed: `stream_cache`

The `stream_cache` table (`id`, `file_id` → `files.id` cascade, `target_codec`, `target_container`,
`target_resolution`, `target_bitrate`, `hls_playlist_path`, `cache_path`, `created_at`) is **dropped**
by migration this push. It existed to back an HLS/fMP4 remux-on-request cache that was never fully
wired up — nothing in the current codebase writes to it. Since Beam never transcodes or remuxes at
request time in the target architecture, there is no server-side derived-artifact cache to track. See
[ADR-0004](decisions/ADR-0004-never-transcode.md).

## Invariants

- **Files dual-FK CHECK:** a `files` row has `movie_entry_id` XOR `episode_id` set, unless
  `file_status = 'unknown'`, in which case both are NULL. This is what lets one table serve both
  movie files and episode files without a separate table per media type, and it is enforced at the
  database level, not just in application code.
- **Enrichment dual-FK CHECK:** `metadata_enrichment` has `movie_id` XOR `show_id` set, always (no
  "unknown" escape hatch here — a queue row is only ever created for a title that already exists).
- **One file per `(hash_xxh3, file_path)`:** encodes "the indexer will not create two rows for the
  same content at the same path," while still tolerating the same content hash legitimately
  appearing at multiple paths (hardlinks, intentional duplicates across libraries).
- **One entry per `(library_id, movie_id, edition)`:** encodes "editions are a per-library,
  per-movie namespace" — the same edition label can exist in two different libraries without
  conflict, but not twice in one.
- **One season per `(show_id, season_number)`, one episode per `(season_id, episode_number)`:**
  standard hierarchical uniqueness; prevents the indexer from creating duplicate season/episode rows
  on rescans.
- **`users` identity is `(oidc_issuer, oidc_subject)`, not a password:** there is no credential
  stored in Postgres for end users at all in the target state; `beam-server` never sees or stores a
  password.
- **`sessions.token_hash` is the only session-lookup key,** and it is a hash — the plaintext session
  token that the browser holds in its cookie is never persisted, so a Postgres dump or backup leak
  does not by itself expose valid sessions.
- **Read-only media filesystem:** not a table-level constraint, but a whole-system invariant worth
  repeating here since it bounds what the `files.file_path` column can ever be used for by
  `beam-server` — read access only, never write. See `security.md`.
