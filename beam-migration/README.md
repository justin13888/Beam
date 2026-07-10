# Beam Migrations

`sea-orm-migration`-based schema migrations for beam's Postgres database. Migrations run in
order and are additive by file name (`m<timestamp>_<description>.rs`); see
[`docs/architecture/components.md`](../docs/architecture/components.md) and
[`docs/architecture/data-model.md`](../docs/architecture/data-model.md) for the current schema
and its invariants.

Requires `DATABASE_URL` to be set (see [`docs/operations/configuration.md`](../docs/operations/configuration.md)).

```sh
# Apply all pending migrations
cargo run -p beam-migration -- up

# Roll back the most recent migration
cargo run -p beam-migration -- down

# Check migration status
cargo run -p beam-migration -- status
```

> Beam is pre-alpha: destructive migrations (dropping/altering columns without a backward-
> compatible path) are acceptable for now -- see the migrations under `src/` for examples (e.g.
> the OIDC cutover migration drops the `users.password_hash`/`username` columns outright).
