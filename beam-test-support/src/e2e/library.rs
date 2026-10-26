//! The library on disk, and proof the server never wrote to it.
//!
//! Beam must never touch the filesystem holding the source media (FR-202):
//! no sidecar, no lock file, no rename, no changed timestamp. Every change to
//! the tree goes through [`FixtureLibrary`], which records what it wrote --
//! each directory, each file's bytes and modification time, each link's
//! target -- and [`FixtureLibrary::assert_untouched`] walks the tree and
//! compares it with that record.
//!
//! Between the test's own changes the tree is sealed read-only (directories
//! `0555`, files `0444`), the closest a test process gets to the `:ro` mount
//! the deployment uses: a write the server attempted fails rather than lands.
//! A test running as root is not stopped by modes, so the walk afterwards is
//! the assertion and the seal only makes a violation loud sooner.
//!
//! The record is the test's own, written as it changes the tree, so a test
//! can rename and delete files mid-run and the comparison still means "the
//! server changed nothing".

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::fixtures::Video;

/// What one path in the tree is, as the test left it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Dir,
    File {
        len: u64,
        sha256: [u8; 32],
        modified: SystemTime,
    },
    Symlink {
        target: PathBuf,
    },
}

/// A directory tree the server is pointed at as its `BEAM_VIDEO_DIR`, plus a
/// directory outside it for files a library may link to but never read.
#[derive(Debug)]
pub struct FixtureLibrary {
    /// Holds `video_dir/` and `outside/`; dropping it deletes both.
    dir: TempDir,
    /// Every path under `video_dir`, relative to it.
    manifest: BTreeMap<PathBuf, Entry>,
}

impl Default for FixtureLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl FixtureLibrary {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("beam-e2e-library-")
            .tempdir()
            .expect("create the library directory");
        fs::create_dir(dir.path().join("video_dir")).expect("create the video directory");
        fs::create_dir(dir.path().join("outside")).expect("create the outside directory");
        let library = Self {
            dir,
            manifest: BTreeMap::new(),
        };
        library.seal();
        library
    }

    /// The directory the server is configured with; every library root is
    /// beneath it.
    pub fn video_dir(&self) -> PathBuf {
        self.dir.path().join("video_dir")
    }

    /// A directory beside `video_dir`, outside every library: where a link
    /// in a library points when the test proves the link is not followed.
    pub fn outside(&self) -> PathBuf {
        self.dir.path().join("outside")
    }

    /// The absolute path of `rel` under `video_dir`.
    pub fn path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.video_dir().join(rel)
    }

    /// Write `bytes` at `rel`, creating its parent directories.
    pub fn write(&mut self, rel: impl AsRef<Path>, bytes: &[u8]) {
        let rel = rel.as_ref().to_path_buf();
        self.change(|library| {
            library.create_parents(&rel);
            fs::write(library.path(&rel), bytes)
                .unwrap_or_else(|e| panic!("write {}: {e}", rel.display()));
        });
        self.record_parents(&rel);
        self.record_file(&rel);
    }

    /// Render `video` at `rel`, creating its parent directories.
    pub fn add_video(&mut self, rel: impl AsRef<Path>, video: &Video) {
        let rel = rel.as_ref().to_path_buf();
        self.change(|library| {
            library.create_parents(&rel);
            video.render(&library.path(&rel));
        });
        self.record_parents(&rel);
        self.record_file(&rel);
    }

    /// Copy the file at `source`, which is outside the library, to `rel`.
    pub fn copy_in(&mut self, source: &Path, rel: impl AsRef<Path>) {
        let bytes = fs::read(source).unwrap_or_else(|e| panic!("read {}: {e}", source.display()));
        self.write(rel, &bytes);
    }

    /// Create a file of `len` bytes at `rel` that occupies no disk: a hole,
    /// read back as zeros.
    pub fn write_sparse(&mut self, rel: impl AsRef<Path>, len: u64) {
        let rel = rel.as_ref().to_path_buf();
        self.change(|library| {
            library.create_parents(&rel);
            let file = fs::File::create(library.path(&rel))
                .unwrap_or_else(|e| panic!("create {}: {e}", rel.display()));
            file.set_len(len)
                .unwrap_or_else(|e| panic!("size {}: {e}", rel.display()));
        });
        self.record_parents(&rel);
        self.record_file(&rel);
    }

    /// Create the directory `rel`, and its parents.
    pub fn create_dir(&mut self, rel: impl AsRef<Path>) {
        let rel = rel.as_ref().to_path_buf();
        self.change(|library| {
            fs::create_dir_all(library.path(&rel))
                .unwrap_or_else(|e| panic!("create {}: {e}", rel.display()));
        });
        self.record_dir_and_ancestors(&rel);
    }

    /// Make `rel` a symbolic link to `target`, which may be anywhere.
    pub fn symlink(&mut self, target: &Path, rel: impl AsRef<Path>) {
        let rel = rel.as_ref().to_path_buf();
        self.change(|library| {
            library.create_parents(&rel);
            std::os::unix::fs::symlink(target, library.path(&rel))
                .unwrap_or_else(|e| panic!("link {} -> {}: {e}", rel.display(), target.display()));
        });
        self.record_parents(&rel);
        self.manifest.insert(
            rel,
            Entry::Symlink {
                target: target.to_path_buf(),
            },
        );
    }

    /// Rename `from` to `to` -- a file or a whole directory, within the
    /// video directory -- creating `to`'s parents.
    pub fn rename(&mut self, from: impl AsRef<Path>, to: impl AsRef<Path>) {
        let (from, to) = (from.as_ref().to_path_buf(), to.as_ref().to_path_buf());
        self.change(|library| {
            library.create_parents(&to);
            fs::rename(library.path(&from), library.path(&to))
                .unwrap_or_else(|e| panic!("rename {} to {}: {e}", from.display(), to.display()));
        });
        let moved: Vec<(PathBuf, Entry)> = self
            .manifest
            .iter()
            .filter(|(path, _)| path.starts_with(&from))
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect();
        for (path, entry) in moved {
            self.manifest.remove(&path);
            let suffix = path.strip_prefix(&from).expect("filtered on the prefix");
            let renamed = if suffix.as_os_str().is_empty() {
                to.clone()
            } else {
                to.join(suffix)
            };
            self.manifest.insert(renamed, entry);
        }
        self.record_parents(&to);
    }

    /// Exchange the names of the files `a` and `b`.
    pub fn swap(&mut self, a: impl AsRef<Path>, b: impl AsRef<Path>) {
        let (a, b) = (a.as_ref().to_path_buf(), b.as_ref().to_path_buf());
        let holding = a.with_extension("swapping");
        self.rename(&a, &holding);
        self.rename(&b, &a);
        self.rename(&holding, &b);
    }

    /// Set the modification time of the file at `rel` to `modified`, as a
    /// copy that preserves timestamps would leave it.
    pub fn set_modified(&mut self, rel: impl AsRef<Path>, modified: SystemTime) {
        let rel = rel.as_ref().to_path_buf();
        self.change(|library| {
            fs::File::options()
                .write(true)
                .open(library.path(&rel))
                .and_then(|file| file.set_modified(modified))
                .unwrap_or_else(|e| panic!("set the mtime of {}: {e}", rel.display()));
        });
        self.record_file(&rel);
    }

    /// The modification time of the file at `rel`, as the test left it.
    pub fn modified(&self, rel: impl AsRef<Path>) -> SystemTime {
        let rel = rel.as_ref();
        match self.manifest.get(rel) {
            Some(Entry::File { modified, .. }) => *modified,
            other => panic!("{} is not a file the test wrote: {other:?}", rel.display()),
        }
    }

    /// Remove the file or link at `rel`.
    pub fn remove_file(&mut self, rel: impl AsRef<Path>) {
        let rel = rel.as_ref().to_path_buf();
        self.change(|library| {
            fs::remove_file(library.path(&rel))
                .unwrap_or_else(|e| panic!("remove {}: {e}", rel.display()));
        });
        self.manifest.remove(&rel);
    }

    /// Run `edit` with the tree writable, then seal it again. The caller
    /// records what the edit made; nothing is read back from the tree, so a
    /// path the server wrote is never absorbed into the record.
    fn change(&mut self, edit: impl FnOnce(&Self)) {
        self.unseal();
        edit(self);
        self.seal();
    }

    fn create_parents(&self, rel: &Path) {
        if let Some(parent) = rel.parent() {
            fs::create_dir_all(self.path(parent))
                .unwrap_or_else(|e| panic!("create {}: {e}", parent.display()));
        }
    }

    /// Record the file at `rel` as it is now.
    fn record_file(&mut self, rel: &Path) {
        let entry = file_entry(&self.path(rel));
        self.manifest.insert(rel.to_path_buf(), entry);
    }

    /// Record the directories above `rel`, which the edit created where they
    /// did not already exist.
    fn record_parents(&mut self, rel: &Path) {
        if let Some(parent) = rel.parent() {
            self.record_dir_and_ancestors(parent);
        }
    }

    /// Record `rel` as a directory, and every directory above it. Only the
    /// paths the edit named are recorded -- never a walk of the tree -- so a
    /// directory the server created between two edits stays out of the
    /// record and [`Self::assert_untouched`] reports it.
    fn record_dir_and_ancestors(&mut self, rel: &Path) {
        for dir in rel.ancestors() {
            if dir.as_os_str().is_empty() {
                break;
            }
            self.manifest.insert(dir.to_path_buf(), Entry::Dir);
        }
    }

    /// Assert the tree is exactly what the test made it: the same paths, of
    /// the same kinds, with the same bytes, modification times and link
    /// targets.
    ///
    /// # Panics
    ///
    /// Naming every path that differs, which is a write the server made into
    /// the source media.
    pub fn assert_untouched(&self) {
        let found = walk(&self.video_dir());
        let mut differences = Vec::new();
        for (rel, expected) in &self.manifest {
            match found.get(rel) {
                None => differences.push(format!("{} is gone", rel.display())),
                Some(actual) if actual != expected => differences.push(format!(
                    "{} changed: expected {expected:?}, found {actual:?}",
                    rel.display()
                )),
                Some(_) => {}
            }
        }
        for (rel, actual) in &found {
            if !self.manifest.contains_key(rel) {
                differences.push(format!("{} appeared: {actual:?}", rel.display()));
            }
        }
        assert!(
            differences.is_empty(),
            "the library tree was written to:\n  {}",
            differences.join("\n  ")
        );
    }

    /// Make every directory `0555` and every file `0444`.
    fn seal(&self) {
        set_modes(&self.video_dir(), 0o555, 0o444);
    }

    /// Make every directory `0755` and every file `0644`.
    fn unseal(&self) {
        set_modes(&self.video_dir(), 0o755, 0o644);
    }
}

impl Drop for FixtureLibrary {
    fn drop(&mut self) {
        // A sealed directory's entries cannot be deleted, so `TempDir` would
        // leave the tree behind.
        self.unseal();
    }
}

/// Set `dir`'s mode and that of everything beneath it, never following a
/// link: a link's target may be outside the tree, and is not the tree's to
/// change.
fn set_modes(dir: &Path, dir_mode: u32, file_mode: u32) {
    // Parents before children when opening up, children before parents when
    // closing: a directory being walked must stay listable.
    let set = |path: &Path, mode: u32| {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
    };
    if dir_mode & 0o200 != 0 {
        set(dir, dir_mode);
    }
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("list {}: {e}", dir.display())) {
        let entry = entry.expect("a directory entry");
        let file_type = entry.file_type().expect("an entry's type");
        if file_type.is_dir() {
            set_modes(&entry.path(), dir_mode, file_mode);
        } else if file_type.is_file() {
            set(&entry.path(), file_mode);
        }
    }
    if dir_mode & 0o200 == 0 {
        set(dir, dir_mode);
    }
}

/// Every path beneath `root`, relative to it, as it is on disk now. Links are
/// recorded, never followed.
fn walk(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("list {}: {e}", dir.display())) {
            let path = entry.expect("a directory entry").path();
            let rel = path
                .strip_prefix(root)
                .expect("beneath the root")
                .to_path_buf();
            let metadata = fs::symlink_metadata(&path).expect("an entry's metadata");
            let entry = if metadata.file_type().is_symlink() {
                Entry::Symlink {
                    target: fs::read_link(&path).expect("a link's target"),
                }
            } else if metadata.is_dir() {
                pending.push(path.clone());
                Entry::Dir
            } else {
                file_entry(&path)
            };
            found.insert(rel, entry);
        }
    }
    found
}

fn file_entry(path: &Path) -> Entry {
    let metadata = fs::symlink_metadata(path).expect("a file's metadata");
    let mut file = fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).expect("read a library file");
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Entry::File {
        len: metadata.len(),
        sha256: hasher.finalize().into(),
        modified: metadata.modified().expect("a modification time"),
    }
}
