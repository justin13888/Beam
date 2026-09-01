# Data Model

The schema below reflects `beam-entity/src/*.rs` and the migration history in
`beam-migration/src/`. Migrations apply automatically at startup when `BEAM_AUTO_MIGRATE` is set
(the default); the `beam-migration` CLI (`up`/`down`/`status`) is available for operator-managed
migration instead.

Conventions: primary keys are `UUID` (application-generated v4, except where noted), timestamps are
`TIMESTAMPTZ`, and foreign keys cascade on delete unless noted otherwise.

## Identity / session tables

### `users`
The account record. Identity is OIDC-only; no password or other end-user credential is stored. See
[ADR-0003](decisions/ADR-0003-oidc-bff-auth.md).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `oidc_issuer` | TEXT | no | OIDC `iss` claim |
| `oidc_subject` | TEXT | no | OIDC `sub` claim |
| `email` | TEXT | yes | OIDC `email` claim; informational/display only, never identity (and no longer used for admin) |
| `display_name` | TEXT | no | OIDC `name` claim, falling back to `preferred_username`; refreshed on login |
| `avatar_url` | TEXT | yes | OIDC `picture` claim; refreshed on login |
| `is_admin` | BOOLEAN | no | default `false`; recomputed from the configured ID-token claim (`BEAM_OIDC_ADMIN_CLAIM`) at every login — grants and revokes — never trusted as durable state alone; see `security.md` |
| `created_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |

Unique constraint: `(oidc_issuer, oidc_subject)` — the JIT-provisioning lookup key.

### `sessions`
Cookie-backed sessions with hash-at-rest credentials and two-tier expiry. See
[ADR-0005](decisions/ADR-0005-sessions-in-postgres.md).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK; stable internal identifier for listing/revoking a session without re-exposing its credential |
| `user_id` | UUID | no | FK → `users.id`, `ON DELETE CASCADE` |
| `token_hash` | TEXT | no | unique; SHA-256 of the opaque session token — the raw token is never stored, only ever held by the browser in the `beam_session` cookie |
| `device_hash` | TEXT | no | best-effort device fingerprint for the session list |
| `ip` | TEXT | no | best-effort, for user/admin visibility only, not an auth decision input |
| `created_at` | TIMESTAMPTZ | no | |
| `last_active` | TIMESTAMPTZ | no | updated as activity slides the idle expiry forward |
| `idle_expires_at` | TIMESTAMPTZ | no | sliding idle expiry; extended on activity (`BEAM_SESSION_IDLE_DAYS`) |
| `absolute_expires_at` | TIMESTAMPTZ | no | hard ceiling the slide never extends past (`BEAM_SESSION_MAX_DAYS`) |

Indexes: unique on `token_hash` (lookup is always by hash of the presented cookie value); `user_id`
(list/revoke all sessions for a user); `idle_expires_at` (cheap sweep of expired rows).

### `pending_auths`
A single-use OIDC authorization round-trip record, created when the login redirect is issued and
consumed atomically (a `state` value can be exchanged at most once) when the callback arrives.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `state` | TEXT | no | PK; the OIDC `state` value |
| `nonce` | TEXT | no | |
| `pkce_verifier` | TEXT | no | |
| `redirect_path` | TEXT | yes | post-login destination; sanitized to same-origin-relative before storage |
| `created_at` | TIMESTAMPTZ | no | |
| `expires_at` | TIMESTAMPTZ | no | indexed, for sweeping abandoned logins |

## Library / catalog tables

### `libraries`
A configured library root.

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
files represent it. Nullable metadata columns are populated by the enrichment worker post-scan
([ADR-0006](decisions/ADR-0006-cameo-enrichment.md)); NULL until enriched.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `title` | TEXT | no | |
| `title_localized` | TEXT | yes | |
| `description` | TEXT | yes | |
| `year` | INTEGER | yes | |
| `release_date` | DATE | yes | |
| `runtime_mins` | INTEGER | yes | |
| `poster_url` | TEXT | yes | the **provider** URL enrichment found. Never served to a client: the artwork endpoint resolves a title to it, fetches it once and serves the bytes — see [ADR-0015](decisions/ADR-0015-artwork-served-by-beam.md) |
| `backdrop_url` | TEXT | yes | as `poster_url` |
| `tmdb_id` | INTEGER | yes | unique |
| `imdb_id` | TEXT | yes | unique |
| `tvdb_id` | INTEGER | yes | unique |
| `anilist_id` | INTEGER | yes | unique — AniList's numeric media ID |
| `rating_tmdb` | FLOAT | yes | |
| `rating_imdb` | FLOAT | yes | |
| `created_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |

A trigram GIN index on `title` (via the `pg_trgm` extension) backs catalog search; `shows.title`
has the same.

### `shows`
Canonical show/series record, analogous to `movies`: `id` (PK), `title`, `title_localized`,
`description`, `year`, `poster_url`, `backdrop_url`, `tmdb_id`/`imdb_id`/`tvdb_id`/`anilist_id`
(each unique, nullable), `created_at`, `updated_at`.

### `library_movies` / `library_shows`
Many-to-many junctions linking a `libraries` row to the `movies`/`shows` rows discovered within it
(a title can appear across more than one configured library root).

| Table | Columns (composite PK) | FKs |
|---|---|---|
| `library_movies` | `library_id`, `movie_id` | both `ON DELETE CASCADE` |
| `library_shows` | `library_id`, `show_id` | both `ON DELETE CASCADE` |

Each has a secondary index on the non-library side (`movie_id` / `show_id`) for reverse lookups.

### `movie_entries`
A specific *edition* of a movie within a library — e.g. a theatrical cut vs. a director's cut, each
potentially backed by its own file(s).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `library_id` | UUID | no | FK → `libraries.id`, cascade |
| `movie_id` | UUID | no | FK → `movies.id`, cascade |
| `edition` | TEXT | yes | e.g. `"Director's Cut"`; NULL for the default edition |
| `is_primary` | BOOLEAN | no | default `false` |
| `created_at` | TIMESTAMPTZ | no | |

Unique index on `(library_id, movie_id, edition)` — at most one entry per edition label per library,
per movie. Indexes on `library_id` and `movie_id` individually.

### `seasons` / `episodes`
Standard show hierarchy.

`seasons`: `id` (PK), `show_id` (FK → `shows.id`, cascade), `season_number` (INTEGER, not null),
`poster_url` (TEXT, nullable), `first_aired` (DATE, nullable), `last_aired` (DATE, nullable). Unique
index on `(show_id, season_number)`; index on `show_id`.

`episodes`: `id` (PK), `season_id` (FK → `seasons.id`, cascade), `episode_number` (INTEGER, not
null), `title` (TEXT, not null — filled from the scene-filename parser at index time, refined by
enrichment), `description` (TEXT, nullable), `air_date` (DATE, nullable), `runtime_mins` (INTEGER,
nullable), `thumbnail_url` (TEXT, nullable), `created_at` (TIMESTAMPTZ, not null). Unique index on
`(season_id, episode_number)`; index on `season_id`.

## Media-file tables

### `files`
One physical file on disk. This is what makes multi-version delivery (scenario (c) in
`streaming.md`) possible: a single logical title can have many `files` rows, each a distinct
quality/edition/language rip.

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
| `quality` | TEXT | yes | e.g. `"1080p"` — the human label the client's source picker displays |
| `release_group` | TEXT | yes | |
| `is_primary` | BOOLEAN | no | default `false` — which file plays by default for the parent entry/episode |
| `scanned_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |
| `file_status` | ENUM (`file_status`) | no | `known` \| `changed` \| `unknown`; default `known` |
| `mtime` | TIMESTAMPTZ | yes | filesystem mtime; cheap change-detection gate (with `file_size`) before an XXH3 rehash; NULL rows are treated as "suspected changed" |

**CHECK constraint** (table-level): exactly one of `movie_entry_id` / `episode_id` is set — *unless*
`file_status = 'unknown'`, in which case both must be NULL (a file the indexer found but could not
classify). This is the load-bearing polymorphic-association invariant for the media graph.

**Unique index** on `(hash_xxh3, file_path)`: the same content hash can legitimately appear at more
than one path (hardlinks, duplicates), but the *pair* must be unique — this is what the indexer's
dedup logic keys off. Other indexes: `movie_entry_id`, `episode_id`, `library_id`, `hash_xxh3`.

### `media_streams`
One row per elementary stream (video/audio/subtitle track) within a `files` row, populated by
`beam-index`'s ffmpeg-based probing at index time.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `file_id` | UUID | no | FK → `files.id`, cascade |
| `stream_index` | INTEGER | no | container stream index |
| `stream_type` | ENUM (`stream_type`) | no | `video` \| `audio` \| `subtitle` |
| `codec` | TEXT | no | plain probed codec name (e.g. `"h264"`, `"hevc"`, `"aac"`) — never an FFI type; see [ADR-0004](decisions/ADR-0004-never-transcode.md) |
| `language` | TEXT | yes | |
| `title` | TEXT | yes | |
| `is_default` | BOOLEAN | no | default `false` |
| `is_forced` | BOOLEAN | no | default `false` |
| `width` / `height` | INTEGER | yes | video only |
| `frame_rate` | DOUBLE PRECISION | yes | video only |
| `bit_rate` | BIGINT | yes | |
| `color_space` / `color_range` / `hdr_format` | TEXT | yes | video only |
| `channels` | INTEGER | yes | audio only |
| `sample_rate` | INTEGER | yes | audio only |
| `channel_layout` | TEXT | yes | audio only |

Unique index on `(file_id, stream_index)`. Indexes on `file_id`, `stream_type`, `language`.

### `playback_progress`
Resume/continue-watching state, one row per (user, file) the user has started.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `user_id` | UUID | no | FK → `users.id`, cascade |
| `file_id` | UUID | no | FK → `files.id`, cascade |
| `position_secs` | DOUBLE PRECISION | no | last reported playback position |
| `duration_secs` | DOUBLE PRECISION | yes | denormalized snapshot of the file's duration, so percent-complete needs no join |
| `completed` | BOOLEAN | no | default `false`; set once position crosses a near-end threshold, removes the row from continue-watching |
| `updated_at` | TIMESTAMPTZ | no | |

Unique index on `(user_id, file_id)`; index on `(user_id, updated_at)` for the continue-watching
query. Progress is tracked per concrete file, not per abstract title — cross-file progress
carryover is deliberately not attempted.

## Enrichment tables

### `genres` / `movie_genres` / `show_genres`
Populated by the enrichment worker.

`genres`: `id` (PK), `name` (TEXT, unique, not null), `slug` (TEXT, unique, not null).

`movie_genres`: composite PK `(movie_id, genre_id)`, both FKs cascade; index on `genre_id`.

`show_genres`: composite PK `(show_id, genre_id)`, both FKs cascade; index on `genre_id`.

### `metadata_enrichment`
Per-title enrichment queue and status, mirroring `files`' dual-nullable-FK polymorphism (one row per
movie *or* show, never both, never neither).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `movie_id` | UUID | yes | FK → `movies.id`, cascade — polymorphic target 1 |
| `show_id` | UUID | yes | FK → `shows.id`, cascade — polymorphic target 2 |
| `status` | ENUM (`enrichment_status`) | no | `pending` \| `enriched` \| `unmatched` \| `failed`; default `pending` |
| `attempts` | INTEGER | no | default `0`; incremented on each transient-failure retry |
| `next_attempt_at` | TIMESTAMPTZ | yes | backoff scheduling; NULL when not awaiting retry |
| `enriched_at` | TIMESTAMPTZ | yes | set when `status` becomes `enriched` |
| `match_confidence` | REAL | yes | matcher score (0.0–1.0) for the accepted match |
| `matched_ref` | TEXT | yes | canonical `"provider:id"` string, e.g. `"tmdb:603"` |
| `force_refresh` | BOOLEAN | no | default `false`; set by the re-enrich admin action, cleared once processed |
| `last_error` | TEXT | yes | most recent failure/unmatched detail, for admin triage |
| `created_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |

**CHECK constraint:** exactly one of `movie_id` / `show_id` is set. Unique indexes on `movie_id` and
on `show_id` guarantee at most one enrichment row per title — a rescan or refresh updates the
existing row, and multiple files mapping to the same title share one row. Composite index on
`(status, next_attempt_at)` for the worker's due-row poll.

## Admin / log tables

### `admin_logs`
Operational event log surfaced in the admin area.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK, default `gen_random_uuid()` |
| `level` | ENUM (`admin_log_level`) | no | `info` \| `warning` \| `error` |
| `category` | ENUM (`admin_log_category`) | no | `library_scan` \| `system` \| `auth` \| `enrichment` |
| `message` | TEXT | no | |
| `details` | JSONB | yes | |
| `created_at` | TIMESTAMPTZ | no | default `now()` |

Indexes: `created_at DESC` (recent-first admin log view), `level`.

## Invariants

- **Files dual-FK CHECK:** a `files` row has `movie_entry_id` XOR `episode_id` set, unless
  `file_status = 'unknown'`, in which case both are NULL — enforced at the database level, not just
  in application code.
- **Enrichment dual-FK CHECK:** `metadata_enrichment` has `movie_id` XOR `show_id` set, always (no
  "unknown" escape hatch — a queue row is only ever created for a title that already exists).
- **One file per `(hash_xxh3, file_path)`:** the indexer never creates two rows for the same content
  at the same path, while tolerating the same hash at multiple paths.
- **One entry per `(library_id, movie_id, edition)`:** editions are a per-library, per-movie
  namespace.
- **One season per `(show_id, season_number)`, one episode per `(season_id, episode_number)`:**
  prevents duplicate rows on rescans.
- **`users` identity is `(oidc_issuer, oidc_subject)`, not a password:** no end-user credential is
  stored in Postgres at all; `beam-server` never sees or stores a password.
- **`sessions.token_hash` is the only session-lookup key,** and it is a hash — a Postgres dump or
  backup leak does not by itself expose valid sessions.
- **Read-only media filesystem:** not a table-level constraint, but a whole-system invariant that
  bounds what `files.file_path` can ever be used for — read access only, never write. See
  `security.md`.
