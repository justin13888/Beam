//! Simple and efficient file hashing utilities using XXH3.

use rayon::ThreadPool;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::utils::hash::compute_hash;

#[derive(Debug, Clone)]
pub struct HashConfig {
    pub num_threads: usize,
}

impl Default for HashConfig {
    fn default() -> Self {
        Self {
            num_threads: num_cpus::get_physical(),
        }
    }
}

/// A service that manages file hashing operations.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait::async_trait]
pub trait HashService: Send + Sync + std::fmt::Debug {
    fn hash_sync(&self, path: &Path) -> io::Result<u64>;
    async fn hash_async(&self, path: PathBuf) -> io::Result<u64>;
}

/// A service that manages file hashing operations using a dedicated Rayon thread pool.
#[derive(Debug, Clone)]
pub struct LocalHashService {
    thread_pool: Arc<ThreadPool>,
}

impl Default for LocalHashService {
    fn default() -> Self {
        Self::new(HashConfig::default())
    }
}

impl LocalHashService {
    pub fn new(config: HashConfig) -> Self {
        let num_threads = if config.num_threads > 0 {
            config.num_threads
        } else {
            num_cpus::get_physical()
        };

        let thread_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .thread_name(|idx| format!("hash-worker-{}", idx))
            .build()
            .expect("Failed to build hash service thread pool");

        tracing::info!("Initialized hash thread pool with {} threads", num_threads);

        Self {
            thread_pool: Arc::new(thread_pool),
        }
    }
}

#[async_trait::async_trait]
impl HashService for LocalHashService {
    fn hash_sync(&self, path: &Path) -> io::Result<u64> {
        let path = path.to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();

        self.thread_pool.spawn(move || {
            let result = compute_hash(&path);
            let _ = tx.send(result);
        });

        rx.recv().map_err(io::Error::other)?
    }

    async fn hash_async(&self, path: PathBuf) -> io::Result<u64> {
        let thread_pool = self.thread_pool.clone();

        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();

            thread_pool.spawn(move || {
                let result = compute_hash(&path);
                let _ = tx.send(result);
            });

            rx.recv().map_err(io::Error::other)?
        })
        .await
        .map_err(io::Error::other)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_hash_file_sync() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"Hello, World!").unwrap();
        temp_file.flush().unwrap();

        let service = LocalHashService::default();
        let hash = service.hash_sync(temp_file.path()).unwrap();
        assert!(hash > 0);
    }

    #[tokio::test]
    async fn test_hash_file_async() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"Hello, World!").unwrap();
        temp_file.flush().unwrap();

        let service = LocalHashService::default();
        let hash = service
            .hash_async(temp_file.path().to_path_buf())
            .await
            .unwrap();
        assert!(hash > 0);
    }

    #[tokio::test]
    async fn test_hash_consistency() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"Consistent data").unwrap();
        temp_file.flush().unwrap();

        let service = LocalHashService::default();
        let hash_sync = service.hash_sync(temp_file.path()).unwrap();
        let hash_async = service
            .hash_async(temp_file.path().to_path_buf())
            .await
            .unwrap();

        assert_eq!(hash_sync, hash_async);
    }
}
