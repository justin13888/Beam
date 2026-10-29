//! What the migrations actually produce, and that they reverse.
//!
//! `beam-migration` is excluded from mutation testing and has no hermetic
//! tests -- DDL has no behaviour a fake can stand in for. This is where it is
//! verified instead.

use sea_orm::{ConnectionTrait, Statement};

use beam_test_support::postgres::{
    ScopedSchema, connection, migrate_down, migrate_up, table_names,
};

/// The names in column `v` of what `sql` (bound to the schema's name as `$1`)
/// returns.
async fn names_in_schema(db: &sea_orm::DatabaseConnection, sql: &str, schema: &str) -> Vec<String> {
    db.query_all_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        sql,
        [schema.into()],
    ))
    .await
    .expect("query the catalog")
    .into_iter()
    .map(|row| row.try_get::<String>("", "v").expect("a text column v"))
    .collect()
}

const ENUM_TYPES: &str = "SELECT t.typname::text AS v FROM pg_type t \
     JOIN pg_namespace n ON n.oid = t.typnamespace \
     WHERE n.nspname = $1 AND t.typtype = 'e' ORDER BY 1";
/// Every index but the ledger's own primary key, which `down` keeps with
/// the ledger.
const INDEXES: &str = "SELECT indexname::text AS v FROM pg_indexes \
     WHERE schemaname = $1 AND tablename <> 'seaql_migrations' ORDER BY 1";

/// The tables the migrations build are exactly the entities' tables and the
/// ledger: a table no entity reads, or an entity whose table is missing,
/// fails here. `down` then leaves nothing of the application -- no table, no
/// enum type, no index -- and `up` builds the same schema again over that.
#[tokio::test]
async fn migrations_apply_and_fully_reverse() {
    let scoped = ScopedSchema::create("migrate")
        .await
        .expect("create schema");
    let name = scoped.name().to_string();
    let db = scoped.db();
    let db = db.as_ref();

    let mut expected: Vec<String> = crate::entity::entity_table_names()
        .into_iter()
        .chain(["seaql_migrations"])
        .map(str::to_owned)
        .collect();
    expected.sort();

    migrate_up(db)
        .await
        .expect("migrations apply to an empty schema");
    let after_up = table_names(db, &name).await.expect("list tables");
    assert_eq!(
        after_up, expected,
        "the migrated tables are the entities' tables and the ledger"
    );
    assert!(
        !names_in_schema(db, ENUM_TYPES, &name).await.is_empty()
            && !names_in_schema(db, INDEXES, &name).await.is_empty(),
        "the migrated schema has enum types and indexes for down() to remove"
    );

    migrate_down(db).await.expect("every migration reverses");
    assert_eq!(
        table_names(db, &name).await.expect("list tables"),
        vec!["seaql_migrations".to_string()],
        "down migrations must leave nothing but the migration ledger"
    );
    assert_eq!(
        names_in_schema(db, ENUM_TYPES, &name).await,
        Vec::<String>::new(),
        "down migrations must drop every enum type"
    );
    assert_eq!(
        names_in_schema(db, INDEXES, &name).await,
        Vec::<String>::new(),
        "down migrations must drop every index"
    );

    migrate_up(db)
        .await
        .expect("migrations apply again over the reversed schema");
    assert_eq!(
        table_names(db, &name).await.expect("list tables"),
        expected,
        "the reapplied schema has the same tables"
    );

    scoped.drop_schema().await.expect("drop schema");
}

/// A movie's watched state for one user is one row (issue #188): without
/// the unique index `record_progress`'s ON CONFLICT target would not exist.
#[tokio::test]
async fn watch_state_enforces_one_row_per_user_and_title() {
    let db = connection().await;
    let db = db.as_ref();
    let user = beam_test_support::seed::user(db).await.unwrap();
    let movie = beam_test_support::seed::movie(db).await.unwrap();

    let insert = |id: uuid::Uuid| {
        Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO watch_state (id, user_id, movie_id, last_played_at) \
             VALUES ($1, $2, $3, now())",
            [id.into(), user.into(), movie.into()],
        )
    };

    db.execute_raw(insert(uuid::Uuid::new_v4()))
        .await
        .expect("the first row for a (user, movie) pair inserts");
    assert!(
        db.execute_raw(insert(uuid::Uuid::new_v4())).await.is_err(),
        "the unique index on (user_id, movie_id) must reject a second row"
    );
}

#[tokio::test]
async fn watch_state_rows_are_removed_with_their_user() {
    let db = connection().await;
    let db = db.as_ref();
    let user = beam_test_support::seed::user(db).await.unwrap();
    let movie = beam_test_support::seed::movie(db).await.unwrap();

    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO watch_state (id, user_id, movie_id, last_played_at) \
         VALUES ($1, $2, $3, now())",
        [uuid::Uuid::new_v4().into(), user.into(), movie.into()],
    ))
    .await
    .expect("insert watched state");

    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "DELETE FROM users WHERE id = $1",
        [user.into()],
    ))
    .await
    .expect("deleting a user cascades rather than failing on the foreign key");

    let remaining = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT id FROM watch_state WHERE user_id = $1",
            [user.into()],
        ))
        .await
        .expect("query watched state");

    assert!(
        remaining.is_empty(),
        "a deleted user must not leave orphaned watched state"
    );
}

/// `idx_movies_identity_key` and `idx_shows_identity_key` are what make
/// find-or-create by identity key one atomic statement (issue #183): without
/// them `ON CONFLICT (identity_key)` has no target. The column stays nullable
/// with any number of `NULL`s, for titles that predate it.
#[tokio::test]
async fn identity_keys_are_unique_but_any_number_of_titles_may_lack_one() {
    let db = connection().await;
    let db = db.as_ref();

    for table in ["movies", "shows"] {
        let insert = |key: Option<String>| {
            Statement::from_sql_and_values(
                db.get_database_backend(),
                format!(
                    "INSERT INTO {table} (id, title, identity_key, created_at, updated_at) \
                     VALUES ($1, 'pg-integration', $2, now(), now())"
                ),
                [uuid::Uuid::new_v4().into(), key.into()],
            )
        };
        let key = format!("{table} {}|", uuid::Uuid::new_v4());

        db.execute_raw(insert(Some(key.clone())))
            .await
            .expect("the first title with a key inserts");
        assert!(
            db.execute_raw(insert(Some(key))).await.is_err(),
            "a second {table} row with the same identity key must be rejected"
        );
        for _ in 0..2 {
            db.execute_raw(insert(None))
                .await
                .expect("titles without a key never collide");
        }
    }
}
