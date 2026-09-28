//! Opening a file beneath a library root to read it (issues #186 and #189).
//!
//! Beam reads what a library holds -- an NFO while indexing, a video or a
//! subtitle file while serving it -- but a file there is not trusted. Between
//! the scan that recorded it and the read, it may have been replaced by a
//! symbolic link to a file outside the library (FR-212: Beam never follows
//! one), or by a FIFO or a device, which a read would wait on forever or
//! never finish. Every such read opens the file with [`open_regular_file`]
//! and reads from that handle alone, so what is checked is what is read.

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

/// Open `path` for reading, never through a symbolic link: on Unix with
/// `O_NOFOLLOW`, so a link swapped in after the caller's `lstat` fails to open
/// rather than being followed out of the library. The open is also
/// non-blocking, so a FIFO swapped in opens at once -- to be refused as not a
/// regular file -- rather than waiting forever for a writer; on a regular
/// file `O_NONBLOCK` changes nothing.
pub fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
        );
    }
    options.open(path)
}

/// Open the regular file at `path` for reading, with its metadata read from
/// the open handle: [`open_no_follow`], then refused unless the handle is a
/// regular file.
///
/// The metadata is the handle's own, so the length and modification time a
/// caller serves or bounds a read by belong to the bytes it reads -- not to
/// whatever a path named a moment earlier. A directory, a FIFO or a device
/// (`/dev/zero`, which stats as empty and never ends) fails with
/// [`io::ErrorKind::InvalidInput`].
pub fn open_regular_file(path: &Path) -> io::Result<(File, Metadata)> {
    let file = open_no_follow(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok((file, metadata))
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn a_regular_file_opens_with_its_own_metadata() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("movie.en.srt");
        std::fs::write(&path, b"subtitle bytes").unwrap();

        let (mut file, metadata) = open_regular_file(&path).unwrap();
        assert_eq!(metadata.len(), 14);
        let mut read = String::new();
        file.read_to_string(&mut read).unwrap();
        assert_eq!(read, "subtitle bytes");
    }

    /// The open itself refuses a link, not just an `lstat` before it: a link
    /// swapped in between the two fails to open rather than being followed
    /// out of the library.
    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_not_followed() {
        let dir = TempDir::new().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"secret").unwrap();
        let link = dir.path().join("movie.en.srt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        assert!(open_no_follow(&link).is_err());
        assert!(open_regular_file(&link).is_err());
    }

    #[test]
    fn a_directory_is_not_a_regular_file() {
        let dir = TempDir::new().unwrap();

        let err = open_regular_file(dir.path()).unwrap_err();
        // A directory may fail to open at all on some platforms; where it
        // opens, it is refused for what it is.
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::IsADirectory
            ),
            "{err:?}"
        );
    }

    /// A device stats as empty and reads forever: refused before a byte is
    /// read, so no bound on the read is what stops it.
    #[cfg(unix)]
    #[test]
    fn a_device_is_not_a_regular_file() {
        let err = open_regular_file(Path::new("/dev/zero")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// A FIFO swapped in for a file opens at once rather than blocking until
    /// something writes to it -- and is then refused as no regular file.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = TempDir::new().unwrap();
        let fifo = dir.path().join("movie.en.srt");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .unwrap();

        let (sent, opened) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent.send(
                open_regular_file(&fifo)
                    .map(|_| ())
                    .map_err(|err| err.kind()),
            );
        });
        let result = opened
            .recv_timeout(Duration::from_secs(10))
            .expect("the open returned instead of waiting for a writer");
        assert_eq!(result, Err(io::ErrorKind::InvalidInput));
    }
}
