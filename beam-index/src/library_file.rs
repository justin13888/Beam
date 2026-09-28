//! Opening a file beneath a library root to read it (issues #186 and #189).
//!
//! Beam reads what a library holds -- an NFO while indexing, a video or a
//! subtitle file while serving it -- but a file there is not trusted. Between
//! the scan that recorded it and the read, it may have been replaced by a
//! symbolic link to a file outside the library, or a folder above it may
//! have been (FR-212: Beam follows no symbolic link beneath a library root);
//! or it may have become a FIFO or a device, which a read would wait on
//! forever or never finish. Every such read opens the file with
//! [`open_regular_file`] and reads from that handle alone, so what is checked
//! is what is read.
//!
//! The root itself is opened as named: it is the administrator's choice of
//! folder, and a root that is itself a link is followed like any path they
//! configure. Everything beneath it is resolved one component at a time with
//! no link followed anywhere -- on Linux by the kernel (`openat2` with
//! `RESOLVE_NO_SYMLINKS | RESOLVE_BENEATH`), elsewhere on Unix by a walk of
//! `openat` calls from the root, each refusing a link.

use std::fs::{File, Metadata};
use std::io;
use std::path::{Component, Path, PathBuf};

/// `path` relative to the library `root` it was indexed under: what
/// [`open_regular_file`] takes. A path not beneath `root` fails with
/// [`io::ErrorKind::InvalidInput`] -- it is no file of that library.
pub fn relative_to<'a>(root: &Path, path: &'a Path) -> io::Result<&'a Path> {
    path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not beneath {}", path.display(), root.display()),
        )
    })
}

/// Open the regular file at `relative` beneath the library `root` for
/// reading, with its metadata read from the open handle.
///
/// No symbolic link is followed beneath `root` -- neither the file nor any
/// folder between it and the root -- so a link swapped in anywhere on the
/// path since the scan fails to open rather than being followed out of the
/// library. `relative` must name a file beneath the root: an absolute path,
/// a `..` or an empty path fails with [`io::ErrorKind::InvalidInput`]. The
/// open is non-blocking, so a FIFO swapped in opens at once -- to be refused
/// -- rather than waiting forever for a writer; on a regular file
/// `O_NONBLOCK` changes nothing.
///
/// The metadata is the handle's own, so the length and modification time a
/// caller serves or bounds a read by belong to the bytes it reads -- not to
/// whatever a path named a moment earlier. A directory, a FIFO or a device
/// (`/dev/zero`, which stats as empty and never ends) fails with
/// [`io::ErrorKind::InvalidInput`].
pub fn open_regular_file(root: &Path, relative: &Path) -> io::Result<(File, Metadata)> {
    let relative = checked_relative(relative)?;
    let file = open_beneath(root, &relative)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", relative.display()),
        ));
    }
    Ok((file, metadata))
}

/// `relative` as plain names, one per component: refused unless every
/// component is a name (`.` is dropped), and unless there is at least one.
fn checked_relative(relative: &Path) -> io::Result<PathBuf> {
    let mut names = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => names.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} does not name a path beneath a root", relative.display()),
                ));
            }
        }
    }
    if names.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an empty path names no file beneath a root",
        ));
    }
    Ok(names)
}

/// Open `relative` -- plain names only -- beneath `root`, following no link
/// beneath it.
///
/// On Linux the kernel resolves the path (`openat2`); where the kernel is too
/// old to have it (before 5.6), or a seccomp policy refuses it with `EPERM`,
/// the portable walk does the same job.
#[cfg(target_os = "linux")]
fn open_beneath(root: &Path, relative: &Path) -> io::Result<File> {
    let root = open_root(root)?;
    match open_beneath_by_kernel(&root, relative) {
        Err(err)
            if err.raw_os_error() == Some(rustix::io::Errno::NOSYS.raw_os_error())
                || err.raw_os_error() == Some(rustix::io::Errno::PERM.raw_os_error()) =>
        {
            open_beneath_by_walk(root, relative)
        }
        opened => opened,
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn open_beneath(root: &Path, relative: &Path) -> io::Result<File> {
    open_beneath_by_walk(open_root(root)?, relative)
}

/// Unix is the only platform Beam serves from (a first-class platform is
/// Linux or macOS); elsewhere the path is opened as named.
#[cfg(not(unix))]
fn open_beneath(root: &Path, relative: &Path) -> io::Result<File> {
    File::open(root.join(relative))
}

/// The root, opened as a directory as the administrator named it.
#[cfg(unix)]
fn open_root(root: &Path) -> io::Result<rustix::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    Ok(rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

/// The flags the file itself is opened with, whichever way the path is
/// resolved.
#[cfg(unix)]
fn leaf_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}

/// `openat2` from the root: `RESOLVE_NO_SYMLINKS` refuses a link at any
/// component, the file included, and `RESOLVE_BENEATH` any path that would
/// resolve outside the root.
#[cfg(target_os = "linux")]
fn open_beneath_by_kernel(root: &rustix::fd::OwnedFd, relative: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, ResolveFlags};
    let fd = rustix::fs::openat2(
        root,
        relative,
        leaf_flags(),
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS | ResolveFlags::BENEATH,
    )?;
    Ok(File::from(fd))
}

/// A walk from the root: each folder opened with `O_NOFOLLOW | O_DIRECTORY`
/// relative to the one before, so a link at any of them fails to open, then
/// the file with [`leaf_flags`]. `relative` holds plain names only, so the
/// walk can never climb above the root.
#[cfg(unix)]
fn open_beneath_by_walk(root: rustix::fd::OwnedFd, relative: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    let names: Vec<_> = relative.components().collect();
    let Some((leaf, folders)) = names.split_last() else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    };
    let mut dir = root;
    for folder in folders {
        dir = rustix::fs::openat(
            &dir,
            folder.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
    }
    let fd = rustix::fs::openat(&dir, leaf.as_os_str(), leaf_flags(), Mode::empty())?;
    Ok(File::from(fd))
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    /// Each way a path beneath a root is resolved on this platform: every
    /// case below holds for all of them.
    #[allow(clippy::type_complexity)]
    fn resolvers() -> Vec<(&'static str, fn(&Path, &Path) -> io::Result<File>)> {
        let mut resolvers: Vec<(&'static str, fn(&Path, &Path) -> io::Result<File>)> =
            vec![("platform", open_beneath)];
        #[cfg(unix)]
        resolvers.push(("walk", |root, relative| {
            open_beneath_by_walk(open_root(root)?, relative)
        }));
        #[cfg(target_os = "linux")]
        resolvers.push(("kernel", |root, relative| {
            open_beneath_by_kernel(&open_root(root)?, relative)
        }));
        resolvers
    }

    fn read(mut file: File) -> String {
        let mut read = String::new();
        file.read_to_string(&mut read).unwrap();
        read
    }

    #[test]
    fn a_regular_file_opens_with_its_own_metadata() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("movie.en.srt"), b"subtitle bytes").unwrap();

        let (file, metadata) = open_regular_file(dir.path(), Path::new("movie.en.srt")).unwrap();
        assert_eq!(metadata.len(), 14);
        assert_eq!(read(file), "subtitle bytes");
    }

    /// A file folders deep is reached through each of them, by every
    /// resolver -- and it is that file, not another of its name.
    #[test]
    fn a_nested_file_opens_through_its_folders() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("Show/Season 01")).unwrap();
        std::fs::write(dir.path().join("Show/Season 01/E01.mkv"), b"nested").unwrap();
        std::fs::write(dir.path().join("E01.mkv"), b"top level").unwrap();

        for (resolver, open) in resolvers() {
            let file = open(dir.path(), Path::new("Show/Season 01/E01.mkv"))
                .unwrap_or_else(|err| panic!("{resolver}: {err}"));
            assert_eq!(read(file), "nested", "{resolver}");
        }
    }

    #[test]
    fn a_path_is_taken_relative_to_its_root() {
        let root = Path::new("/media/films");
        assert_eq!(
            relative_to(root, Path::new("/media/films/A/a.mkv")).unwrap(),
            Path::new("A/a.mkv")
        );
        assert_eq!(
            relative_to(root, Path::new("/media/filmsX/a.mkv"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    /// Nothing but plain names reaches a resolver: a path that could name
    /// something outside the root, or nothing, is refused before any open.
    #[test]
    fn only_a_path_of_names_is_opened() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("inner")).unwrap();
        std::fs::write(dir.path().join("a.mkv"), b"a").unwrap();

        // Each of these names an existing file, or the root itself, by a
        // path that is not plain names.
        for (root, relative) in [
            (dir.path().join("inner"), "../a.mkv"),
            (dir.path().to_path_buf(), "inner/../a.mkv"),
            (PathBuf::from("/"), "/etc/hostname"),
            (dir.path().to_path_buf(), ""),
            (dir.path().to_path_buf(), "."),
        ] {
            let err = open_regular_file(&root, Path::new(relative))
                .map(|_| ())
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{relative:?}");
        }
        // `.` is no step at all, so it is dropped rather than refused.
        let (file, _) = open_regular_file(dir.path(), Path::new("./a.mkv")).unwrap();
        assert_eq!(read(file), "a");
    }

    /// The open itself refuses a link, not just an `lstat` before it: a link
    /// swapped in for the file fails to open rather than being followed out
    /// of the library.
    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_not_followed() {
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("movie.en.srt"), b"secret").unwrap();
        let root = TempDir::new().unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("movie.en.srt"),
            root.path().join("movie.en.srt"),
        )
        .unwrap();

        for (resolver, open) in resolvers() {
            assert!(
                open(root.path(), Path::new("movie.en.srt")).is_err(),
                "{resolver}"
            );
        }
        assert!(open_regular_file(root.path(), Path::new("movie.en.srt")).is_err());
    }

    /// A folder above the file swapped for a link to a folder outside the
    /// library holding a file of the same name: the link is refused, at the
    /// first level and deeper, rather than serving the outside file.
    #[cfg(unix)]
    #[test]
    fn a_folder_above_the_file_swapped_for_a_link_is_not_followed() {
        let outside = TempDir::new().unwrap();
        std::fs::create_dir(outside.path().join("Season 01")).unwrap();
        std::fs::write(outside.path().join("Movie.en.srt"), b"OUTSIDE SECRET").unwrap();
        std::fs::write(outside.path().join("Season 01/E01.mkv"), b"OUTSIDE SECRET").unwrap();
        let root = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("sub")).unwrap();
        std::fs::create_dir(root.path().join("Show")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("Show/Season 01")).unwrap();

        for relative in ["sub/Movie.en.srt", "Show/Season 01/E01.mkv"] {
            for (resolver, open) in resolvers() {
                assert!(
                    open(root.path(), Path::new(relative)).is_err(),
                    "{resolver}: {relative}"
                );
            }
        }
    }

    /// The root is the administrator's to name: one that is itself a link is
    /// followed, and only what lies beneath it is held to no links.
    #[cfg(unix)]
    #[test]
    fn a_root_that_is_itself_a_link_is_followed() {
        let real = TempDir::new().unwrap();
        std::fs::create_dir(real.path().join("A")).unwrap();
        std::fs::write(real.path().join("A/a.mkv"), b"inside").unwrap();
        let named = TempDir::new().unwrap();
        let root = named.path().join("library");
        std::os::unix::fs::symlink(real.path(), &root).unwrap();

        for (resolver, open) in resolvers() {
            let file =
                open(&root, Path::new("A/a.mkv")).unwrap_or_else(|err| panic!("{resolver}: {err}"));
            assert_eq!(read(file), "inside", "{resolver}");
        }
    }

    #[test]
    fn a_directory_is_not_a_regular_file() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("Extras")).unwrap();

        let err = open_regular_file(dir.path(), Path::new("Extras")).unwrap_err();
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
        let err = open_regular_file(Path::new("/dev"), Path::new("zero")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// A FIFO swapped in for a file opens at once rather than blocking until
    /// something writes to it -- and is then refused as no regular file.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = TempDir::new().unwrap();
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            dir.path().join("movie.en.srt"),
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .unwrap();

        let root = dir.path().to_path_buf();
        let (sent, opened) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent.send(
                open_regular_file(&root, Path::new("movie.en.srt"))
                    .map(|_| ())
                    .map_err(|err| err.kind()),
            );
        });
        let result = opened
            .recv_timeout(Duration::from_secs(10))
            .expect("the open returned instead of waiting for a writer");
        assert_eq!(result, Err(io::ErrorKind::InvalidInput));
        drop(dir);
    }
}
