use std::{
    fs::File,
    io::{self, BufReader, Read},
    ops::Deref,
    path::Path,
};

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::Xxh3;

/// XXH3 Hash
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct XXH3Hash(u64);

impl XXH3Hash {
    pub fn new(hash: u64) -> Self {
        Self(hash)
    }
}

impl Deref for XXH3Hash {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::fmt::Debug for XXH3Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "XXH3Hash({:016x})", self.0)
    }
}

/// Computes the hash of a file using XXH3 (64-bit).
pub fn compute_hash(path: &Path) -> io::Result<u64> {
    const BUFFER_SIZE: usize = 1024 * 1024; // 1 MB buffer

    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, file);
    let mut hasher = Xxh3::new();
    let mut buffer = vec![0; BUFFER_SIZE];

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break, // End of file
            Ok(bytes_read) => {
                hasher.update(&buffer[..bytes_read]);
            }
            Err(e) => return Err(e),
        }
    }

    Ok(hasher.digest())
}
