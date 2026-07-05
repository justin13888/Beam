# Component: `beam-entity` and `beam-migration`

Status: describes target-state ownership and conventions. For the full column-by-column schema —
including every table this push adds or removes — see
[docs/architecture/data-model.md](../architecture/data-model.md), which is the authoritative
reference. This document does not duplicate that content; it describes *how the two crates relate
and the conventions new migrations/entities must follow*.

## Division of responsibility

`beam-migration` is the source of truth for the database schema: it holds every `sea-orm-migration`
migration, in chronological order, and defines the ENUM types, tables, indexes, foreign keys, and
CHECK constraints that actually exist in Postgres. `beam-entity` is a mirror: one SeaORM
`DeriveEntityModel` struct per table, generated/maintained by hand to match exactly what the latest
migration state produces. **`beam-entity` never drives schema changes — it follows them.** Any schema
change starts in `beam-migration`; `beam-entity` is updated afterward to match. This is CLAUDE.md's
workflow rule #1 made concrete: check both crates before touching schema, and change them in that
order.

Neither crate contains business logic. `beam-entity` is a pure data-shape layer (entity structs +
`Relation`/`ActiveModelBehavior` boilerplate); `beam-migration` is pure DDL. Repository
implementations that satisfy `beam-domain`'s repository traits (living in `beam-index` and
`beam-auth`) are the only code that queries through `beam-entity`.

## `beam-migration` conventions

- One file per migration, named `m<YYYYMMDD>_<NNNNNN>_<description>.rs` (e.g.
  `m20260209_000001_create_schema.rs`, `m20260522_000001_add_file_mtime.rs`), registered in order in
  `Migrator::migrations()` in `src/lib.rs`. Migrations are additive and ordered — never edit a
  migration that has already landed on any shared environment; add a new one.
  This push adds new migrations for: the `sessions` table, the destructive `users` reshape (OIDC
  columns replacing username/password), the `metadata_enrichment` table, `anilist_id` columns on
  `movies`/`shows`, the `playback_progress` table, and the `stream_cache` drop. See
  [data-model.md](../architecture/data-model.md) for exactly what each contains.
- Every `up()` has a corresponding `down()` that reverses it, including custom Postgres ENUM types
  (`Type::create()`/`Type::drop()`), which today's `m20260209_000001_create_schema.rs` demonstrates
  for `StreamType` and `FileStatus`.
- Table/column identifiers are declared as `#[derive(DeriveIden)]` enums local to each migration file
  (see `Libraries`, `Movies`, `Files`, etc. in `m20260209_000001_create_schema.rs`) — this is the
  established pattern for new migrations too.
- `main.rs` is the `sea-orm-migration` CLI entry point (`migrate up`/`down`/`status`, etc.) used in
  local dev and deployment; it is not application code.

## `beam-entity` conventions

- One file per table under `src/`, each defining a `Model` struct with `#[sea_orm(table_name = "...")]`
  and `#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]`, plus a
  `Relation` enum (`#[derive(EnumIter, DeriveRelation)]`) capturing foreign-key relationships, and an
  empty `impl ActiveModelBehavior for ActiveModel {}` unless custom insert/update hooks are needed.
  `src/lib.rs` re-exports each table's `Entity` under a short name (`pub use movie::Entity as Movie;`,
  etc.) — new tables follow the same pattern.
- **UUID primary keys throughout**, application-generated (`Uuid::new_v4()` in the repository
  `create()` methods), not `gen_random_uuid()`-on-insert, except where `data-model.md` notes
  otherwise (e.g. `admin_logs.id` defaults to `gen_random_uuid()` at the database level).
- Postgres ENUM-backed columns use SeaORM's `DeriveActiveEnum` on a Rust enum matching the migration's
  `Type::create().as_enum(...)` values (see `beam_entity::admin_log::AdminLogLevel`/
  `AdminLogCategory`, `beam_entity::files`'s `FileStatus`, `beam_entity::media_stream`'s
  `StreamType`). New enum columns this push (e.g. `sessions`, `metadata_enrichment.status`) follow the
  same shape.
- `beam-domain` models convert from `beam-entity` models via `#[cfg(feature = "entity")] impl
  From<beam_entity::X::Model> for domain::X`, never the reverse — `beam-entity` has no dependency on
  `beam-domain`.

## What is changing this push (ownership-relevant summary)

- **Additions**: `sessions` (replacing Redis/Valkey-backed sessions — see
  [ADR-0005](../architecture/decisions/ADR-0005-sessions-in-postgres.md)), `metadata_enrichment`,
  `playback_progress` tables; `anilist_id` on
  `movies`/`shows`; the `genres`/`movie_genres`/`show_genres` tables (which already exist in both
  crates today) finally get populated, since the enrichment pipeline that writes to them is being
  built (see [indexer.md](indexer.md)) — no schema change needed for genres, just a consumer.
- **Removals**: `stream_cache` is dropped (`beam-entity::stream_cache` and its migration entry are
  removed) — it backed an HLS/remux cache path that is deleted along with transcoding, see
  [ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md).
- **Destructive reshape**: `users` drops `username`/`password_hash` and gains `oidc_issuer`/
  `oidc_subject`/`display_name`/`avatar_url`, since auth moves to OIDC — see
  [ADR-0003](../architecture/decisions/ADR-0003-oidc-bff-auth.md). This is a genuine breaking schema
  change, not an additive one; treat any environment with pre-push user data as needing a wipe/reseed,
  not a data migration, per the target architecture's framing of this as pre-alpha software.

For column-level detail on any of the above, consult
[data-model.md](../architecture/data-model.md) rather than this file.
