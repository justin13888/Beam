//! One composed stack per test: a database of its own, a library of its own,
//! and a `beam-server` process over both, signed in to through Dex.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use sea_orm::DatabaseConnection;
use tempfile::TempDir;

use super::client::{self, api};
use super::database::{self, EphemeralDatabase};
use super::idp::{self, Account, Browser};
use super::library::FixtureLibrary;
use super::server::{self, ServerProcess};
use super::wait;

/// How long a device sign-in may take once approved.
const DEVICE_SIGN_IN_BOUND: Duration = Duration::from_secs(60);

/// How long the composed Postgres and Dex have to come up. Generous: a
/// container runtime that has just started them may still be initialising
/// the database.
const DEPENDENCY_BOUND: Duration = Duration::from_secs(120);

/// What a test changes about the stack it starts.
#[derive(Debug, Default, Clone)]
pub struct StackOptions {
    port: Option<u16>,
    env: Vec<(String, String)>,
}

impl StackOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Listen on exactly `port`. Only the browser sign-in needs this: Dex's
    /// static client registers one redirect URI,
    /// `http://localhost:8000/v1/auth/callback`. Every other test takes a
    /// free port.
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set a server environment variable, over the suite's default for it.
    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.env.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// A running stack.
#[derive(Debug)]
pub struct Stack {
    /// The tree the server is pointed at. Change it only through this.
    pub library: FixtureLibrary,
    database: EphemeralDatabase,
    db: DatabaseConnection,
    server: ServerProcess,
    browser: Browser,
    /// What [`Self::restart`] starts again: the binary, its port and its
    /// whole environment.
    binary: PathBuf,
    port: u16,
    settings: Vec<(String, String)>,
    /// Holds the server's `BEAM_DATA_DIR` and its log.
    work: TempDir,
}

impl Stack {
    /// Start `binary` -- the `beam-server` the calling test was built with --
    /// over a new database and an empty library.
    pub async fn start(binary: &Path, options: StackOptions) -> Self {
        let StackOptions { port, env } = options;
        dependencies_ready_once();
        let library = FixtureLibrary::new();
        let database = EphemeralDatabase::create().await;
        let work = tempfile::Builder::new()
            .prefix("beam-e2e-server-")
            .tempdir()
            .expect("create the server's work directory");
        let data_dir = work.path().join("data");
        let port = port.unwrap_or_else(server::free_port);

        let mut settings = default_env(port, &library.video_dir(), &data_dir, database.url());
        settings.extend(env);
        let server = ServerProcess::start(binary, port, &settings, work.path()).await;
        let db = database.connect().await;

        Self {
            library,
            database,
            db,
            server,
            browser: Browser::new(),
            binary: binary.to_path_buf(),
            port,
            settings,
            work,
        }
    }

    /// Stop the server and start it again over the same database, library
    /// and data directory, on the same port and with the same environment:
    /// what an operator's restart does. Sessions survive it, because they are
    /// rows in the database.
    ///
    /// For what only happens as the process starts -- the background workers'
    /// first pass, the artwork cache's index rebuilt from its files. The new
    /// process writes a fresh log over the old one's.
    pub async fn restart(&mut self) {
        self.server.stop();
        self.server =
            ServerProcess::start(&self.binary, self.port, &self.settings, self.work.path()).await;
    }

    /// The server's `BEAM_DATA_DIR`, as it was started with it.
    pub fn data_dir(&self) -> PathBuf {
        self.settings
            .iter()
            .rev()
            .find(|(name, _)| name == "BEAM_DATA_DIR")
            .map(|(_, value)| PathBuf::from(value))
            .expect("the suite always sets BEAM_DATA_DIR")
    }

    /// Where the server is reached.
    pub fn base_url(&self) -> &str {
        self.server.base_url()
    }

    /// The test's own connection to the server's database.
    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    /// The person at the browser.
    pub fn browser(&self) -> &Browser {
        &self.browser
    }

    /// A client presenting no session.
    pub fn anonymous(&self) -> api::Client {
        client::anonymous(self.base_url())
    }

    /// A client presenting `session`.
    pub fn client(&self, session: &str) -> api::Client {
        client::signed_in(self.base_url(), session)
    }

    /// Sign in as `account` through the device grant, the way the Android
    /// app does, and return the session.
    pub async fn sign_in(&self, account: Account) -> String {
        let anonymous = self.anonymous();
        let started = anonymous
            .start_device_login()
            .await
            .expect("start a device sign-in")
            .into_inner();
        let approve_at = started
            .verification_uri_complete
            .as_deref()
            .expect("Dex offers a complete verification URI");
        self.browser.approve_device(approve_at, account).await;

        let poll = api::types::DeviceLoginPoll {
            device_handle: started.device_handle.clone(),
        };
        wait::until(
            "the approved device sign-in to complete",
            DEVICE_SIGN_IN_BOUND,
            || {
                let request = anonymous.poll_device_login(&poll);
                async move {
                    match request.await.map(|response| response.into_inner()) {
                        Ok(api::PollDeviceLoginResponse::Status200(complete)) => {
                            Ok(complete.session_token)
                        }
                        Ok(api::PollDeviceLoginResponse::Status202(pending)) => {
                            // The server's interval, which a client must honour.
                            let interval = u64::try_from(pending.interval_secs).unwrap_or(5);
                            tokio::time::sleep(Duration::from_secs(interval)).await;
                            Err(format!("{:?}", pending.status))
                        }
                        Err(error) => panic!("polling the device sign-in failed: {error:?}"),
                    }
                }
            },
        )
        .await
    }

    /// Sign in as `account` through the browser flow and return the session.
    pub async fn sign_in_with_browser(&self, account: Account) -> String {
        self.browser.sign_in(self.base_url(), account).await
    }

    /// End the test: assert the library tree is exactly as the test left it,
    /// stop the server, and drop the database.
    pub async fn finish(self) {
        let Self {
            library,
            database,
            db,
            server,
            browser: _,
            binary: _,
            port: _,
            settings: _,
            work,
        } = self;
        library.assert_untouched();
        drop(server);
        drop(db);
        database.drop_database().await;
        drop(work);
    }
}

/// Wait, the first time this is called in the process, until Postgres accepts
/// a connection and Dex serves its discovery document.
///
/// Not every compose provider waits for a service's health check (podman
/// compose's `up --wait` returns at once), and `beam-server` reads the
/// issuer's discovery document only at startup, so the suite asks both
/// services itself rather than trusting whoever started them. On a thread and
/// runtime of its own, for the reason `database::sweep_once` gives.
fn dependencies_ready_once() {
    static READY: OnceLock<bool> = OnceLock::new();
    let ready = READY.get_or_init(|| {
        std::thread::spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build the readiness runtime")
                .block_on(async {
                    wait::until(
                        "Postgres to accept connections",
                        DEPENDENCY_BOUND,
                        database::probe,
                    )
                    .await;
                    wait::until(
                        "Dex to serve its discovery document",
                        DEPENDENCY_BOUND,
                        idp::probe,
                    )
                    .await;
                });
        })
        .join()
        .is_ok()
    });
    assert!(
        *ready,
        "the tier's Postgres and Dex did not come up; the first failure says which"
    );
}

/// The server's configuration in this tier, before a test's own options.
///
/// The OIDC wiring is `mise run dev:up`'s against `dex/config.yaml`; the rest
/// makes the server deterministic to observe: no settle window, no periodic
/// rescan, no watcher unless a test turns it on, and no metadata provider --
/// enrichment would otherwise reach the internet and rename fixture titles
/// to whatever a provider matched.
fn default_env(
    port: u16,
    video_dir: &Path,
    data_dir: &Path,
    database_url: &str,
) -> Vec<(String, String)> {
    let path = |p: &Path| -> String { p.to_str().expect("a UTF-8 path").to_owned() };
    let url = format!("http://localhost:{port}");
    [
        ("BEAM_BIND_ADDRESS", format!("127.0.0.1:{port}")),
        ("BEAM_SERVER_URL", url.clone()),
        ("BEAM_WEB_URL", url),
        ("BEAM_VIDEO_DIR", path(video_dir)),
        ("BEAM_DATA_DIR", path(data_dir)),
        ("BEAM_DATABASE_URL", database_url.to_owned()),
        ("BEAM_DB_MIN_CONNECTIONS", "1".to_owned()),
        ("BEAM_DB_MAX_CONNECTIONS", "5".to_owned()),
        ("BEAM_OIDC_ISSUER", idp::issuer()),
        ("BEAM_OIDC_CLIENT_ID", idp::CLIENT_ID.to_owned()),
        ("BEAM_OIDC_CLIENT_SECRET", idp::CLIENT_SECRET.to_owned()),
        (
            "BEAM_OIDC_CLIENT_AUTH_METHOD",
            "client_secret_post".to_owned(),
        ),
        ("BEAM_OIDC_SCOPES", "openid profile email groups".to_owned()),
        ("BEAM_OIDC_ADMIN_CLAIM", "groups".to_owned()),
        ("BEAM_OIDC_ADMIN_VALUE", "beam-admin".to_owned()),
        ("BEAM_SCAN_SETTLE_SECS", "0".to_owned()),
        ("BEAM_SCAN_INTERVAL_SECS", "86400".to_owned()),
        ("BEAM_WATCH_ENABLED", "false".to_owned()),
        ("BEAM_ENRICH_INTERVAL_SECS", "86400".to_owned()),
        ("BEAM_ANILIST_ENABLED", "false".to_owned()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect()
}

/// The path of the workspace a test crate at `manifest_dir` belongs to.
pub fn workspace_root(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir)
        .parent()
        .expect("a crate inside the workspace")
        .to_path_buf()
}
