# Beam Entity

`sea-orm` entity definitions mapping directly to the tables created by `beam-migration`. This is
a thin, mechanical layer -- one entity module per table, no business logic -- consumed by
`beam-index`/`beam-server`'s repository implementations and by `beam-domain`'s optional `entity`
feature (row ↔ domain-model conversions). See
[`docs/components/persistence.md`](../docs/components/persistence.md) and
[`docs/architecture/data-model.md`](../docs/architecture/data-model.md) for the schema itself.

## Structure

One module per entity, each re-exporting the conventional sea-orm `Entity`/`Model`/`ActiveModel`/
`Column`/`Relation` types: `admin_log`, `episode`, `files`, `genre`, `library`, `library_movie`,
`library_show`, `media_stream`, `metadata_enrichment`, `movie`, `movie_entry`, `movie_genre`,
`pending_auth`, `playback_progress`, `season`, `session`, `show`, `show_genre`, `user`.

Keep this crate and `beam-migration` in lockstep: a schema change needs a migration here *and* a
matching entity update, in the same commit.
