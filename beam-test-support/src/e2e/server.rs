//! The `beam-server` binary, run as the process an operator runs.
//!
//! Not the router in-process: the binary is what reads the environment,
//! migrates, starts the background indexer and the watcher, and binds. Its
//! output goes to a log file, printed when a test fails so the failure comes
//! with the server's side of it.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::wait;

/// How long the server has to answer its health check. Generous: it migrates
/// an empty database first, and a CI runner may be slow.
const STARTUP_BOUND: Duration = Duration::from_secs(120);

/// A running `beam-server`.
#[derive(Debug)]
pub struct ServerProcess {
    child: Child,
    base_url: String,
    log_path: PathBuf,
}

impl ServerProcess {
    /// Start `binary` with exactly `env` on top of a cleared environment,
    /// listening on `port`, and wait until it answers `GET /v1/health`.
    ///
    /// The environment is cleared so a developer's own `BEAM_*` variables --
    /// or a `.env` the binary would load from the working directory, which is
    /// why that is `log_dir` -- cannot change what the suite tests.
    pub async fn start(binary: &Path, port: u16, env: &[(String, String)], log_dir: &Path) -> Self {
        let log_path = log_dir.join("beam-server.log");
        let log = File::create(&log_path).expect("create the server log");
        let mut command = Command::new(binary);
        command
            .env_clear()
            .current_dir(log_dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("clone the server log handle"))
            .stderr(log);
        // A clean environment still needs a PATH and a HOME for anything the
        // process looks up, and the log filter if the developer set one.
        for inherited in ["PATH", "HOME", "RUST_LOG", "LD_LIBRARY_PATH"] {
            if let Some(value) = std::env::var_os(inherited) {
                command.env(inherited, value);
            }
        }
        for (name, value) in env {
            command.env(name, value);
        }
        let child = command
            .spawn()
            .unwrap_or_else(|e| panic!("start {}: {e}", binary.display()));

        let mut server = Self {
            child,
            base_url: format!("http://localhost:{port}"),
            log_path,
        };
        server.wait_until_healthy().await;
        server
    }

    /// Where the server is reached: the same `http://localhost:PORT` it is
    /// told is its own URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn wait_until_healthy(&mut self) {
        let health = format!("{}/v1/health", self.base_url);
        let http = reqwest::Client::new();
        let log_path = self.log_path.clone();
        let child = &mut self.child;
        wait::until(
            "the server to answer its health check",
            STARTUP_BOUND,
            || {
                let exited = child.try_wait().expect("poll the server process");
                let request = http.get(&health).send();
                let log_path = log_path.clone();
                async move {
                    if let Some(status) = exited {
                        panic!(
                            "beam-server exited during startup ({status}); its log:\n{}",
                            tail(&log_path, 200)
                        );
                    }
                    match request.await {
                        Ok(response) if response.status().is_success() => Ok(()),
                        Ok(response) => Err(format!("status {}", response.status())),
                        Err(error) => Err(error.to_string()),
                    }
                }
            },
        )
        .await;
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        // Killed rather than signalled: the suite asserts nothing about
        // shutdown, and a server mid-drain would hold the database open.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            eprintln!(
                "--- beam-server log ({}) ---\n{}",
                self.log_path.display(),
                tail(&self.log_path, 300)
            );
        }
    }
}

/// The last `lines` lines of the file at `path`.
fn tail(path: &Path, lines: usize) -> String {
    let Ok(file) = File::open(path) else {
        return format!("(no log at {})", path.display());
    };
    let all: Vec<String> = BufReader::new(file).lines().map_while(Result::ok).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// A port nothing is listening on right now.
///
/// The listener is dropped before the server binds, so another process could
/// take the port in between; the server then fails to start and says so,
/// which is a loud failure, never a wrong answer.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .expect("find a free port")
}
