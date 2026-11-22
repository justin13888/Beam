//! What kind of filesystem a library root lives on.
//!
//! The watcher needs one fact about a root it cannot learn from the root
//! itself: whether changes made there are delivered as native events at all.
//! inotify only reports changes made through the local kernel, so a file an
//! NFS client or an SMB peer writes on another host never produces an event
//! here -- the library would only update on the periodic rescan. A root on a
//! network filesystem is therefore polled instead of natively watched.
//!
//! A narrow trait for that one question, not a filesystem abstraction: every
//! other filesystem behaviour is tested against a real `TempDir`, and a
//! `TempDir` cannot be put on NFS. The classification tables are pure
//! functions so they are tested directly.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

/// Whether a filesystem delivers native change events for changes made
/// anywhere, or only for changes made through this host's kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemKind {
    /// Changes are made through this kernel, so native events see them.
    Local,
    /// Changes may be made by another host, so native events can miss them.
    Network,
}

/// Answers which [`FilesystemKind`] a path lives on, and which other
/// filesystems are mounted beneath it.
pub trait FilesystemProbe: Send + Sync + std::fmt::Debug {
    fn kind(&self, path: &Path) -> io::Result<FilesystemKind>;

    /// One directory on each filesystem mounted beneath `root`, other than
    /// the one `root` itself is on (issue #452).
    ///
    /// A local root can hold a network share mounted inside it, and native
    /// events miss what other hosts change there just as they would at the
    /// root; [`Self::kind`] of each directory returned says whether one is.
    /// One directory per filesystem is enough, because the kind is a property
    /// of the filesystem. Symbolic links are not followed: they are not part
    /// of a library. A subtree that cannot be read is skipped, so the answer
    /// covers what the watcher itself can see.
    ///
    /// Nothing, by default: the answer before mounts were looked for, and
    /// the right one for a probe that is only ever asked about files.
    fn filesystems_beneath(&self, _root: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

/// Linux `statfs(2)` `f_type` magic numbers of filesystems whose contents
/// another host can change. From `linux/magic.h` and `statfs(2)`.
///
/// FUSE is included deliberately: sshfs, rclone and s3fs are FUSE, and
/// nothing distinguishes them from a local FUSE filesystem such as mergerfs,
/// which is then polled when it did not need to be. A missed change costs a
/// stale library until the hourly rescan; an unneeded poll costs one tree walk
/// per poll interval.
const LINUX_NETWORK_MAGICS: &[u32] = &[
    0x0000_6969, // nfs
    0x0000_517B, // smb
    0xFF53_4D42, // cifs
    0xFE53_4D42, // smb2
    0x0102_1997, // 9p
    0x00C3_6400, // ceph
    0x5346_414F, // afs
    0x6B41_4653, // kafs
    0x7375_7245, // coda
    0x0BD0_0BD0, // lustre
    0x0116_1970, // gfs2
    0x7461_636F, // ocfs2
    0x4750_4653, // gpfs
    0x0000_564C, // ncp
    0x786F_4256, // vboxsf
    0x6573_5546, // fuse
];

/// Classify a Linux `statfs` `f_type`. Every magic number is 32 bits wide,
/// while `f_type`'s own width varies by architecture, so the caller truncates.
pub fn classify_linux_magic(f_type: u32) -> FilesystemKind {
    if LINUX_NETWORK_MAGICS.contains(&f_type) {
        FilesystemKind::Network
    } else {
        FilesystemKind::Local
    }
}

/// Classify a macOS `statfs` `f_fstypename`.
pub fn classify_macos_fstypename(name: &str) -> FilesystemKind {
    let name = name.to_ascii_lowercase();
    let network = matches!(
        name.as_str(),
        "nfs" | "smbfs" | "afpfs" | "webdav" | "cifs" | "ftp"
    ) || name.contains("fuse");
    if network {
        FilesystemKind::Network
    } else {
        FilesystemKind::Local
    }
}

/// The production probe: one `statfs(2)` call on the path.
#[derive(Debug, Default, Clone, Copy)]
pub struct StatfsFilesystemProbe;

impl FilesystemProbe for StatfsFilesystemProbe {
    fn kind(&self, path: &Path) -> io::Result<FilesystemKind> {
        statfs_kind(path)
    }

    fn filesystems_beneath(&self, root: &Path) -> io::Result<Vec<PathBuf>> {
        walk_filesystems_beneath(root)
    }
}

/// The first of `directories` on each device other than `root_device`.
///
/// `directories` is a pre-order walk, so the first directory seen on a
/// device is the top of that filesystem's subtree -- its mount point, or the
/// first directory of it the walk could reach.
pub fn first_directory_per_device(
    root_device: u64,
    directories: impl IntoIterator<Item = (PathBuf, u64)>,
) -> Vec<PathBuf> {
    let mut seen = HashSet::from([root_device]);
    directories
        .into_iter()
        .filter(|(_, device)| seen.insert(*device))
        .map(|(path, _)| path)
        .collect()
}

/// Walk `root` comparing each directory's `st_dev` with the root's: one
/// `lstat` per directory, none per file.
#[cfg(unix)]
fn walk_filesystems_beneath(root: &Path) -> io::Result<Vec<PathBuf>> {
    use std::os::unix::fs::MetadataExt;

    let root_device = std::fs::metadata(root)?.dev();
    let directories = walkdir::WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        // An unreadable entry is skipped, and walkdir does not descend into a
        // directory it could not read.
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir())
        .filter_map(|entry| {
            let device = entry.metadata().ok()?.dev();
            Some((entry.into_path(), device))
        });
    Ok(first_directory_per_device(root_device, directories))
}

/// Without `st_dev` a mount point cannot be told apart, so nothing beneath a
/// root is reported -- the behaviour before mounts were looked for.
#[cfg(not(unix))]
fn walk_filesystems_beneath(_root: &Path) -> io::Result<Vec<PathBuf>> {
    Ok(Vec::new())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn statfs_kind(path: &Path) -> io::Result<FilesystemKind> {
    let stat = rustix::fs::statfs(path).map_err(io::Error::from)?;
    // `f_type` is a `long` on some architectures and a `u32` on others; every
    // magic it can hold fits in 32 bits, so truncation loses nothing.
    #[allow(clippy::unnecessary_cast)]
    let f_type = stat.f_type as u32;
    Ok(classify_linux_magic(f_type))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn statfs_kind(path: &Path) -> io::Result<FilesystemKind> {
    let stat = rustix::fs::statfs(path).map_err(io::Error::from)?;
    // `c_char` is `i8` on x86_64 and `u8` on aarch64.
    #[allow(clippy::unnecessary_cast)]
    let bytes: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    Ok(classify_macos_fstypename(&String::from_utf8_lossy(&bytes)))
}

/// Elsewhere there is no classification table, so every root is treated as
/// local -- the behaviour before network filesystems were detected at all.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn statfs_kind(_path: &Path) -> io::Result<FilesystemKind> {
    Ok(FilesystemKind::Local)
}

/// Test doubles. See the note on `watcher::in_memory` for why they live in
/// one `#[mutants::skip]` module.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;

    /// Reports the same kind for every path -- how a test puts a `TempDir`
    /// "on NFS".
    #[derive(Debug, Clone, Copy)]
    pub struct FixedFilesystemProbe(pub FilesystemKind);

    impl FilesystemProbe for FixedFilesystemProbe {
        fn kind(&self, _path: &Path) -> io::Result<FilesystemKind> {
            Ok(self.0)
        }
    }

    /// Fails for every path, as `statfs` does when it is denied.
    #[derive(Debug, Clone, Copy)]
    pub struct FailingFilesystemProbe;

    impl FilesystemProbe for FailingFilesystemProbe {
        fn kind(&self, _path: &Path) -> io::Result<FilesystemKind> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }

        fn filesystems_beneath(&self, _root: &Path) -> io::Result<Vec<PathBuf>> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
    }

    /// Local storage with other filesystems mounted at the given directories
    /// -- how a test mounts an NFS share inside a `TempDir`. A path's kind is
    /// that of the innermost mount holding it, and local outside them all.
    #[derive(Debug, Clone, Default)]
    pub struct MountedFilesystemProbe {
        pub mounts: Vec<(PathBuf, FilesystemKind)>,
    }

    impl FilesystemProbe for MountedFilesystemProbe {
        fn kind(&self, path: &Path) -> io::Result<FilesystemKind> {
            Ok(self
                .mounts
                .iter()
                .filter(|(mount, _)| path.starts_with(mount))
                .max_by_key(|(mount, _)| mount.components().count())
                .map_or(FilesystemKind::Local, |(_, kind)| *kind))
        }

        fn filesystems_beneath(&self, root: &Path) -> io::Result<Vec<PathBuf>> {
            Ok(self
                .mounts
                .iter()
                .map(|(mount, _)| mount)
                .filter(|mount| mount.starts_with(root) && mount.as_path() != root)
                .cloned()
                .collect())
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::{FailingFilesystemProbe, FixedFilesystemProbe, MountedFilesystemProbe};

#[cfg(test)]
mod tests {
    use super::*;

    /// Twelve of the sixteen network literals above are checked against
    /// independent sources: seven against the constants `libc` publishes, and
    /// five that `libc` does not name (cifs, smb2, 9p, ceph and kafs) against
    /// the kernel's own `linux/magic.h` as `linux-raw-sys` generates it.
    ///
    /// The other four -- Lustre, GFS2, GPFS and vboxsf -- are not checked by
    /// any test. Neither crate has a constant for them (their magic lives in
    /// out-of-tree or filesystem-private headers), and restating the literal
    /// here would only mirror the list, which proves nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_magics_agree_with_libc_and_the_kernel_headers() {
        #[allow(clippy::unnecessary_cast)]
        let network = [
            libc::NFS_SUPER_MAGIC as u32,
            libc::SMB_SUPER_MAGIC as u32,
            libc::AFS_SUPER_MAGIC as u32,
            libc::CODA_SUPER_MAGIC as u32,
            libc::OCFS2_SUPER_MAGIC as u32,
            libc::NCP_SUPER_MAGIC as u32,
            libc::FUSE_SUPER_MAGIC as u32,
            linux_raw_sys::general::CIFS_SUPER_MAGIC,
            linux_raw_sys::general::SMB2_SUPER_MAGIC,
            linux_raw_sys::general::V9FS_MAGIC,
            linux_raw_sys::general::CEPH_SUPER_MAGIC,
            linux_raw_sys::general::AFS_FS_MAGIC,
        ];
        for magic in network {
            assert_eq!(
                classify_linux_magic(magic),
                FilesystemKind::Network,
                "{magic:#x}"
            );
        }
        #[allow(clippy::unnecessary_cast)]
        let local = [
            libc::EXT4_SUPER_MAGIC as u32,
            libc::BTRFS_SUPER_MAGIC as u32,
            libc::XFS_SUPER_MAGIC as u32,
            libc::TMPFS_MAGIC as u32,
        ];
        for magic in local {
            assert_eq!(
                classify_linux_magic(magic),
                FilesystemKind::Local,
                "{magic:#x}"
            );
        }
    }

    #[test]
    fn macos_type_names_classify() {
        for name in [
            "nfs", "smbfs", "afpfs", "webdav", "cifs", "macfuse", "osxfuse", "NFS",
        ] {
            assert_eq!(
                classify_macos_fstypename(name),
                FilesystemKind::Network,
                "{name}"
            );
        }
        for name in ["apfs", "hfs", "msdos", "exfat", "devfs"] {
            assert_eq!(
                classify_macos_fstypename(name),
                FilesystemKind::Local,
                "{name}"
            );
        }
    }

    /// The real probe on a real directory: a `TempDir` is local storage.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_temp_dir_probes_as_local() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            StatfsFilesystemProbe.kind(dir.path()).unwrap(),
            FilesystemKind::Local
        );
    }

    /// The walk is pre-order, so a filesystem's first directory is where it
    /// is mounted; nothing after it on the same device, and nothing on the
    /// root's own device, is reported.
    #[test]
    fn the_first_directory_on_each_other_device_is_reported() {
        let walk = [
            ("/m/a", 1),
            ("/m/nfs", 2),
            ("/m/nfs/deeper", 2),
            ("/m/nfs/usb", 3),
            ("/m/b", 1),
            ("/m/bind-of-nfs", 2),
        ]
        .map(|(path, device)| (PathBuf::from(path), device));
        assert_eq!(
            first_directory_per_device(1, walk),
            [PathBuf::from("/m/nfs"), PathBuf::from("/m/nfs/usb")],
        );
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Each device other than the root's is reported exactly once,
            /// by the first directory the walk met on it, in walk order.
            #[test]
            fn every_other_device_is_reported_once_by_its_first_directory(
                root_device in 0u64..4,
                devices in prop::collection::vec(0u64..4, 0..30),
            ) {
                let walk: Vec<(PathBuf, u64)> = devices
                    .iter()
                    .enumerate()
                    .map(|(n, device)| (PathBuf::from(format!("/d{n}")), *device))
                    .collect();
                let reported = first_directory_per_device(root_device, walk.clone());

                let expected: Vec<PathBuf> = walk
                    .iter()
                    .enumerate()
                    .filter(|(n, (_, device))| {
                        *device != root_device
                            && walk.iter().position(|(_, d)| d == device) == Some(*n)
                    })
                    .map(|(_, (path, _))| path.clone())
                    .collect();
                prop_assert_eq!(reported, expected);
            }
        }
    }

    /// The real walk over a tree on one filesystem: nested directories are
    /// on the root's device, and a symbolic link to another directory is not
    /// followed, so nothing is reported.
    #[cfg(unix)]
    #[test]
    fn a_tree_on_one_filesystem_has_nothing_mounted_beneath_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        // `/dev` is a filesystem of its own on Linux and macOS alike.
        std::os::unix::fs::symlink("/dev", dir.path().join("a/linked")).unwrap();

        assert_eq!(
            StatfsFilesystemProbe
                .filesystems_beneath(dir.path())
                .unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    /// The real walk finds a real mount. `/dev` holds mounts of its own on
    /// Linux (`/dev/shm`, `/dev/pts`, `/dev/mqueue`), found in the mount table
    /// rather than assumed; a host with none skips the assertion.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_filesystem_mounted_beneath_the_root_is_reported() {
        let Ok(table) = std::fs::read_to_string("/proc/self/mountinfo") else {
            return;
        };
        let mounted: Vec<PathBuf> = table
            .lines()
            .filter_map(|line| line.split(' ').nth(4).map(PathBuf::from))
            .filter(|point| point.parent() == Some(Path::new("/dev")))
            .collect();
        if mounted.is_empty() {
            return;
        }

        let reported = StatfsFilesystemProbe
            .filesystems_beneath(Path::new("/dev"))
            .unwrap();
        assert!(
            mounted.iter().any(|point| reported.contains(point)),
            "none of {mounted:?} was reported beneath /dev: {reported:?}"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_missing_path_is_an_error_not_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        let err = StatfsFilesystemProbe
            .kind(&dir.path().join("absent"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
