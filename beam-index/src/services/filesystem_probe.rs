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

use std::io;
use std::path::Path;

/// Whether a filesystem delivers native change events for changes made
/// anywhere, or only for changes made through this host's kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemKind {
    /// Changes are made through this kernel, so native events see them.
    Local,
    /// Changes may be made by another host, so native events can miss them.
    Network,
}

/// Answers which [`FilesystemKind`] a path lives on.
pub trait FilesystemProbe: Send + Sync + std::fmt::Debug {
    fn kind(&self, path: &Path) -> io::Result<FilesystemKind>;
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
}

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::FixedFilesystemProbe;

#[cfg(test)]
mod tests {
    use super::*;

    /// The hex literals above are checked against an independent source: the
    /// constants `libc` publishes for the filesystems it knows.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_magics_agree_with_libc() {
        #[allow(clippy::unnecessary_cast)]
        let network = [
            libc::NFS_SUPER_MAGIC as u32,
            libc::SMB_SUPER_MAGIC as u32,
            libc::AFS_SUPER_MAGIC as u32,
            libc::CODA_SUPER_MAGIC as u32,
            libc::OCFS2_SUPER_MAGIC as u32,
            libc::NCP_SUPER_MAGIC as u32,
            libc::FUSE_SUPER_MAGIC as u32,
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
