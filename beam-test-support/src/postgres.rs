//! Connecting to, and preparing, the Postgres the `pg-integration` tier runs
//! against.

use std::sync::OnceLock;

use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

/// Environment variable naming the database the tier runs against.
pub const DATABASE_URL_ENV: &str = "BEAM_TEST_DATABASE_URL";

/// How long to wait for a connection before concluding nothing is there.
/// Short on purpose: this tier is opt-in, so the database is either running or
/// the developer wants to know immediately that it is not.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The connection string, or a message explaining precisely what to set.
///
/// Deliberately a hard failure rather than a silent skip: a tier that quietly
/// passes when it did not run is worse than one that does not run at all.
pub fn database_url() -> String {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        std::env::var(DATABASE_URL_ENV).unwrap_or_else(|_| {
            panic!(
                "the pg-integration tier needs a real Postgres: set {DATABASE_URL_ENV}, \
                 e.g. `docker compose -f compose.dependencies.yaml up -d` then \
                 {DATABASE_URL_ENV}=postgres://beam:beam@localhost:5432/beam. \
                 This tier is opt-in and never part of `cargo test --workspace`."
            )
        })
    })
    .clone()
}

/// A connection to the migrated test database.
///
/// Each caller gets its **own** small pool rather than sharing one. `#[tokio::test]`
/// builds a fresh current-thread runtime per test and tears it down at the end,
/// and a `sqlx` pool is bound to the runtime that created it -- a shared static
/// pool starts failing with "a Tokio 1.x context was found, but it is being
/// shutdown" as soon as the first test finishes. A per-test pool is bound to
/// the runtime that uses it and dies with it.
///
/// Migrations run exactly once per process, on a runtime of their own so they
/// are not tied to whichever test happened to be first.
///
/// Tests stay independent because every row they touch is keyed by identifiers
/// [`crate::seed`] allocates fresh, so no test can observe another's rows.
pub async fn connection() -> std::sync::Arc<DatabaseConnection> {
    migrate_once();

    let mut options = ConnectOptions::new(database_url());
    options
        .max_connections(4)
        .min_connections(0)
        // Fail fast when nothing is listening. The default retries for
        // minutes, so a developer who forgot to start the database waits
        // nine minutes to be told so.
        .connect_timeout(CONNECT_TIMEOUT)
        .acquire_timeout(CONNECT_TIMEOUT)
        .sqlx_logging(false);
    std::sync::Arc::new(
        Database::connect(options)
            .await
            .expect("connect to the pg-integration database"),
    )
}

/// Apply the migrations the first time this is called in the process.
///
/// The outcome -- including failure -- is cached. Without that, a run with no
/// database standing has every test pay the connect timeout in turn, and
/// "you forgot to start Postgres" takes minutes to say instead of seconds.
fn migrate_once() {
    static MIGRATED: OnceLock<Result<(), String>> = OnceLock::new();
    let outcome = MIGRATED.get_or_init(|| {
        // A dedicated thread and runtime: the connection and its background
        // tasks must not outlive, or be owned by, a caller's test runtime.
        std::thread::spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("build the migration runtime: {e}"))?
                .block_on(async {
                    let mut options = ConnectOptions::new(database_url());
                    options.connect_timeout(CONNECT_TIMEOUT).sqlx_logging(false);
                    let db = Database::connect(options).await.map_err(|e| {
                        format!(
                            "could not reach the pg-integration database at \
                             {DATABASE_URL_ENV}: {e}. Start it with \
                             `docker compose -f compose.dependencies.yaml up -d`."
                        )
                    })?;
                    drop_stale_scoped_schemas(&db)
                        .await
                        .map_err(|e| format!("drop scoped schemas from an earlier run: {e}"))?;
                    beam_migration::Migrator::up(&db, None)
                        .await
                        .map_err(|e| format!("apply migrations: {e}"))
                })
        })
        .join()
        .unwrap_or_else(|_| Err("the migration thread panicked".to_string()))
    });

    if let Err(message) = outcome {
        panic!("{message}");
    }
}

/// Drop every `beam_test_*` schema left behind by a previous run.
///
/// [`ScopedSchema`] cannot clean up in `Drop` (dropping cannot await), and a
/// test that fails part-way never reaches its explicit cleanup. Sweeping at
/// process start keeps the test database from accumulating them without making
/// every test carry its own teardown.
async fn drop_stale_scoped_schemas(db: &DatabaseConnection) -> Result<(), DbErr> {
    use sea_orm::{ConnectionTrait, Statement};

    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT nspname FROM pg_namespace WHERE nspname LIKE 'beam_test_%'",
        ))
        .await?;
    for row in rows {
        let name: String = row.try_get("", "nspname")?;
        db.execute(Statement::from_string(
            db.get_database_backend(),
            format!(r#"DROP SCHEMA IF EXISTS "{name}" CASCADE"#),
        ))
        .await?;
    }
    Ok(())
}

/// A connection scoped to its own freshly-created Postgres schema, dropped when
/// the guard is.
///
/// Used by tests that must own the whole schema -- notably the migration
/// up/down round-trip, which would otherwise tear down the tables every other
/// test is using.
pub struct ScopedSchema {
    // An `Arc` rather than the connection itself: sea-orm's `mock` feature (on
    // for the hermetic SQL-shape tests) removes `Clone` from
    // `DatabaseConnection`, so a caller cannot copy one out.
    db: std::sync::Arc<DatabaseConnection>,
    name: String,
}

impl ScopedSchema {
    /// Create a schema named after `label` plus a fresh suffix, and return a
    /// connection whose `search_path` points at it.
    pub async fn create(label: &str) -> Result<Self, DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        let name = format!("beam_test_{label}_{}", uuid::Uuid::new_v4().simple());
        let mut admin_options = ConnectOptions::new(database_url());
        admin_options
            .connect_timeout(CONNECT_TIMEOUT)
            .sqlx_logging(false);
        let admin = Database::connect(admin_options).await?;
        admin
            .execute(Statement::from_string(
                admin.get_database_backend(),
                format!(r#"CREATE SCHEMA "{name}""#),
            ))
            .await?;
        drop(admin);

        let mut options = ConnectOptions::new(database_url());
        options
            .max_connections(2)
            .sqlx_logging(false)
            // Every statement on this pool resolves unqualified names inside the
            // scoped schema; `public` stays on the path so shared extensions
            // (pg_trgm) remain reachable.
            .set_schema_search_path(format!("{name},public"));
        let db = Database::connect(options).await?;

        Ok(Self {
            db: std::sync::Arc::new(db),
            name,
        })
    }

    /// The scoped connection. Cloning the handle is cheap and shares one pool.
    pub fn db(&self) -> std::sync::Arc<DatabaseConnection> {
        self.db.clone()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Create a scoped schema with every migration already applied.
    ///
    /// Used by contracts whose assertions are *global* -- a total count, a
    /// full-table ordering -- which cannot hold on a database other tests are
    /// concurrently writing to. Costs one migration run per call, which is why
    /// it is not the default.
    pub async fn create_migrated(label: &str) -> Result<Self, DbErr> {
        migrate_once();
        let scoped = Self::create(label).await?;
        migrate_up(scoped.db().as_ref()).await?;
        Ok(scoped)
    }

    /// Drop the schema and everything in it. Call at the end of a test; the
    /// `Drop` impl cannot await, so cleanup is explicit.
    pub async fn drop_schema(self) -> Result<(), DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        let Self { db, name } = self;
        drop(db);
        let admin = Database::connect(database_url()).await?;
        admin
            .execute(Statement::from_string(
                admin.get_database_backend(),
                format!(r#"DROP SCHEMA IF EXISTS "{name}" CASCADE"#),
            ))
            .await?;
        Ok(())
    }
}

/// Apply every migration to `db`.
///
/// Re-exported through this crate so the test binaries do not each need
/// `beam-migration` and `sea-orm-migration` in their own dev-dependencies.
pub async fn migrate_up(db: &DatabaseConnection) -> Result<(), DbErr> {
    beam_migration::Migrator::up(db, None).await
}

/// Reverse every migration on `db`.
pub async fn migrate_down(db: &DatabaseConnection) -> Result<(), DbErr> {
    beam_migration::Migrator::down(db, None).await
}

/// The tables present in `schema`, sorted.
pub async fn table_names(db: &DatabaseConnection, schema: &str) -> Result<Vec<String>, DbErr> {
    use sea_orm::{ConnectionTrait, Statement};

    let rows = db
        .query_all(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT tablename FROM pg_tables WHERE schemaname = $1 ORDER BY tablename",
            [schema.into()],
        ))
        .await?;
    rows.into_iter()
        .map(|row| row.try_get::<String>("", "tablename"))
        .collect()
}
