//! A database of its own for every test.
//!
//! The server migrates whatever database it is pointed at, and the suite
//! asserts on rows it never inserted itself, so tests cannot share one: each
//! creates `beam_e2e_<uuid>` on the Postgres [`DATABASE_URL_ENV`] names, hands
//! its URL to the server, and drops it once the test has passed. A failed test
//! leaves its database behind to be looked at; the next run sweeps it.

use std::sync::OnceLock;
use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use uuid::Uuid;

/// Environment variable naming the Postgres the tier creates its databases on.
/// Its own database is only connected to, never written.
pub const DATABASE_URL_ENV: &str = "BEAM_E2E_DATABASE_URL";

/// Every database this tier creates starts with this.
const PREFIX: &str = "beam_e2e_";

/// How long to wait for a connection before concluding nothing is there.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The administrative connection string, or a message saying what to set.
///
/// A hard failure rather than a skip, as in the `pg-integration` tier: a tier
/// that quietly passes when it did not run is worse than one that does not run.
pub fn admin_url() -> String {
    std::env::var(DATABASE_URL_ENV).unwrap_or_else(|_| {
        panic!(
            "the e2e-server tier needs a real Postgres: set {DATABASE_URL_ENV}, e.g. \
             postgres://beam:password@localhost:5432/beam, or run `mise run e2e:server`, \
             which composes one. This tier is opt-in and never part of `cargo test --workspace`."
        )
    })
}

/// A freshly created, empty database.
#[derive(Debug)]
pub struct EphemeralDatabase {
    name: String,
    url: String,
}

impl EphemeralDatabase {
    /// Create one. Databases an earlier run left behind are dropped first,
    /// once per process.
    pub async fn create() -> Self {
        sweep_once();
        let name = format!("{PREFIX}{}", Uuid::new_v4().simple());
        let admin = connect(&admin_url()).await;
        admin
            .execute_unprepared(&format!(r#"CREATE DATABASE "{name}""#))
            .await
            .unwrap_or_else(|e| panic!("create the test database {name}: {e}"));
        let url = with_database(&admin_url(), &name);
        Self { name, url }
    }

    /// The connection string the server is given.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// A connection of the test's own, for asserting on what the server wrote.
    pub async fn connect(&self) -> DatabaseConnection {
        connect(&self.url).await
    }

    /// Drop the database. Called once the test has passed; `FORCE` ends any
    /// connection still open to it.
    pub async fn drop_database(self) {
        let admin = connect(&admin_url()).await;
        admin
            .execute_unprepared(&format!(
                r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#,
                self.name
            ))
            .await
            .unwrap_or_else(|e| panic!("drop the test database {}: {e}", self.name));
    }
}

/// `url` with its database replaced by `name`.
fn with_database(url: &str, name: &str) -> String {
    let mut parsed = reqwest::Url::parse(url)
        .unwrap_or_else(|e| panic!("{DATABASE_URL_ENV} is not a URL ({e})"));
    parsed.set_path(&format!("/{name}"));
    parsed.to_string()
}

/// Whether the administrative Postgres accepts a connection now; the error
/// is why not.
pub async fn probe() -> Result<(), String> {
    let mut options = ConnectOptions::new(admin_url());
    options
        .max_connections(1)
        .min_connections(0)
        .connect_timeout(CONNECT_TIMEOUT)
        .acquire_timeout(CONNECT_TIMEOUT)
        .sqlx_logging(false);
    let connection = Database::connect(options)
        .await
        .map_err(|e| e.to_string())?;
    connection.close().await.map_err(|e| e.to_string())
}

async fn connect(url: &str) -> DatabaseConnection {
    let mut options = ConnectOptions::new(url.to_owned());
    options
        .max_connections(2)
        .min_connections(0)
        .connect_timeout(CONNECT_TIMEOUT)
        .acquire_timeout(CONNECT_TIMEOUT)
        .sqlx_logging(false);
    Database::connect(options).await.unwrap_or_else(|e| {
        panic!(
            "could not reach the e2e-server Postgres named by {DATABASE_URL_ENV}: {e}. \
             `mise run e2e:server` composes one."
        )
    })
}

/// Drop every `beam_e2e_*` database left by an earlier run, the first time
/// this is called in the process.
///
/// On a thread and runtime of its own, as `postgres::migrate_once` does: the
/// connection must not belong to whichever test's runtime happened to be
/// first, which ends with that test.
fn sweep_once() {
    static SWEPT: OnceLock<Result<(), String>> = OnceLock::new();
    let outcome = SWEPT.get_or_init(|| {
        std::thread::spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("build the sweep runtime: {e}"))?
                .block_on(async {
                    let admin = connect(&admin_url()).await;
                    let rows = admin
                        .query_all_raw(Statement::from_string(
                            admin.get_database_backend(),
                            format!(
                                "SELECT datname FROM pg_database WHERE datname LIKE '{PREFIX}%'"
                            ),
                        ))
                        .await
                        .map_err(|e| format!("list stale test databases: {e}"))?;
                    for row in rows {
                        let name: String = row
                            .try_get("", "datname")
                            .map_err(|e| format!("read a database name: {e}"))?;
                        admin
                            .execute_unprepared(&format!(
                                r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#
                            ))
                            .await
                            .map_err(|e| format!("drop the stale test database {name}: {e}"))?;
                    }
                    Ok(())
                })
        })
        .join()
        .unwrap_or_else(|_| Err("the sweep thread panicked".to_owned()))
    });
    if let Err(message) = outcome {
        panic!("{message}");
    }
}
