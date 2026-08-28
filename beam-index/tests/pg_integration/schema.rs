//! What the migrations actually produce, and that they reverse.
//!
//! `beam-migration` is excluded from mutation testing and has no hermetic
//! tests -- DDL has no behaviour a fake can stand in for. This is where it is
//! verified instead.

use sea_orm::{ConnectionTrait, Statement};

use beam_test_support::postgres::{
    ScopedSchema, connection, migrate_down, migrate_up, table_names,
};

#[tokio::test]
async fn migrations_apply_and_fully_reverse() {
    let scoped = ScopedSchema::create("migrate")
        .await
        .expect("create schema");
    let name = scoped.name().to_string();

    migrate_up(scoped.db().as_ref())
        .await
        .expect("migrations apply to an empty schema");
    let after_up = table_names(scoped.db().as_ref(), &name)
        .await
        .expect("list tables");
    assert!(
        after_up.contains(&"playback_progress".to_string()),
        "expected the migrated schema to contain the application tables, got {after_up:?}"
    );

    migrate_down(scoped.db().as_ref())
        .await
        .expect("every migration reverses");
    let after_down = table_names(scoped.db().as_ref(), &name)
        .await
        .expect("list tables");
    assert!(
        after_down.iter().all(|table| table == "seaql_migrations"),
        "down migrations must leave nothing but the migration ledger, got {after_down:?}"
    );

    scoped.drop_schema().await.expect("drop schema");
}

#[tokio::test]
async fn playback_progress_enforces_one_row_per_user_and_file() {
    let db = connection().await;
    let db = db.as_ref();
    let user = beam_test_support::seed::user(db).await.unwrap();
    let file = beam_test_support::seed::file(db).await.unwrap();

    let insert = |id: uuid::Uuid| {
        Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO playback_progress \
             (id, user_id, file_id, position_secs, duration_secs, completed, updated_at) \
             VALUES ($1, $2, $3, 1.0, 100.0, false, now())",
            [id.into(), user.into(), file.into()],
        )
    };

    db.execute(insert(uuid::Uuid::new_v4()))
        .await
        .expect("the first row for a (user, file) pair inserts");
    let second = db.execute(insert(uuid::Uuid::new_v4())).await;

    assert!(
        second.is_err(),
        "the unique index on (user_id, file_id) must reject a second row; \
         without it `upsert`'s ON CONFLICT target would not exist"
    );
}

#[tokio::test]
async fn playback_progress_rows_are_removed_with_their_user() {
    let db = connection().await;
    let db = db.as_ref();
    let user = beam_test_support::seed::user(db).await.unwrap();
    let file = beam_test_support::seed::file(db).await.unwrap();

    db.execute(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO playback_progress \
         (id, user_id, file_id, position_secs, duration_secs, completed, updated_at) \
         VALUES ($1, $2, $3, 1.0, 100.0, false, now())",
        [uuid::Uuid::new_v4().into(), user.into(), file.into()],
    ))
    .await
    .expect("insert progress");

    db.execute(Statement::from_sql_and_values(
        db.get_database_backend(),
        "DELETE FROM users WHERE id = $1",
        [user.into()],
    ))
    .await
    .expect("deleting a user cascades rather than failing on the foreign key");

    let remaining = db
        .query_all(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT id FROM playback_progress WHERE user_id = $1",
            [user.into()],
        ))
        .await
        .expect("query progress");

    assert!(
        remaining.is_empty(),
        "a deleted user must not leave orphaned playback rows"
    );
}
