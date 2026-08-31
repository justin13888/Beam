//! Random-access byte reads, implemented by the foreign side.
//!
//! The core demuxes containers ([`crate::demux`]) but never fetches their
//! bytes. That split is the same one [`crate::ffi::PlaybackHttpConfig`] draws
//! for the platform player, and for the same reason: the platform already has
//! a tuned HTTP stack with connection reuse, background transfer and the
//! user's trust decisions wired in, and routing a whole media file through the
//! FFI boundary would buy nothing but copies. Apple supplies one
//! implementation over `URLSession` ranged requests and another over a local
//! file, and the extractor cannot tell them apart.
//!
//! Deliberately **synchronous**. The Matroska parser pulls bytes as it walks
//! the element tree, so an async boundary here would mean either blocking on a
//! future inside a sync parser or rewriting the parser; the extractor is
//! already required to run off the caller's main thread, which is where a
//! blocking read belongs anyway.

use crate::error::ByteSourceError;

/// Random-access reads over some sequence of bytes the platform owns.
#[uniffi::export(with_foreign)]
pub trait ByteSource: Send + Sync + std::fmt::Debug {
    /// The total number of bytes available.
    ///
    /// Must not change across the life of the source. A stream whose length is
    /// unknown cannot back a seekable demuxer and should not be offered here.
    fn len(&self) -> u64;

    /// Read exactly `length` bytes starting at `offset`.
    ///
    /// A short read is an error, not a truncated success: the parser treats
    /// the returned slice as the bytes it asked for, and silently returning
    /// fewer would be read as a malformed container rather than as a failed
    /// fetch.
    fn read_at(&self, offset: u64, length: u32) -> Result<Vec<u8>, ByteSourceError>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
mod in_memory {
    use super::{ByteSource, ByteSourceError};
    use std::sync::Mutex;

    /// A [`ByteSource`] over a `Vec<u8>` already in memory.
    ///
    /// Real bytes, so the reader and the extractor are exercised against the
    /// same content a device would fetch. `fail_after` exists so the "the
    /// network died mid-file" branch is reachable from a test rather than only
    /// from a train tunnel.
    #[derive(Debug)]
    pub struct InMemoryByteSource {
        bytes: Vec<u8>,
        reads: Mutex<ReadLog>,
    }

    #[derive(Debug, Default)]
    struct ReadLog {
        count: u64,
        fail_after: Option<u64>,
    }

    impl InMemoryByteSource {
        /// A source over `bytes` that never fails.
        #[must_use]
        pub fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                reads: Mutex::new(ReadLog::default()),
            }
        }

        /// Fail every read after the first `count` have succeeded.
        #[must_use]
        pub fn failing_after(bytes: Vec<u8>, count: u64) -> Self {
            Self {
                bytes,
                reads: Mutex::new(ReadLog {
                    count: 0,
                    fail_after: Some(count),
                }),
            }
        }

        /// How many reads have been served.
        ///
        /// Lets a test assert that the buffered reader actually coalesces,
        /// rather than assuming it from the code.
        #[must_use]
        pub fn read_count(&self) -> u64 {
            self.reads.lock().expect("read log poisoned").count
        }
    }

    impl ByteSource for InMemoryByteSource {
        fn len(&self) -> u64 {
            self.bytes.len() as u64
        }

        fn read_at(&self, offset: u64, length: u32) -> Result<Vec<u8>, ByteSourceError> {
            {
                let mut log = self.reads.lock().expect("read log poisoned");
                log.count += 1;
                if let Some(limit) = log.fail_after
                    && log.count > limit
                {
                    return Err(ByteSourceError::Unavailable {
                        detail: "configured to fail".to_owned(),
                    });
                }
            }

            let length = u64::from(length);
            let end = offset
                .checked_add(length)
                .ok_or(ByteSourceError::OutOfBounds { offset, length })?;
            if end > self.len() {
                return Err(ByteSourceError::OutOfBounds { offset, length });
            }

            let start = usize::try_from(offset)
                .map_err(|_| ByteSourceError::OutOfBounds { offset, length })?;
            let end = usize::try_from(end)
                .map_err(|_| ByteSourceError::OutOfBounds { offset, length })?;
            Ok(self.bytes[start..end].to_vec())
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::InMemoryByteSource;
