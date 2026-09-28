//! Simple and efficient file hashing utilities using XXH3.

use rayon::ThreadPool;
use std::fs::File;
use std::io::{self, Seek};
use std::sync::Arc;

use beam_domain::utils::hash::compute_hash;

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
///
/// It hashes an open handle, never a path: the indexer opens a library file
/// beneath its root with no link followed
/// ([`LibraryFile`](crate::library_file::LibraryFile), issue #238) and hands
/// the service that handle, so what is hashed is the file that was opened.
/// The whole file is hashed, whatever the handle's offset.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait::async_trait]
pub trait HashService: Send + Sync + std::fmt::Debug {
    fn hash_sync(&self, file: File) -> io::Result<u64>;
    async fn hash_async(&self, file: File) -> io::Result<u64>;
}

/// The hash of all of `file`, from its start: a handle shares its offset
/// with every handle cloned from it, so it may not be at the start.
fn hash_whole(mut file: File) -> io::Result<u64> {
    file.rewind()?;
    compute_hash(file)
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
    fn hash_sync(&self, file: File) -> io::Result<u64> {
        let (tx, rx) = std::sync::mpsc::channel();

        self.thread_pool.spawn(move || {
            let result = hash_whole(file);
            let _ = tx.send(result);
        });

        rx.recv().map_err(io::Error::other)?
    }

    async fn hash_async(&self, file: File) -> io::Result<u64> {
        let thread_pool = self.thread_pool.clone();

        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();

            thread_pool.spawn(move || {
                let result = hash_whole(file);
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
    use std::io::{Read, Write};
    use tempfile::NamedTempFile;

    fn file_containing(bytes: &[u8]) -> NamedTempFile {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(bytes).unwrap();
        temp_file.flush().unwrap();
        temp_file
    }

    /// The digest a file's content had when the indexer still hashed by
    /// path, before issue #238: a row recorded then must still match the
    /// same content hashed from a handle now, or every file would read as
    /// changed -- and be rehashed and reprobed -- on the first scan after
    /// an upgrade.
    #[tokio::test]
    async fn a_handle_hashes_to_the_digest_a_path_did() {
        for (content, before) in [
            (&b""[..], 0x2d06_8005_38d3_94c2_u64),
            (
                b"Beam #238: hashed through the no-follow opener",
                0x7894_a6e4_cdb2_5465,
            ),
        ] {
            let temp_file = file_containing(content);
            let service = LocalHashService::default();
            let file = File::open(temp_file.path()).unwrap();
            assert_eq!(service.hash_async(file).await.unwrap(), before);
            let file = File::open(temp_file.path()).unwrap();
            assert_eq!(service.hash_sync(file).unwrap(), before);
        }
    }

    /// A handle read part-way -- one sharing its offset with another that
    /// read -- still hashes the whole file.
    #[tokio::test]
    async fn a_handle_is_hashed_from_its_start() {
        let temp_file = file_containing(b"Consistent data");
        let service = LocalHashService::default();
        let whole = service
            .hash_async(File::open(temp_file.path()).unwrap())
            .await
            .unwrap();

        let mut read_part_way = File::open(temp_file.path()).unwrap();
        let mut head = [0u8; 4];
        read_part_way.read_exact(&mut head).unwrap();
        assert_eq!(service.hash_async(read_part_way).await.unwrap(), whole);
    }
}
