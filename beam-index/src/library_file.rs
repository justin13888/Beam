//! Opening a file beneath a library root to read it (issues #186, #189 and
//! #238).
//!
//! Beam reads what a library holds -- an NFO, and a video's stat, hash and
//! probe, while indexing; a video or a subtitle file while serving it -- but
//! a file there is not trusted. Between the walk or scan that found it and
//! the read, it may have been replaced by a
//! symbolic link to a file outside the library, or a folder above it may
//! have been (FR-212: Beam follows no symbolic link beneath a library root);
//! or it may have become a FIFO or a device, which a read would wait on
//! forever or never finish. Every such read opens the file with
//! [`open_regular_file`] and reads from that handle alone, so what is checked
//! is what is read.
//!
//! A file that is only stat'ed -- the walk's stat of every entry, the stat a
//! scan compares with a row -- is not opened at all: [`StatCursor`] resolves
//! its folder beneath the root with no link followed and stats it with
//! `fstatat` and `AT_SYMLINK_NOFOLLOW`, so that reads nothing through a link
//! either. Only a file whose bytes are read is opened.
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
use std::time::SystemTime;
#[cfg(unix)]
use std::time::{Duration, UNIX_EPOCH};

/// Why the opener refused a path: no regular file of the library is there.
/// Carried inside the [`io::Error`] it returns, so [`is_refusal`] tells the
/// opener's own refusals from an error a kernel or a filesystem returned
/// with the same [`io::ErrorKind`].
#[derive(Debug)]
struct NotLibraryFile(String);

impl std::fmt::Display for NotLibraryFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotLibraryFile {}

/// A refusal: [`io::ErrorKind::InvalidInput`], marked as the opener's own.
fn not_library_file(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, NotLibraryFile(why.into()))
}

/// `path` relative to the library `root` it was indexed under: what
/// [`open_regular_file`] takes. A path not beneath `root` fails with
/// [`io::ErrorKind::InvalidInput`] -- it is no file of that library.
pub fn relative_to<'a>(root: &Path, path: &'a Path) -> io::Result<&'a Path> {
    path.strip_prefix(root).map_err(|_| {
        not_library_file(format!(
            "{} is not beneath {}",
            path.display(),
            root.display()
        ))
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
        return Err(not_library_file(format!(
            "{} is not a regular file",
            relative.display()
        )));
    }
    Ok((file, metadata))
}

/// What a stat of a regular file of a library says: its size, and its
/// modification and change times and inode number where the platform has
/// them. Read from an open handle (`From<&Metadata>`) or, without opening
/// the file, by a [`StatCursor`] -- the same fields either way, so a stat of
/// one kind compares with a stat of the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    size: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    mtime: (i64, u32),
    #[cfg(unix)]
    ctime: (i64, u32),
    #[cfg(not(unix))]
    modified: Option<SystemTime>,
}

impl FileMeta {
    /// The file's size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The file's modification time, as [`Metadata::modified`] reads it.
    pub fn modified(&self) -> Option<SystemTime> {
        #[cfg(unix)]
        {
            let (secs, nanos) = self.mtime;
            system_time(secs, nanos)
        }
        #[cfg(not(unix))]
        {
            self.modified
        }
    }

    /// The file's inode number.
    #[cfg(unix)]
    pub fn ino(&self) -> u64 {
        self.ino
    }

    /// The file's modification time: seconds and nanoseconds since the epoch.
    #[cfg(unix)]
    pub fn mtime(&self) -> (i64, u32) {
        self.mtime
    }

    /// The file's change time: seconds and nanoseconds since the epoch.
    #[cfg(unix)]
    pub fn ctime(&self) -> (i64, u32) {
        self.ctime
    }

    /// What a `stat` of a regular file returned.
    #[cfg(unix)]
    fn of_stat(stat: &rustix::fs::Stat) -> Self {
        // `From`, not `as`: the field types differ between targets.
        #[allow(clippy::useless_conversion)]
        let (size, ino, mtime, mtime_nsec, ctime, ctime_nsec) = (
            i64::from(stat.st_size),
            u64::from(stat.st_ino),
            i64::from(stat.st_mtime),
            u64::from(stat.st_mtime_nsec),
            i64::from(stat.st_ctime),
            u64::from(stat.st_ctime_nsec),
        );
        FileMeta {
            size: u64::try_from(size).unwrap_or(0),
            ino,
            mtime: (mtime, nanos(mtime_nsec)),
            ctime: (ctime, nanos(ctime_nsec)),
        }
    }
}

impl From<&Metadata> for FileMeta {
    fn from(meta: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            FileMeta {
                size: meta.len(),
                ino: meta.ino(),
                mtime: (meta.mtime(), nanos(meta.mtime_nsec())),
                ctime: (meta.ctime(), nanos(meta.ctime_nsec())),
            }
        }
        #[cfg(not(unix))]
        {
            FileMeta {
                size: meta.len(),
                modified: meta.modified().ok(),
            }
        }
    }
}

/// A stat's nanosecond field, which is below a second on any sane
/// filesystem; one out of range is clamped into it, as the indexer always
/// read it.
#[cfg(unix)]
fn nanos(nanos: impl Into<i128>) -> u32 {
    // In range after the clamp, so the cast cannot truncate.
    nanos.into().clamp(0, 999_999_999) as u32
}

/// The instant `secs` and `nanos` after the epoch, as the standard library
/// reads a stat's times; `None` where it cannot be represented.
#[cfg(unix)]
fn system_time(secs: i64, nanos: u32) -> Option<SystemTime> {
    let whole = Duration::from_secs(secs.unsigned_abs());
    let base = if secs >= 0 {
        UNIX_EPOCH.checked_add(whole)
    } else {
        UNIX_EPOCH.checked_sub(whole)
    }?;
    base.checked_add(Duration::from_nanos(u64::from(nanos)))
}

/// Stat the regular file at `path`, beneath the library `root`, without
/// opening it and with no link followed: a [`StatCursor`] used once.
pub fn stat_regular_file(root: &Path, path: &Path) -> io::Result<FileMeta> {
    StatCursor::new(root).stat(path)
}

/// Stats files beneath one library root without opening them, following no
/// link anywhere beneath the root (FR-212, issue #238).
///
/// Each folder between the root and a file is opened relative to the one
/// above it with `O_NOFOLLOW | O_DIRECTORY`, so a folder that is a link
/// fails to open, and the file itself is stat'ed relative to its folder with
/// `AT_SYMLINK_NOFOLLOW` and refused unless it is a regular file. The
/// folders a stat opened are kept for the next: a walk, which stats a
/// folder's files one after another, opens each folder once and then costs
/// one `fstatat` per file -- where a path's `lstat` had the kernel resolve
/// the whole path each time. Only as many folders are held open as the tree
/// is deep.
///
/// A folder held open is the folder that was there when it was opened, as a
/// walk's own handle on a folder it is listing is. A folder swapped for a
/// link after that is not followed: a new cursor sees the link and refuses
/// it.
pub struct StatCursor {
    root: PathBuf,
    #[cfg(unix)]
    root_fd: Option<rustix::fd::OwnedFd>,
    /// The folders opened so far, from the root down: each one's name, and
    /// its handle, opened relative to the one before.
    #[cfg(unix)]
    folders: Vec<(std::ffi::OsString, rustix::fd::OwnedFd)>,
}

impl StatCursor {
    /// A cursor for files beneath `root`, opened as the administrator named
    /// it when the first file is stat'ed.
    pub fn new(root: &Path) -> Self {
        StatCursor {
            root: root.to_path_buf(),
            #[cfg(unix)]
            root_fd: None,
            #[cfg(unix)]
            folders: Vec::new(),
        }
    }

    /// Stat `path`, a file beneath the root. A path not beneath the root, a
    /// link at the file or at a folder above it, and anything but a regular
    /// file all fail -- [`is_refusal`] tells those apart from a failure that
    /// says nothing about the file, such as one that is gone.
    #[cfg(unix)]
    pub fn stat(&mut self, path: &Path) -> io::Result<FileMeta> {
        use rustix::fs::{AtFlags, FileType, Mode};
        let relative = checked_relative(relative_to(&self.root, path)?)?;
        let names: Vec<&std::ffi::OsStr> = relative.iter().collect();
        let Some((leaf, folders)) = names.split_last() else {
            return Err(not_library_file("an empty path names no file"));
        };
        if self.root_fd.is_none() {
            self.root_fd = Some(open_root(&self.root)?);
        }
        let Some(root_fd) = &self.root_fd else {
            return Err(io::Error::other("the root was just opened"));
        };
        // The folders already open that lead to this file are kept; the rest
        // are closed, and the file's own opened beneath the last one kept.
        let kept = self
            .folders
            .iter()
            .zip(folders)
            .take_while(|((held, _), name)| held.as_os_str() == **name)
            .count();
        self.folders.truncate(kept);
        for name in &folders[kept..] {
            let parent = self.folders.last().map_or(root_fd, |(_, fd)| fd);
            let fd = rustix::fs::openat(parent, *name, folder_flags(), Mode::empty())?;
            self.folders.push((name.to_os_string(), fd));
        }
        let folder = self.folders.last().map_or(root_fd, |(_, fd)| fd);
        let stat = rustix::fs::statat(folder, *leaf, AtFlags::SYMLINK_NOFOLLOW)?;
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Ok(FileMeta::of_stat(&stat)),
            FileType::Symlink => Err(not_library_file(format!(
                "{} is a symbolic link",
                relative.display()
            ))),
            _ => Err(not_library_file(format!(
                "{} is not a regular file",
                relative.display()
            ))),
        }
    }

    /// Unix is the only platform Beam serves from; elsewhere the path is
    /// stat'ed as named, refusing only a link at the file itself.
    #[cfg(not(unix))]
    pub fn stat(&mut self, path: &Path) -> io::Result<FileMeta> {
        let relative = checked_relative(relative_to(&self.root, path)?)?;
        let meta = std::fs::symlink_metadata(self.root.join(&relative))?;
        if !meta.is_file() {
            return Err(not_library_file(format!(
                "{} is not a regular file",
                relative.display()
            )));
        }
        Ok(FileMeta::from(&meta))
    }
}

/// A file of a library, opened by [`LibraryFile::open`]: the handle, the
/// metadata read from that handle, and the full path it was indexed under.
///
/// Everything the indexer learns about a media file -- its size, mtime and
/// identity, its content hash, its streams -- is read from one of these
/// (issue #238), so none of it can come from a file reached through a link.
#[derive(Debug)]
pub struct LibraryFile {
    file: File,
    metadata: Metadata,
    path: PathBuf,
}

impl LibraryFile {
    /// Open `path`, a file beneath the library `root`, with
    /// [`open_regular_file`]: a path not beneath `root`, a link at the file or
    /// at any folder between it and the root, and anything but a regular file
    /// all fail -- [`is_refusal`] tells those apart from a failure that says
    /// nothing about the file.
    pub fn open(root: &Path, path: &Path) -> io::Result<Self> {
        let (file, metadata) = open_regular_file(root, relative_to(root, path)?)?;
        Ok(Self {
            file,
            metadata,
            path: path.to_path_buf(),
        })
    }

    /// The full path the file was opened at: for logs, and for FFmpeg's
    /// guess at a container from its extension. Never opened again.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The metadata of the open handle, read as it was opened.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// A second handle on the same open file -- not a second open of its
    /// path. The two share a file offset, so each reader positions it first.
    pub fn try_clone_file(&self) -> io::Result<File> {
        self.file.try_clone()
    }

    /// The handle, its metadata and its path.
    pub fn into_parts(self) -> (File, Metadata, PathBuf) {
        let Self {
            file,
            metadata,
            path,
        } = self;
        (file, metadata, path)
    }
}

/// Whether `err`, from [`LibraryFile::open`] or a [`StatCursor`], says that
/// no regular file of the library is at the path: a link at the file or at a
/// folder above it (`ELOOP`), a folder above it that is no folder any more
/// (`ENOTDIR`), or -- the opener's own refusals, which it marks as such -- a
/// path not beneath the root, or a file that is a link or not a regular one.
/// Such a path is no file of the library, as a walk would not have listed
/// it. Any other failure -- a file deleted, a permission error, a transient
/// I/O error, an `EINVAL` from a filesystem -- says nothing about what is
/// there.
pub fn is_refusal(err: &io::Error) -> bool {
    if err
        .get_ref()
        .is_some_and(|inner| inner.is::<NotLibraryFile>())
    {
        return true;
    }
    #[cfg(unix)]
    {
        let errno = err.raw_os_error();
        errno == Some(rustix::io::Errno::LOOP.raw_os_error())
            || errno == Some(rustix::io::Errno::NOTDIR.raw_os_error())
    }
    #[cfg(not(unix))]
    {
        false
    }
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
                return Err(not_library_file(format!(
                    "{} does not name a path beneath a root",
                    relative.display()
                )));
            }
        }
    }
    if names.as_os_str().is_empty() {
        return Err(not_library_file(
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

/// The flags a folder beneath the root is opened with to stat a file in it
/// ([`StatCursor`]): never through a link. On Linux `O_PATH`, which, like the
/// kernel's own resolution of a path, needs only search permission on the
/// folder, not read permission.
#[cfg(unix)]
fn folder_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    #[cfg(target_os = "linux")]
    let access = OFlags::PATH;
    #[cfg(not(target_os = "linux"))]
    let access = OFlags::RDONLY;
    access | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
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
        return Err(not_library_file("an empty path names no file"));
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

    /// What a failed [`LibraryFile::open`] says about the path: a link at the
    /// file or above it, a path not beneath the root and a file that is no
    /// regular one are refusals -- no file of the library is there -- while a
    /// file that is simply gone says nothing more than that.
    #[cfg(unix)]
    #[test]
    fn a_refusal_is_told_apart_from_a_file_that_is_gone() {
        let outside = TempDir::new().unwrap();
        std::fs::create_dir(outside.path().join("Season 01")).unwrap();
        std::fs::write(outside.path().join("Season 01/E01.mkv"), b"outside").unwrap();
        let root = TempDir::new().unwrap();
        let root = root.path();
        std::fs::create_dir(root.join("Show")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("Season 01"),
            root.join("Show/Season 01"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("Season 01/E01.mkv"),
            root.join("E01.mkv"),
        )
        .unwrap();
        std::fs::write(root.join("file"), b"not a folder").unwrap();

        let refused = |path: &Path| is_refusal(&LibraryFile::open(root, path).unwrap_err());
        assert!(
            refused(&root.join("Show/Season 01/E01.mkv")),
            "a folder link"
        );
        assert!(refused(&root.join("E01.mkv")), "a file link");
        assert!(
            refused(&root.join("file/E01.mkv")),
            "a file where a folder was"
        );
        assert!(refused(&root.join("Show")), "a folder where a file was");
        assert!(
            refused(&outside.path().join("Season 01/E01.mkv")),
            "outside the root"
        );
        assert!(!refused(&root.join("gone.mkv")), "a file that is gone");

        let opened =
            LibraryFile::open(outside.path(), &outside.path().join("Season 01/E01.mkv")).unwrap();
        assert_eq!(opened.path(), outside.path().join("Season 01/E01.mkv"));
        assert_eq!(opened.metadata().len(), 7);
    }

    /// A refusal is the opener's own, or a link or a non-folder met on the
    /// way down; an error that merely shares a kind with a refusal -- an
    /// `EINVAL` a filesystem returned -- says nothing about the file.
    #[cfg(unix)]
    #[test]
    fn only_the_openers_own_refusals_and_links_are_refusals() {
        use rustix::io::Errno;
        let cases: [(&str, io::Error, bool); 8] = [
            ("the opener's refusal", not_library_file("no"), true),
            ("ELOOP", io::Error::from(Errno::LOOP), true),
            ("ENOTDIR", io::Error::from(Errno::NOTDIR), true),
            (
                "a filesystem's EINVAL",
                io::Error::from(Errno::INVAL),
                false,
            ),
            (
                "another InvalidInput",
                io::Error::new(io::ErrorKind::InvalidInput, "bad flag"),
                false,
            ),
            ("ENOENT", io::Error::from(Errno::NOENT), false),
            ("EACCES", io::Error::from(Errno::ACCESS), false),
            ("EIO", io::Error::from(Errno::IO), false),
        ];
        for (case, err, refusal) in cases {
            assert_eq!(is_refusal(&err), refusal, "{case}");
        }
    }

    /// A cursor stats each file as the file's own handle does, whatever
    /// order a walk visits folders in -- down, back up and across -- with the
    /// folders it keeps open.
    #[cfg(unix)]
    #[test]
    fn a_cursor_stats_each_file_as_its_handle_does() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let files = [
            "top.mkv",
            "A/a1.mkv",
            "A/B/b1.mkv",
            "A/B/C/c1.mkv",
            "A/a2.mkv",
            "A/D/d1.mkv",
            "E/e1.mkv",
            "A/B/b2.mkv",
            "top2.mkv",
        ];
        for (index, rel) in files.iter().enumerate() {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x".repeat(index + 1)).unwrap();
        }

        let mut cursor = StatCursor::new(root);
        for rel in files {
            let path = root.join(rel);
            let stat = cursor
                .stat(&path)
                .unwrap_or_else(|err| panic!("{rel}: {err}"));
            let (_, handle) = open_regular_file(root, Path::new(rel)).unwrap();
            assert_eq!(stat, FileMeta::from(&handle), "{rel}");
            assert_eq!(stat.modified(), handle.modified().ok(), "{rel}");
            assert_eq!(stat, stat_regular_file(root, &path).unwrap(), "{rel}");
        }
    }

    /// A cursor refuses what the opener refuses -- a link at the file or at
    /// a folder above it, a path outside the root, anything but a regular
    /// file -- and says no more than "gone" of a file that is gone.
    #[cfg(unix)]
    #[test]
    fn a_cursor_refuses_what_the_opener_refuses() {
        let outside = TempDir::new().unwrap();
        std::fs::create_dir(outside.path().join("Season 01")).unwrap();
        std::fs::write(outside.path().join("Season 01/E01.mkv"), b"outside").unwrap();
        let root = TempDir::new().unwrap();
        let root = root.path();
        std::fs::create_dir_all(root.join("Show/Extras")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("Season 01"),
            root.join("Show/Season 01"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("Season 01/E01.mkv"),
            root.join("E01.mkv"),
        )
        .unwrap();
        std::fs::write(root.join("file"), b"not a folder").unwrap();
        #[cfg(target_os = "linux")]
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            root.join("fifo.mkv"),
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .unwrap();

        let mut cursor = StatCursor::new(root);
        let mut refused = |path: &Path| is_refusal(&cursor.stat(path).unwrap_err());
        assert!(
            refused(&root.join("Show/Season 01/E01.mkv")),
            "a folder link"
        );
        assert!(refused(&root.join("E01.mkv")), "a file link");
        assert!(
            refused(&root.join("file/E01.mkv")),
            "a file where a folder was"
        );
        assert!(refused(&root.join("Show/Extras")), "a folder");
        #[cfg(target_os = "linux")]
        assert!(refused(&root.join("fifo.mkv")), "a FIFO");
        assert!(
            refused(&outside.path().join("Season 01/E01.mkv")),
            "outside the root"
        );
        assert!(refused(&root.join("Show/../file")), "not a path of names");
        let gone = StatCursor::new(root)
            .stat(&root.join("Show/gone.mkv"))
            .unwrap_err();
        assert_eq!(gone.kind(), io::ErrorKind::NotFound);
        assert!(!is_refusal(&gone), "a file that is gone");
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

    /// A folder a cursor already holds is read as it was when the cursor
    /// opened it, even once a link has replaced it: nothing is read through
    /// the link, and a new cursor refuses it.
    #[cfg(unix)]
    #[test]
    fn a_held_folder_is_read_as_it_was_and_a_new_cursor_refuses_the_link() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        let folder = root.join("Heat (1995)");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("a.mkv"), b"inside").unwrap();
        std::fs::write(folder.join("c.mkv"), b"inside").unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("c.mkv"), b"a longer outside file").unwrap();
        let before = FileMeta::from(&std::fs::metadata(folder.join("c.mkv")).unwrap());

        let mut cursor = StatCursor::new(&root);
        cursor.stat(&folder.join("a.mkv")).unwrap();
        std::fs::rename(&folder, root.join("Heat (1995).old")).unwrap();
        std::os::unix::fs::symlink(&outside, &folder).unwrap();

        assert_eq!(cursor.stat(&folder.join("c.mkv")).unwrap(), before);
        let err = stat_regular_file(&root, &folder.join("c.mkv")).unwrap_err();
        assert!(is_refusal(&err), "{err:?}");
    }

    /// A stat's nanosecond field outside a second is clamped into it, from
    /// either end.
    #[cfg(unix)]
    #[test]
    fn an_out_of_range_nanosecond_field_is_clamped_into_the_second() {
        let cases: [(i128, u32); 6] = [
            (i128::from(i64::MIN), 0),
            (-1, 0),
            (0, 0),
            (999_999_999, 999_999_999),
            (1_000_000_000, 999_999_999),
            (i128::from(u64::MAX), 999_999_999),
        ];
        for (field, expected) in cases {
            assert_eq!(nanos(field), expected, "{field}");
        }
    }
}
