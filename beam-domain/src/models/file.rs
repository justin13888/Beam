use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use crate::utils::classification::ContainerTags;

/// The instant `mtime` reads back as once a
/// [`crate::repositories::FileRepository`] has stored it (issue #229).
///
/// `files.mtime` is a `TIMESTAMPTZ`, which Postgres keeps in whole
/// microseconds, and the driver encodes an instant as whole microseconds
/// since 2000-01-01 -- truncating toward that epoch, so an instant before it
/// rounds up. A filesystem reports nanoseconds, so an mtime read from disk
/// almost never equals the one its row holds: every comparison of the two
/// first brings the file's to this precision. The same holds for a
/// [`FileIdentity`]'s `ctime`, kept in a `TIMESTAMPTZ` too.
pub fn mtime_as_stored(mtime: DateTime<Utc>) -> DateTime<Utc> {
    let epoch = DateTime::from_timestamp(946_684_800, 0).expect("2000-01-01 is a valid instant");
    // chrono spans about 262,000 years either side of year 0, so no instant
    // is further from 2000-01-01 than about 8.3e18 microseconds -- inside
    // i64's 9.2e18.
    let micros = (mtime - epoch)
        .num_microseconds()
        .expect("chrono's range fits in i64 microseconds from 2000-01-01");
    epoch + chrono::TimeDelta::microseconds(micros)
}

/// What a rename or a replace always changes about the file at a path, as
/// the indexer recorded it (issue #228): its inode and its change time.
///
/// A size and an mtime can both survive a swap -- two files of one size
/// written within one timestamp tick, or copied by a tool that keeps mtimes
/// (`cp -p`, `rsync -a`). An inode cannot: a file renamed over a path brings
/// its own. Nor can a ctime: the kernel sets it on every rename and write,
/// and no copy tool can set it back. So a file whose identity is not its
/// row's may hold other content, and is hashed. On a network or FUSE
/// filesystem, which may renumber inodes between scans, the indexer
/// compares only the ctime.
///
/// Read only on Unix, Beam's first-class platforms (Linux and macOS);
/// elsewhere a file has none and only its size and mtime are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub inode: u64,
    /// Held at the precision [`mtime_as_stored`] describes.
    pub ctime: DateTime<Utc>,
}

impl FileIdentity {
    /// This identity as a [`crate::repositories::FileRepository`] reads it
    /// back: its `ctime` brought to [`mtime_as_stored`]'s precision.
    pub fn as_stored(self) -> Self {
        Self {
            inode: self.inode,
            ctime: mtime_as_stored(self.ctime),
        }
    }
}

/// Represents a media file in the library
#[derive(Debug, Clone)]
pub struct MediaFile {
    pub id: Uuid,
    pub library_id: Uuid,
    pub path: PathBuf,
    pub hash: u64,
    pub size_bytes: u64,
    /// Filesystem modification time; paired with `size_bytes` for change
    /// detection. Held at the precision [`mtime_as_stored`] describes.
    pub mtime: Option<DateTime<Utc>>,
    /// The file's [`FileIdentity`] when it was last recorded; `None` on a
    /// platform without one, or for a row recorded before identities were
    /// (the next scan that finds the file unchanged records it).
    pub identity: Option<FileIdentity>,
    pub mime_type: Option<String>,
    pub duration: Option<Duration>,
    pub container_format: Option<String>,
    pub content: Option<MediaFileContent>,
    pub status: FileStatus,
    pub scanned_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the indexer first found this file gone from disk; `None` while it
    /// is present. A missing file is soft-deleted: every visible read of
    /// [`crate::repositories::FileRepository`] skips it, and it keeps its id
    /// -- and so its playback progress -- until the path reappears or the
    /// grace period runs out (issue #179).
    pub missing_since: Option<DateTime<Utc>>,
    /// The version of the path-classification rules that decided `content`
    /// (see [`crate::utils::media_path::CLASSIFIER_VERSION`]). A row decided
    /// by older rules is reclassified from its path by the next scan.
    pub classifier_version: u16,
    /// The file-level container tags classification reads, as the last
    /// successful probe read them (issue #184); `None` while the file has no
    /// successful probe -- or had its last one before these were stored. A
    /// reclassification reads these rather than probing the file again.
    pub container_tags: Option<ContainerTags>,
}

/// Status of the file in the library
#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum FileStatus {
    /// File is indexed and metadata matches
    Known,
    /// File exists but metadata/hash has changed
    Changed,
    /// File exists but extension is unknown/unsupported
    Unknown,
}

// `Display`/`FromStr` used to exist here to move this enum in and out of the
// `files.file_status` column as text. That was the bug: the column is a
// Postgres enum type, and binding text to it fails at runtime. The conversion
// now goes through `beam_entity::files::FileStatus` (a `DeriveActiveEnum`),
// which leaves the string forms with no callers -- so they are gone rather
// than kept as an untested second way to spell the same values.

/// The content type of a media file
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaFileContent {
    /// File is a movie -- or, when `part_number` is set, that part of a movie
    /// split across files (`Movie (2019) - CD2`, issue #233). Every part of
    /// one edition is a file of its one entry; the parts are told apart and
    /// ordered by this number.
    Movie {
        movie_entry_id: Uuid,
        part_number: Option<u32>,
    },
    /// File is a TV episode -- or, when `last_episode_number` is set, the
    /// run of episodes from `episode_id`'s up to that number that one
    /// multi-episode file holds (`S01E01E02`). The file is attached to the
    /// first episode; the rest of the range is recorded here rather than as
    /// extra rows.
    Episode {
        episode_id: Uuid,
        last_episode_number: Option<u32>,
    },
}

impl MediaFileContent {
    /// A whole-movie file of the entry `movie_entry_id`.
    pub fn movie(movie_entry_id: Uuid) -> Self {
        MediaFileContent::Movie {
            movie_entry_id,
            part_number: None,
        }
    }

    /// A single-episode file of `episode_id`.
    pub fn episode(episode_id: Uuid) -> Self {
        MediaFileContent::Episode {
            episode_id,
            last_episode_number: None,
        }
    }
}

/// Parameters for creating a new media file
#[derive(Debug, Clone)]
pub struct CreateMediaFile {
    pub library_id: Uuid,
    pub path: PathBuf,
    pub hash: u64,
    pub size_bytes: u64,
    pub mtime: Option<DateTime<Utc>>,
    /// See [`MediaFile::identity`].
    pub identity: Option<FileIdentity>,
    pub mime_type: Option<String>,
    pub duration: Option<Duration>,
    pub container_format: Option<String>,
    pub content: Option<MediaFileContent>,
    pub status: FileStatus,
    /// See [`MediaFile::classifier_version`].
    pub classifier_version: u16,
    /// See [`MediaFile::container_tags`].
    pub container_tags: Option<ContainerTags>,
}

/// A file's classification, replaced as a whole when a scan reclassifies it
/// under newer rules. `content: None` clears it: the path no longer says what
/// the file is.
#[derive(Debug, Clone)]
pub struct FileClassification {
    pub content: Option<MediaFileContent>,
    pub status: FileStatus,
    pub classifier_version: u16,
}

/// Parameters for updating an existing media file
#[derive(Debug, Clone)]
pub struct UpdateMediaFile {
    pub id: Uuid,
    pub hash: Option<u64>,
    pub size_bytes: Option<u64>,
    /// `Some` sets the stored mtime; `None` leaves it unchanged.
    pub mtime: Option<DateTime<Utc>>,
    /// `Some` sets the stored [`FileIdentity`]; `None` leaves it unchanged.
    pub identity: Option<FileIdentity>,
    pub probe: ProbeUpdate,
    pub content: Option<MediaFileContent>,
    pub status: Option<FileStatus>,
}

/// A row pointed at the path its file now has -- moved, renamed, or swapped
/// with another (issue #180) -- found there at `size_bytes`, `mtime` and
/// `identity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRelink {
    pub id: Uuid,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub mtime: Option<DateTime<Utc>>,
    pub identity: Option<FileIdentity>,
}

/// Where a *displaced* row is kept: one whose path a relink hands to another
/// row's file, and whose own file is nowhere to be found (issue #180).
///
/// One row per path means it cannot stay at its path, and deleting it would
/// take its playback progress on first sight, which nothing else in the
/// indexer does (issue #179). So it moves beside its old path, to a name no
/// scan ever indexes -- no video extension -- that no other row can hold,
/// since it names the row. It is missing there like any other gone file: a
/// relink can still find it by content, and a scan purges it after the
/// grace period.
pub fn displaced_path(path: &std::path::Path, id: Uuid) -> PathBuf {
    let mut displaced = path.as_os_str().to_os_string();
    displaced.push(format!(".{DISPLACED_MARKER}{id}"));
    PathBuf::from(displaced)
}

/// What [`displaced_path`] appends to a path, after a dot and before the
/// row's id.
const DISPLACED_MARKER: &str = "beam-displaced-";

/// The path a row kept at `path` was displaced from, if `path` is a
/// [`displaced_path`] -- its inverse. A displaced path never existed on
/// disk, so what is reported about such a row names the path it had.
pub fn displaced_from(path: &std::path::Path) -> Option<PathBuf> {
    let id = path.extension()?.to_str()?.strip_prefix(DISPLACED_MARKER)?;
    // Only the form `displaced_path` writes: an id spelled any other way is
    // part of some other name.
    let parsed = Uuid::try_parse(id).ok()?;
    (parsed.hyphenated().to_string() == id).then(|| path.with_extension(""))
}

/// What an [`UpdateMediaFile`] does to a file's probe results -- its MIME
/// type, duration, container format and container tags, which one probe
/// sets together.
///
/// A row with no duration is one whose probe has not succeeded, and the
/// indexer probes it again on every visit (FR-218). So a failed probe of
/// changed content must [`ProbeUpdate::Clear`] them: kept, the old content's
/// results would describe a file that is gone and the row would never be
/// probed again.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeUpdate {
    /// Leave them as they are.
    Keep,
    /// Replace them with a successful probe's.
    Set {
        mime_type: String,
        duration: Duration,
        container_format: String,
        container_tags: ContainerTags,
    },
    /// Clear them all: the content changed and its probe failed.
    Clear,
}

/// The `files.container_tags` value of `tags`.
#[cfg(feature = "entity")]
pub fn container_tags_json(tags: &ContainerTags) -> serde_json::Value {
    // A struct of strings and integers always serializes.
    serde_json::to_value(tags).unwrap_or_default()
}

#[cfg(feature = "entity")]
impl From<beam_entity::files::FileStatus> for FileStatus {
    fn from(status: beam_entity::files::FileStatus) -> Self {
        match status {
            beam_entity::files::FileStatus::Known => FileStatus::Known,
            beam_entity::files::FileStatus::Changed => FileStatus::Changed,
            beam_entity::files::FileStatus::Unknown => FileStatus::Unknown,
        }
    }
}

#[cfg(feature = "entity")]
impl From<FileStatus> for beam_entity::files::FileStatus {
    fn from(status: FileStatus) -> Self {
        match status {
            FileStatus::Known => beam_entity::files::FileStatus::Known,
            FileStatus::Changed => beam_entity::files::FileStatus::Changed,
            FileStatus::Unknown => beam_entity::files::FileStatus::Unknown,
        }
    }
}

#[cfg(feature = "entity")]
impl From<beam_entity::files::Model> for MediaFile {
    fn from(model: beam_entity::files::Model) -> Self {
        let content = model
            .movie_entry_id
            .map(|id| MediaFileContent::Movie {
                movie_entry_id: id,
                part_number: model.part_number.map(|n| n as u32),
            })
            .or_else(|| {
                model.episode_id.map(|id| MediaFileContent::Episode {
                    episode_id: id,
                    last_episode_number: model.last_episode_number.map(|n| n as u32),
                })
            });

        Self {
            id: model.id,
            library_id: model.library_id,
            path: PathBuf::from(model.file_path),
            hash: model.hash_xxh3 as u64,
            size_bytes: model.file_size as u64,
            mtime: model.mtime.map(|d| d.with_timezone(&Utc)),
            // Written together, so a row has both or neither (a CHECK
            // enforces it). The inode is a `BIGINT`: its bits, as
            // `hash_xxh3`'s are.
            identity: model
                .inode
                .zip(model.ctime)
                .map(|(inode, ctime)| FileIdentity {
                    inode: inode as u64,
                    ctime: ctime.with_timezone(&Utc),
                }),
            mime_type: model.mime_type,
            duration: model.duration_secs.map(Duration::from_secs_f64),
            container_format: model.container_format,
            content,
            status: FileStatus::from(model.file_status),
            scanned_at: model.scanned_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
            missing_since: model.missing_since.map(|d| d.with_timezone(&Utc)),
            classifier_version: model.classifier_version as u16,
            // The column holds only what `container_tags_json` wrote; a value
            // that does not read back is treated as no tags.
            container_tags: model
                .container_tags
                .and_then(|json| serde_json::from_value(json).ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::path::Path;

    /// Which paths are displaced ones, and the path each was displaced from.
    #[test]
    fn a_displaced_path_names_the_path_it_was_displaced_from() {
        let cases: [(&str, Option<&str>); 9] = [
            (
                "/lib/Old (1990).mkv.beam-displaced-fa9571c0-5b1e-4c2a-9d3e-0123456789ab",
                Some("/lib/Old (1990).mkv"),
            ),
            (
                "/lib/.hidden.beam-displaced-fa9571c0-5b1e-4c2a-9d3e-0123456789ab",
                Some("/lib/.hidden"),
            ),
            ("/lib/Old (1990).mkv", None),
            ("/lib/Old (1990).mkv.beam-displaced-", None),
            ("/lib/Old (1990).mkv.beam-displaced-not-an-id", None),
            // The same id, but not as `displaced_path` writes it.
            (
                "/lib/Old (1990).mkv.beam-displaced-fa9571c05b1e4c2a9d3e0123456789ab",
                None,
            ),
            (
                "/lib/Old (1990).mkv.beam-displaced-FA9571C0-5B1E-4C2A-9D3E-0123456789AB",
                None,
            ),
            (
                "/lib/Old (1990).mkv.other-fa9571c0-5b1e-4c2a-9d3e-0123456789ab",
                None,
            ),
            // The marker ends the name; it is not merely somewhere in it.
            (
                "/lib/x.beam-displaced-fa9571c0-5b1e-4c2a-9d3e-0123456789ab.mkv",
                None,
            ),
        ];
        for (path, expected) in cases {
            assert_eq!(
                displaced_from(Path::new(path)),
                expected.map(PathBuf::from),
                "{path}"
            );
        }
    }

    proptest! {
        /// A row displaced from any path reports that path back, and a path
        /// never displaced is not read as one.
        #[test]
        fn displacing_a_path_and_reading_it_back_is_the_identity(
            dirs in prop::collection::vec("[A-Za-z0-9 ()_-]{1,12}", 0..4),
            name in "[A-Za-z0-9 ()_-][A-Za-z0-9 ()._-]{0,24}",
            id in any::<u128>(),
        ) {
            let mut path = PathBuf::from("/lib");
            path.extend(&dirs);
            path.push(&name);
            let displaced = displaced_path(&path, Uuid::from_u128(id));
            prop_assert_eq!(displaced_from(&displaced), Some(path.clone()));
            prop_assert_eq!(displaced_from(&path), None);
        }
    }
}
