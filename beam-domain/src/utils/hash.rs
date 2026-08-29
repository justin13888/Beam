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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// `compute_hash` reads a real file, and a `TempDir` is a real filesystem
    /// that needs no infrastructure -- so the subject here is the function
    /// against actual bytes rather than a `FileSystem` double, which would only
    /// prove the double returns what it was configured to return.
    fn file_containing(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("input.bin");
        let mut file = File::create(&path).expect("create");
        file.write_all(bytes).expect("write");
        file.sync_all().expect("sync");
        (dir, path)
    }

    /// The digest of `bytes` via the library's one-shot API.
    ///
    /// The subject of these tests is the buffered read loop above -- the only
    /// part of this module that is our code. Comparing it against the one-shot
    /// digest of the same bytes constrains the loop (chunk slicing, the final
    /// partial read, the end-of-file exit) without restating a constant, and
    /// without re-testing XXH3 itself.
    fn expected(bytes: &[u8]) -> u64 {
        xxhash_rust::xxh3::xxh3_64(bytes)
    }

    #[test]
    fn hashes_an_empty_file() {
        let (_dir, path) = file_containing(b"");
        assert_eq!(compute_hash(&path).unwrap(), expected(b""));
    }

    #[test]
    fn hashes_a_small_file() {
        let bytes = b"the quick brown fox jumps over the lazy dog";
        let (_dir, path) = file_containing(bytes);
        assert_eq!(compute_hash(&path).unwrap(), expected(bytes));
    }

    #[test]
    fn hashes_a_file_larger_than_the_read_buffer() {
        // The read loop buffers 1 MiB at a time; a file spanning several
        // buffers is the case where a mis-sliced `buffer[..bytes_read]` or a
        // dropped final chunk would show up.
        let bytes: Vec<u8> = (0..(3 * 1024 * 1024 + 7))
            .map(|i| (i % 251) as u8)
            .collect();
        let (_dir, path) = file_containing(&bytes);
        assert_eq!(compute_hash(&path).unwrap(), expected(&bytes));
    }

    #[test]
    fn the_same_content_hashes_the_same_from_two_different_paths() {
        let bytes = b"identical content";
        let (_a, path_a) = file_containing(bytes);
        let (_b, path_b) = file_containing(bytes);
        assert_eq!(
            compute_hash(&path_a).unwrap(),
            compute_hash(&path_b).unwrap(),
            "the hash identifies content, not location -- deduplication depends on it"
        );
    }

    #[test]
    fn a_single_flipped_byte_changes_the_hash() {
        let mut bytes = vec![0u8; 4096];
        let (_a, path_a) = file_containing(&bytes);
        bytes[2048] = 1;
        let (_b, path_b) = file_containing(&bytes);
        assert_ne!(
            compute_hash(&path_a).unwrap(),
            compute_hash(&path_b).unwrap()
        );
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_hash_of_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let err = compute_hash(&dir.path().join("absent.bin"))
            .expect_err("hashing a file that does not exist must fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_directory_is_an_error_not_a_hash_of_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            compute_hash(dir.path()).is_err(),
            "a directory is not hashable content"
        );
    }

    #[test]
    fn the_debug_format_is_a_fixed_width_hex_digest() {
        // Padded to 16 digits: the digest is used in log lines and admin
        // output, where a variable-width value is hard to scan.
        assert_eq!(
            format!("{:?}", XXH3Hash::new(0xff)),
            "XXH3Hash(00000000000000ff)"
        );
        assert_eq!(
            format!("{:?}", XXH3Hash::new(u64::MAX)),
            "XXH3Hash(ffffffffffffffff)"
        );
    }

    #[test]
    fn the_wrapper_derefs_to_the_digest_it_wraps() {
        let hash = XXH3Hash::new(0x0123_4567_89ab_cdef);
        assert_eq!(*hash, 0x0123_4567_89ab_cdef);
    }
}
