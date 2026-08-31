//! A buffered [`Read`] + [`Seek`] view over a foreign [`ByteSource`].
//!
//! The Matroska parser walks an element tree with many small reads and
//! frequent short seeks. Passing each of those straight through the FFI
//! boundary to a ranged HTTP request would issue thousands of requests to read
//! one header, so this holds a single sliding window and serves reads from it.
//!
//! The window is deliberately simple -- one contiguous range, refilled
//! whenever a read falls outside it. A more elaborate cache would have to
//! decide what to evict, and the access pattern that matters (a linear sweep
//! through clusters, punctuated by a seek) is served exactly as well by one
//! window.

use crate::error::ByteSourceError;
use crate::ports::byte_source::ByteSource;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

/// The last failure a [`ByteSourceReader`] saw, shared with whoever is driving
/// it.
///
/// `Read` can only report a `std::io::Error`, so once a byte-source failure has
/// been through that keyhole it is indistinguishable from the unexpected-EOF a
/// parser produces when it runs off the end of a *corrupt* file. Those two need
/// different answers -- one is worth retrying and the other never is -- so the
/// reader records which one actually happened instead of leaving the caller to
/// guess from an error string.
pub type FaultLog = Arc<Mutex<Option<ByteSourceError>>>;

/// How many bytes to fetch on a miss.
///
/// Large enough that a cluster's worth of frames is usually one fetch, small
/// enough that a seek does not pull a megabyte that is immediately discarded.
const WINDOW_BYTES: u32 = 128 * 1024;

/// A seekable reader over bytes the platform fetches.
#[derive(Debug)]
pub struct ByteSourceReader {
    source: Arc<dyn ByteSource>,
    len: u64,
    position: u64,
    window: Vec<u8>,
    window_start: u64,
    fault: FaultLog,
}

impl ByteSourceReader {
    /// Wrap `source`, positioned at its start.
    #[must_use]
    pub fn new(source: Arc<dyn ByteSource>) -> Self {
        let len = source.len();
        Self {
            source,
            len,
            position: 0,
            window: Vec::new(),
            window_start: 0,
            fault: FaultLog::default(),
        }
    }

    /// A handle on this reader's fault log.
    ///
    /// Taken before the reader is moved into a parser that offers no way back
    /// to it.
    #[must_use]
    pub fn fault_log(&self) -> FaultLog {
        Arc::clone(&self.fault)
    }

    /// The total size of the underlying source.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the source holds no bytes at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether `self.position` currently falls inside the loaded window.
    fn window_contains_position(&self) -> bool {
        let end = self.window_start.saturating_add(self.window.len() as u64);
        self.position >= self.window_start && self.position < end
    }

    /// Fetch a window starting at the current position.
    fn refill(&mut self) -> Result<(), ByteSourceError> {
        // Never ask for more than remains: `read_at` treats a short read as an
        // error, so a request that overruns the end would fail rather than
        // return the tail.
        let remaining = self.len.saturating_sub(self.position);
        let wanted = u32::try_from(remaining.min(u64::from(WINDOW_BYTES))).unwrap_or(WINDOW_BYTES);
        if wanted == 0 {
            self.window.clear();
            self.window_start = self.position;
            return Ok(());
        }
        match self.source.read_at(self.position, wanted) {
            Ok(bytes) => {
                self.window = bytes;
                self.window_start = self.position;
                Ok(())
            }
            Err(error) => {
                *self
                    .fault
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.clone());
                Err(error)
            }
        }
    }
}

impl Read for ByteSourceReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() || self.position >= self.len {
            return Ok(0);
        }

        if !self.window_contains_position() {
            self.refill().map_err(std::io::Error::other)?;
        }

        // A source that returned fewer bytes than asked for would leave the
        // window short; treating that as EOF rather than looping forever is
        // the only safe reading of it.
        let offset = usize::try_from(self.position - self.window_start)
            .map_err(|_| std::io::Error::other("window offset exceeds usize"))?;
        let Some(available) = self.window.get(offset..) else {
            return Ok(0);
        };
        if available.is_empty() {
            return Ok(0);
        }

        let count = available.len().min(out.len());
        out[..count].copy_from_slice(&available[..count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for ByteSourceReader {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        // Seeking past the end is legal and must not error: the parser probes
        // beyond the last element to detect EOF, and an error there would be
        // reported as a malformed container.
        let target = match from {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(delta) => i128::from(self.len) + i128::from(delta),
            SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
        };

        if target < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek to a negative position",
            ));
        }

        self.position = u64::try_from(target)
            .map_err(|_| std::io::Error::other("seek beyond the addressable range"))?;
        Ok(self.position)
    }

    fn stream_position(&mut self) -> std::io::Result<u64> {
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::byte_source::InMemoryByteSource;

    fn reader_over(bytes: Vec<u8>) -> ByteSourceReader {
        ByteSourceReader::new(Arc::new(InMemoryByteSource::new(bytes)))
    }

    #[test]
    fn a_linear_read_returns_every_byte_in_order() {
        let bytes: Vec<u8> = (0..=u8::MAX).collect();
        let mut reader = reader_over(bytes.clone());

        let mut read_back = Vec::new();
        reader.read_to_end(&mut read_back).expect("read failed");

        assert_eq!(read_back, bytes);
    }

    #[test]
    fn a_read_after_a_seek_starts_at_the_seeked_byte() {
        let bytes: Vec<u8> = (0..100).collect();
        let mut reader = reader_over(bytes);

        reader.seek(SeekFrom::Start(40)).expect("seek failed");
        let mut buf = [0_u8; 4];
        reader.read_exact(&mut buf).expect("read failed");

        assert_eq!(buf, [40, 41, 42, 43]);
    }

    #[test]
    fn seek_from_end_and_current_resolve_against_the_right_origin() {
        let mut reader = reader_over((0..100).collect());

        assert_eq!(reader.seek(SeekFrom::End(-10)).expect("seek failed"), 90);
        assert_eq!(reader.seek(SeekFrom::Current(-5)).expect("seek failed"), 85);
        assert_eq!(reader.seek(SeekFrom::Current(10)).expect("seek failed"), 95);
    }

    #[test]
    fn reading_at_or_past_the_end_reports_eof_rather_than_failing() {
        // The parser probes past the last element to find EOF. If that came
        // back as an error it would be reported as a malformed container --
        // which is a user-visible lie about the file.
        let mut reader = reader_over((0..10).collect());

        reader.seek(SeekFrom::Start(10)).expect("seek failed");
        assert_eq!(reader.read(&mut [0_u8; 4]).expect("read failed"), 0);

        reader.seek(SeekFrom::Start(9_999)).expect("seek failed");
        assert_eq!(reader.read(&mut [0_u8; 4]).expect("read failed"), 0);
    }

    #[test]
    fn seeking_before_the_start_is_an_error_rather_than_a_clamp() {
        let mut reader = reader_over((0..10).collect());

        let error = reader
            .seek(SeekFrom::Current(-1))
            .expect_err("expected refusal");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_window_serves_many_reads_from_one_fetch() {
        // The reason this type exists: without coalescing, one ranged HTTP
        // request per element would make opening a file take thousands.
        let source = Arc::new(InMemoryByteSource::new(vec![7_u8; 4096]));
        let mut reader = ByteSourceReader::new(source.clone());

        for _ in 0..512 {
            let mut byte = [0_u8; 1];
            reader.read_exact(&mut byte).expect("read failed");
        }

        assert_eq!(source.read_count(), 1);
    }

    #[test]
    fn an_empty_source_reads_as_eof_immediately() {
        let mut reader = reader_over(Vec::new());

        assert!(reader.is_empty());
        assert_eq!(reader.read(&mut [0_u8; 4]).expect("read failed"), 0);
    }

    #[test]
    fn a_source_failure_surfaces_as_an_io_error_carrying_its_reason() {
        // The extractor turns this back into ExtractorError::Source, so the
        // detail has to survive the round trip through io::Error.
        let source = Arc::new(InMemoryByteSource::failing_after(vec![0_u8; 4096], 0));
        let mut reader = ByteSourceReader::new(source);

        let error = reader.read(&mut [0_u8; 16]).expect_err("expected failure");

        assert!(
            error.to_string().contains("configured to fail"),
            "reason was lost: {error}"
        );
    }
}
