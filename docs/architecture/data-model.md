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
| `token_hash` | TEXT | no | unique; SHA-256 of the opaque session token — the raw token is never stored, only ever held by the client as the `beam_session` cookie |
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

### `device_auths`
An in-flight OAuth 2.0 device authorization grant (FR-111,
[ADR-0017](decisions/ADR-0017-device-authorization-grant.md)): created by `POST /v1/auth/device`,
polled through `POST /v1/auth/device/token`, and deleted when the flow ends (approved, denied, or
expired). Expired rows are swept whenever a new flow starts.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `handle_hash` | TEXT | no | PK; SHA-256 of the opaque device handle the client polls with — the handle itself is never stored |
| `device_code` | TEXT | no | the IdP's device code; server-side only, never sent to the client |
| `user_code` | TEXT | no | the code the user enters at the IdP |
| `verification_uri` | TEXT | no | |
| `verification_uri_complete` | TEXT | yes | the verification URI with the code filled in, when the IdP offers one |
| `interval_secs` | INTEGER | no | minimum seconds between polls; grows by 5 on every `slow_down` |
| `next_poll_at` | TIMESTAMPTZ | no | earliest instant the next poll may reach the IdP; moved by a conditional `UPDATE`, so concurrent polls claim a flow once |
| `created_at` | TIMESTAMPTZ | no | |
| `expires_at` | TIMESTAMPTZ | no | indexed, for the sweep; at most 30 minutes after creation |

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
| `title` | TEXT | no | the **display** title: the filename parse until enrichment replaces it with the provider's. Never used to find the movie |
| `identity_key` | TEXT | yes | unique — what the indexer matches a file to this movie by; see *Title identity* below. NULL only on a row that predates the column and could not be backfilled |
| `identity_key_version` | SMALLINT | no | default `0`: the version of the classification rules (`beam_domain::utils::media_path::CLASSIFIER_VERSION`) that derived `identity_key`. A key an older version derived is re-derived from the title's files (*Rekey* below); `0` marks keys stored before versions existed |
| `pinned_ref` | TEXT | yes | unique — the provider id an NFO beside the media pins the movie to, as `"provider:id"` (`tmdb:603`, `imdb:tt0133093`; `beam_domain::models::pin::ProviderPin`). A file whose NFO names it joins this movie before any key is consulted, and enrichment fetches the movie by it (issue #184). Never written by enrichment; NULL when the movie is not pinned |
| `pin_source` | TEXT | yes | who set `pinned_ref`: `nfo` or `admin` (a `CHECK` holds it to these, and to being NULL exactly when `pinned_ref` is). An NFO never replaces an `admin` pin (FR-312) |
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
has the same. `idx_movies_title_sort` on `(lower(title), id)` serves the default browse order and
`idx_movies_added_sort` on `(created_at, id)` the `date_added` one: each branch of the catalogue
query orders and seeks on exactly those columns, so it reads the index in order. Unfiltered or
filtered only by kind, it stops at the page size; with a selective genre, search or minimum-rating
filter it may read up to the whole index before the page fills, so that page is bounded by the
catalogue rather than the page size. `idx_shows_title_sort` and `idx_shows_added_sort` are the same
on `shows`. Year, rating and runtime sorts have no index.

### `shows`
Canonical show/series record, analogous to `movies`: `id` (PK), `title`, `identity_key` (unique,
nullable — as for movies), `identity_key_version` (as for movies), `pinned_ref` (unique, nullable —
as for movies, pinned by a `tvshow.nfo`), `pin_source` (as for movies), `title_localized`, `description`, `year`, `poster_url`,
`backdrop_url`, `tmdb_id`/`imdb_id`/`tvdb_id`/`anilist_id` (each unique, nullable),
`rating_tmdb` (REAL, nullable — the provider rating on the same 0-10 scale as
`movies.rating_tmdb`; added by `m20261003_000001_catalogue_browse` and filled on a show's next
enrichment), `created_at`, `updated_at`.

### Title identity and lifetime

A movie or show has two names (FR-214, FR-215;
[#183](https://github.com/justin13888/beam/issues/183)). `title` is what users see, and enrichment
overwrites it with the provider's spelling. `identity_key` is what the indexer finds the title by,
and nothing but the indexer's own backfill and rekey (below) ever writes it after insert: enrichment's `UPDATE` does
not name the column. It is `beam_domain::utils::identity::title_identity_key` of the filename
parse — NFKD-decomposed, every combining mark in the Combining Diacritical Marks block
(U+0300–U+036F: Latin, Greek and Cyrillic accents alike) dropped, apostrophes (`'` and `’`) elided
so a scene name's `Greys` is the folder's `Grey's`, other punctuation dropped, lowercased,
`&` read as `and`, and recomposed (NFC) — followed by `|` and the parsed year (empty when there is
none). Every combining mark outside that block is kept: a kana voicing mark or an Indic vowel sign
is part of its letter, so `かぎ` and `かき`, or `दिल` and `दल`, stay two titles. The fold is by block,
not by language, so it also merges letters some languages treat as distinct — Cyrillic `й`/`и`,
`ї`/`і`, `ў`/`у`, Latin `ñ`/`n`, `ä`/`a` — and `Мой` and `Мои` of one year are one title. That is
the accepted cost of `Amélie` and `Amelie` being one (decision D183-6 on
[#214](https://github.com/justin13888/beam/pull/214)). A movie is keyed by its filename (with its folder's year, or its
folder's title for a noise-only name), a show by its series folder: the parent of a season folder
(or the season folder's own leading text, for a season pack; a multi-season pack's range of
seasons ends the title it names, and a folder that is only a range is a pack inside the series
folder above it), else the episode file's parent folder
unless the filename names another show, else the filename
(`beam_domain::utils::media_path::infer_media`, FR-204). Builds before
[#182](https://github.com/justin13888/beam/issues/182) took the immediate parent, so a
`Show/Season 01/` layout keyed a show as `season 01|`; the first scan under the current rules
reclassifies those files onto the correctly keyed show (see `classifier_version` under `files`) and
the emptied husk is retired below. The identity backfill never keys such a husk — a show every one
of whose files the old parent-folder rule names after a season folder — since holding its files'
key, the husk would capture the series' files instead. A show with no file row is keyed from its
stored title like any fileless title, and retired as one. A husk is recognised by its files' paths
or its stored key, never by the display title, which enrichment may have replaced. The year is part of the key, so a remake is a separate title.

**Find-or-create** is one `INSERT ... ON CONFLICT (identity_key) DO NOTHING` followed by a read by
key, against the unique index `idx_movies_identity_key` / `idx_shows_identity_key`. There is no
lookup by display title, and two files of one new title indexed at once — a scan and the watcher —
resolve to one row rather than racing a SELECT against an INSERT. A NULL key is never matched.

**Backfill.** Rows that predate the column have a NULL key, and it cannot be computed in SQL: their
`title` may already be the provider's. On the first `scan_all_libraries` in a process the indexer
derives each such title's key from the paths of all its file rows — soft-deleted ones included, so a
title whose files are away at upgrade is still keyed by them — with the same function classification
uses. Every file agreeing sets it; only a title with no file row that parses as its kind (a movie
filename for a movie, an episode path for a show) takes the key of its stored title and year. Titles are keyed oldest first (`created_at`, then `id`), so of two duplicates the
display-title lookup created, the original takes the key. Files that disagree (two films once merged
under one title) or a key another row already holds (the later duplicate) leave the key NULL and are
named in an admin-log warning; such a row stays listed but is never matched again. A backfill that
fails is logged, reported in the admin log and retried, and holds reclassification (below); the
scan itself goes ahead.

**Rekey.** A change to the path inference or the title fold changes the key a title's files
derive: #183 keyed `Grey's Anatomy` as `grey s anatomy|`, and the current fold keys its files
`greys anatomy|`. Left alone, the next file would create a second title beside the enriched one.
So each key carries `identity_key_version`, and right after the backfill the indexer re-derives
every key older than `CLASSIFIER_VERSION` (`LocalIndexService::rekey_stale_titles`), before any
file is reclassified. The two passes run once per process, under one lock
(`LocalIndexService::identity_passes_done`), asked first by every path that reclassifies:
`scan_all_libraries`, the administrator's `scan_library`, and a watcher event for a known file
that awaits reclassification (probed, and classified by an older version); an event for any other
file does not ask, so a failing pass is not retried on every event. A caller arriving while they
run waits for them. Until both have succeeded, a file row an older
version classified is not reclassified — it keeps its title and its version — while new and changed
files are indexed as usual; a failed pass is logged, reported in an admin-log warning, and retried
by the next caller. Reclassifying before the rekey would find no title by the file's new key,
create one, and leave the enriched title with no file for orphan cleanup to retire. The new key is
the one the title's present files derive, by the backfill's derivation; with no present file, the
one all its file rows derive. A free key is written in place (`rekey`), so the title keeps its id,
enrichment, genres, provider ids and manual match. A key another title holds means the current
rules read the two as one title: the one with provider ids survives (else the older), takes the
key, and receives the other's files — its entries found or created on the survivor per library and
edition, its episodes per season and number, so both shows' files of one episode become sources
of one episode — and the other, now keyless and fileless, is deleted by the scan's orphan cleanup.
A show whose stored key's title part is a season-folder name (`season 05|`) is released (key set to
NULL) instead, as the
backfill leaves one keyless. A title whose files derive no key of its kind keeps its key and
version and is looked at again on the next start; one whose files derive several keeps its key and
is named in an admin-log warning. Rekeys and merges are listed in an admin-log entry.

**Live titles.** A title is *live* while at least one file behind it is present
(`missing_since IS NULL`): for a movie, through `movie_entries`; for a show, through `seasons` and
`episodes`. The `CatalogRepository` — browse and search — lists only live titles, with the check
an `EXISTS` inside each branch of its one statement. Detail reads by id do not filter, so
a bookmark or continue-watching tile still resolves while its file is away.

**Retirement.** A scan whose walk read the whole tree finishes by deleting orphans:
`movie_entries` (and `episodes`) no `files` row references, then `seasons` with no episodes, then
`movies` and `shows` with no child left. Entries, episodes, movies and shows go only if created
before the scan started, which protects a title the watcher is creating concurrently. It does not
protect a title that was already orphaned when the scan started: one the watcher is attaching a new
file to can still be deleted under it, and that file drops out of the index until the next scan
re-indexes it. `seasons` has
no `created_at`, so an empty season goes whenever the scan finds it — including one the watcher has
just created and not yet given its first episode. That episode's insert then fails on the missing
season, the watcher logs a warning (not an admin-log entry) for that file, and the next scan
indexes it. A soft-deleted file row still counts, so a title goes only once its last file is
purged. `library_movies` / `library_shows`, `metadata_enrichment` and the genre links go by
`ON DELETE CASCADE`. The scan's completion entry counts them as `titles_removed`.

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

Unique index `idx_movie_entries_unique` on `(library_id, movie_id, edition)` `NULLS NOT DISTINCT` —
at most one entry per edition per library, per movie, the default (NULL) edition included. Every
copy of one edition is another `files` row of its one entry. The indexer finds or creates an entry
with one `INSERT ... ON CONFLICT (library_id, movie_id, edition) DO NOTHING` and a read-back
(`MovieRepository::find_or_create_entry`). Before
[#182](https://github.com/justin13888/beam/issues/182) the index let any number of NULL-edition
entries coexist and the indexer created one per file; migration `m20260929_000001_classifier_v2`
merged those into the oldest of each group, repointing their files. Indexes on `library_id` and
`movie_id` individually.

### `seasons` / `episodes`
Standard show hierarchy.

`seasons`: `id` (PK), `show_id` (FK → `shows.id`, cascade), `season_number` (INTEGER, not null),
`poster_url` (TEXT, nullable), `first_aired` (DATE, nullable), `last_aired` (DATE, nullable). Unique
index on `(show_id, season_number)`; index on `show_id`.

`episodes`: `id` (PK), `season_id` (FK → `seasons.id`, cascade), `episode_number` (INTEGER, not
null), `title` (TEXT, not null — the text after the episode marker at index time, else
`Episode N`; refined by enrichment), `description` (TEXT, nullable), `air_date` (DATE, nullable —
set at index time for a date-based episode), `runtime_mins` (INTEGER,
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
| `duration_secs` | DOUBLE PRECISION | yes | NULL until a probe succeeds, and cleared (with `mime_type`, `container_format`, `container_tags` and the file's `media_streams`) when changed content fails its probe; the indexer probes a NULL row again on every visit |
| `container_format` | TEXT | yes | |
| `language` | TEXT | yes | primary audio/release language tag |
| `quality` | TEXT | yes | e.g. `"1080p"` — the human label the client's source picker displays |
| `release_group` | TEXT | yes | |
| `is_primary` | BOOLEAN | no | default `false`, but the indexer writes `true` for every file it creates (`SqlFileRepository::create`), and nothing reads it. It does **not** select which of a movie's or episode's files plays by default: `/v1/media/{id}/sources` returns them in no particular order ([#142](https://github.com/justin13888/beam/issues/142)) |
| `scanned_at` | TIMESTAMPTZ | no | |
| `updated_at` | TIMESTAMPTZ | no | |
| `file_status` | ENUM (`file_status`) | no | `known` \| `changed` \| `unknown`; default `known` |
| `mtime` | TIMESTAMPTZ | yes | filesystem mtime; cheap change-detection gate (with `file_size`) before an XXH3 rehash. The column keeps whole microseconds and a filesystem reports nanoseconds, so the indexer brings a file's mtime to the stored precision (`beam_domain::models::file::mtime_as_stored`) before comparing it with its row ([#229](https://github.com/justin13888/beam/issues/229)); NULL rows are treated as "suspected changed" |
| `missing_since` | TIMESTAMPTZ | yes | soft-delete stamp: NULL while the file is on disk; the instant the indexer first found it gone otherwise (FR-211) |
| `last_episode_number` | INTEGER | yes | the last episode of a multi-episode file (`S01E01E02`); the file's `episode_id` is its first. A `CHECK` (`files_last_episode_requires_episode`) allows it only alongside `episode_id` |
| `classifier_version` | SMALLINT | no | default `0`: the version of the classification rules (`beam_domain::utils::media_path::CLASSIFIER_VERSION`) that decided `movie_entry_id`/`episode_id`. A scan reclassifies a probed row with an older version from its path, the NFOs beside it and its `container_tags`, keeping its id, hash and probe results; `0` marks rows classified before versions existed and rows never probed |
| `container_tags` | JSONB | yes | the file-level container tags classification reads (`title`, `show`, `season`, `episode`, `year`; `beam_domain::utils::classification::ContainerTags`) as the last successful probe read them, `title` and `show` each cut to at most 512 bytes (`MAX_TAG_VALUE_BYTES`) on a character boundary. Set with the other probe results, cleared with them when changed content fails its probe; NULL while the file has no successful probe, or had it before this column existed. A `CHECK` (`files_container_tags_object`) holds it to a JSON object. A reclassification reads it instead of probing again, so a file placed by its tags keeps its place (FR-219) |

**CHECK constraint** (table-level): exactly one of `movie_entry_id` / `episode_id` is set — *unless*
`file_status = 'unknown'`, in which case both must be NULL (a file the indexer found but could not
classify). This is the load-bearing polymorphic-association invariant for the media graph.

**Unique index** on `file_path` (`idx_files_path_unique`): one row per path, whatever its hash. The
same content hash can legitimately appear at more than one path (hardlinks, duplicates) — that is
what duplicate detection reads — but a path is one file. Before issue #181 the only uniqueness was
`(hash_xxh3, file_path)`, and two tasks reconciling one library at once could each insert a row for
one path. Migration `m20261001_000001_files_unique_path` merged the rows a path already had,
keeping — in order — a present row before a missing one, a `known` row before any other status, the
row with the most playback progress, the most recently updated, then the lowest id; each user's
most recently updated progress on the path moved to the kept row, and the other rows were deleted
with their streams. `down()` restores the old index but not the merged rows. Other indexes:
`movie_entry_id`, `episode_id`, `library_id`, `hash_xxh3`.

**Soft delete** (FR-211): the indexer never deletes a `files` row on first sight. A file the scan's
walk or the watcher finds gone is stamped `missing_since`; a row with a stamp is *missing*. Every
read outside the indexer — browse, search, detail sources, streaming, continue-watching, the admin
file count — goes through `FileRepository`'s *visible* reads, which filter `missing_since IS NULL`.
The indexer's *reconcile* reads (`find_by_path`, `find_all_by_library_including_missing`) see
missing rows, so a path that comes back clears the stamp on the same row and keeps its id. The stamp
is written once: marking an already-missing row keeps the first instant, because the grace period
runs from when the file was first found gone. `purge_missing` is the only delete, it removes only
rows that are already missing, and a scan calls it only for rows its walk could vouch for (no walk
error above them, and not refused by the empty-root guard) once they have been missing for
`BEAM_MISSING_FILE_GRACE_DAYS`. The `ON DELETE CASCADE` foreign keys from `media_streams` and
`playback_progress` therefore fire only on that purge — a transient absence no longer takes every
user's resume point with it. A *title* whose every file is missing is hidden from browse and search
by the liveness check above, and retired once its last file is purged.

**Relink** (FR-221): a row follows its file's content within its library — through a move or a
rename, two files swapping names, a rotation, or a rename onto the path of a row already missing. A
path whose content hash (never the unhashed `0`) and `file_size` match a row of the same library
whose own content has left its `file_path` is that row. `FileRepository::relink` takes every such
move of one scan at once, in one transaction: it rewrites `file_path`, `file_size` and `mtime` and
clears `missing_since`, so `id`, and with it `playback_progress`, `movie_entry_id`/`episode_id` and
`media_streams`, is kept, and the file is not probed or classified again. Because
`idx_files_path_unique` is checked per statement, each relinked row first steps aside to
`<old path>.beam-relinking-<id>` and only then takes its new path, so rows can trade paths and the
index holds at the end; a path held by a row outside the call still fails the index and rolls the
whole call back. A row whose path a relink takes and whose content is nowhere is *displaced*: it
keeps its id and is stamped missing (a first stamp is kept) at `<old path>.beam-displaced-<id>` —
`displaced_path` — a name no scan indexes, having no video extension, and no other row can hold. It
can still be relinked by content, and is purged after the grace period like any missing row; a move
of such a row is reported from the path it was displaced from (`displaced_from`, the inverse), since
the displaced name never existed on disk. The
watcher's candidates come from `find_by_library_and_hash_including_missing`, a reconcile read
served by `idx_files_hash` and scoped to one library: a movie entry belongs to its library, so a
file moved to another library is that library's new file. A row whose file is still at its path is
never a candidate — the new path is a copy, and gets a row of its own. Ties between identical
copies are broken by `PlaybackProgressRepository::last_played_at`, the latest `updated_at` of a
file's `playback_progress` rows. The same read declines a replace-by-rename: a relink that would
displace a played row for a row never played is not made, so the path keeps its row, with its new
content read as a change, and the other row is paired elsewhere or marked missing. The rows beneath a directory a watcher event names come from
`find_beneath_including_missing`: one `LIKE` on `file_path` scoped to the library, with the
directory's own `\`, `%` and `_` escaped and a trailing separator, so `S1` never matches `S10`.

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

### `sidecar_subtitles`
A text subtitle file beside an indexed video, recorded as a subtitle of that video (issue #184). A
table of its own rather than `media_streams` rows (decision D184-1 on PR #225): `stream_index` is
the container's own index, unique per file and replaced wholesale when a changed file is re-probed,
and every current reader of `media_streams` describes what is inside the container. Written by the
scan and the watcher from what a subtitle's filename says; the file itself is only ever stat-ed.
Not yet read by any endpoint — serving sidecar subtitles is issue #189's.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `file_id` | UUID | no | FK → `files.id`, cascade — the video the subtitle belongs to |
| `library_id` | UUID | no | FK → `libraries.id`, cascade |
| `path` | TEXT | no | unique (`sidecar_subtitles_path_unique`) — the subtitle file; upserted by it |
| `format` | TEXT | no | `srt` \| `vtt` \| `ass` \| `ssa` (a `CHECK`); image formats are not indexed (ADR-0004) |
| `language` | TEXT | yes | ISO 639-2/B, as FFmpeg writes a Matroska track's language |
| `title` | TEXT | yes | what else the name carries (`Commentary`, a BCP 47 region such as `pt-BR`) |
| `is_forced` / `is_sdh` / `is_default` | BOOLEAN | no | default `false` |
| `size_bytes` | BIGINT | no | `CHECK (size_bytes >= 0)`; with `mtime`, what a scan compares to skip an unchanged subtitle |
| `mtime` | TIMESTAMPTZ | yes | |
| `created_at` / `updated_at` | TIMESTAMPTZ | no | |

Indexes on `file_id` and `library_id`. A row no scan finds any more, or whose video is no longer
indexed, is deleted outright: nothing references it. Purging a video's `files` row takes its
subtitles with it.

### `applied_nfos`
What each NFO beside the media held when the indexer last applied it (issue #184, FR-219). An NFO
is re-applied exactly when its size or content hash differs from its row, never by comparing its
modification time with a scan's. Classification records the NFOs it reads for a new file; a scan
and a watcher event record the ones they re-apply.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | UUID | no | PK |
| `library_id` | UUID | no | FK → `libraries.id`, cascade |
| `path` | TEXT | no | unique (`applied_nfos_path_unique`) — the NFO; recorded by it |
| `size_bytes` | BIGINT | no | `CHECK (size_bytes >= 0)` |
| `content_hash` | TEXT | no | XXH3-128 of the NFO's bytes, as hex — tells one version of the file from the next |
| `change_stamp` | TEXT | yes | the NFO's size, modification and change times as a stat reported them; a scan that finds the same stamp does not read the NFO again. NULL where the platform has no change time, or when the NFO was written within two seconds of being read |
| `created_at` / `updated_at` | TIMESTAMPTZ | no | |

Index on `library_id`. The row of an NFO no scan finds any more, or that the watcher reports
removed, is deleted outright.

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
carryover is deliberately not attempted. A row outlives its file going missing (the `files` row is
only soft-deleted) and is dropped from continue-watching and history while the file is missing; it
is removed only when the file is purged. The list reads join `files` and filter
`missing_since IS NULL` in the same statement as their `LIMIT`/`OFFSET` and `COUNT`, so missing
rows neither take a page slot nor inflate the history total.

### `playback_start_counts` / `playback_rebuffer_counts` / `playback_switch_counts`
Operator-local playback telemetry (issue #143, [ADR-0019](decisions/ADR-0019-telemetry-posture.md)):
daily counters, never events. No column references a user, a file or a title, so there are no
foreign keys and nothing here is a viewing record; the server resolves a reported file to these
dimensions and discards its id before writing.

| Table | Primary key (every column `TEXT NOT NULL` except `day DATE`) | Counters (`BIGINT NOT NULL CHECK (>= 0)`) |
|---|---|---|
| `playback_start_counts` | `day, client_kind, outcome, reason, stage, container, video_codec, audio_codec, height_class` | `count` |
| `playback_rebuffer_counts` | `day, client_kind, container, video_codec, height_class, bitrate_class` | `events`, `total_ms`, and one per duration bucket: `lt_1s`, `s1_3`, `s3_10`, `s10_30`, `ge_30s` |
| `playback_switch_counts` | `day, client_kind, trigger, from_height_class, to_height_class` | `count` |

Absent values are sentinels, not `NULL` (`none` for a stream the file lacks or a start that did not
fail, `unknown` for one the prober could not name), because the key columns are the `ON CONFLICT`
target every write increments through. `playback_start_counts` also checks that `outcome =
'started'` exactly when `reason` and `stage` are `none`. `day` leads every key, which serves both
the report's date range and the daily retention prune (`DELETE ... WHERE day < cutoff`). The
vocabularies themselves are not `CHECK`ed: a new client kind is a code change, not a migration.
A reported batch is written in one transaction, as at most one multi-row upsert per table adding
the batch's tally (`count = count + excluded.count`), so a batch is counted whole or not at all and
its rows are locked in key order.

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
- **One file per `file_path`:** a path is one row, whatever its content, while the same hash may sit
  at many paths. The indexer also serialises every scan and watcher reconcile of a library, so it
  never races itself to the insert.
- **One entry per `(library_id, movie_id, edition)`:** editions are a per-library, per-movie
  namespace.
- **One title per pin:** `movies.pinned_ref` and `shows.pinned_ref` are unique, so a provider id an
  NFO names pins at most one movie and one show.
- **One sidecar subtitle per path:** `sidecar_subtitles.path` is unique.
- **One applied-NFO record per path:** `applied_nfos.path` is unique.
- **One season per `(show_id, season_number)`, one episode per `(season_id, episode_number)`:**
  prevents duplicate rows on rescans.
- **`users` identity is `(oidc_issuer, oidc_subject)`, not a password:** no end-user credential is
  stored in Postgres at all; `beam-server` never sees or stores a password.
- **`sessions.token_hash` is the only session-lookup key,** and it is a hash — a Postgres dump or
  backup leak does not by itself expose valid sessions.
- **Read-only media filesystem:** not a table-level constraint, but a whole-system invariant that
  bounds what `files.file_path` can ever be used for — read access only, never write. See
  `security.md`.
