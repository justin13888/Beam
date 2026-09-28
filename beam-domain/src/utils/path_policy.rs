//! Which files under a library root the indexer indexes.
//!
//! A library root holds more than media: hidden files, NAS housekeeping
//! folders, subtitles and artwork beside the video, trailers and samples, and
//! whatever else an administrator has told Beam to leave alone. The policy
//! decides from a path relative to the root alone -- nothing is read from
//! disk -- so the full scan and the watcher decide identically. The one thing
//! a path cannot say is which stream files of a DVD or Blu-ray disc structure
//! its main title plays; the policy marks the candidates
//! ([`PathDisposition::DiscStream`]) and the indexer reads the disc for the
//! rest (issue #234).

use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::utils::filename::parse_media_filename;
use crate::utils::media_path::season_folder_number;

/// Extensions of the video files Beam indexes, lowercase.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "m4v", "ts", "m2ts", "mts", "flv", "wmv", "3gp", "ogv",
    "mpg", "mpeg", "divx", "asf", "f4v",
];

/// Extensions of the files that travel beside a video -- subtitles, metadata
/// and artwork -- lowercase. Not indexed as media; recognised so they can be
/// told apart from files Beam has no use for at all.
pub const SIDECAR_EXTENSIONS: &[&str] = &[
    "srt", "ass", "ssa", "sub", "idx", "vtt", "sup", "smi", "nfo", "jpg", "jpeg", "png", "webp",
    "tbn",
];

/// Folders NAS appliances and operating systems create for their own use.
/// Matched case-insensitively at any depth.
const SYSTEM_DIRECTORIES: &[&str] = &[
    "@eadir",
    "#recycle",
    "$recycle.bin",
    "system volume information",
    "lost+found",
];

/// The folders that sit beside a disc structure's own and hold nothing
/// Beam plays: a DVD's `AUDIO_TS/` (DVD-Audio, empty on a video disc) and a
/// Blu-ray's `CERTIFICATE/` (AACS data and its backups). Matched
/// case-insensitively at any depth.
const DISC_COMPANION_DIRECTORIES: &[&str] = &["audio_ts", "certificate"];

/// A DVD or Blu-ray disc structure copied whole: a `VIDEO_TS/` or `BDMV/`
/// folder (issue #234).
///
/// What is inside one is not a set of titles -- a DVD splits one film into
/// `VTS_01_1.VOB`, `VTS_01_2.VOB`, ...; a Blu-ray's `BDMV/STREAM/` holds
/// `00001.m2ts` and friends -- so no file in it is judged by its own name,
/// which would invent films named `VTS 01 1` or `00001` and merge every
/// disc's same-numbered file into one. The disc is one source of the title
/// its enclosing folder names, and which of its stream files that source
/// plays -- its main title -- is read from the disc itself, which a path
/// cannot say. So the policy only marks a disc's candidate stream files
/// ([`PathDisposition::DiscStream`]) and keeps the rest of it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscKind {
    /// A `VIDEO_TS/` folder: its main title set's `VTS_nn_1.VOB`,
    /// `VTS_nn_2.VOB`, ... play in turn.
    Dvd,
    /// A `BDMV/` folder: the clips in `BDMV/STREAM/` its main playlist plays.
    BluRay,
}

impl DiscKind {
    /// The kind of disc structure a folder named `name` is the root of, in
    /// any case.
    pub fn of_folder(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("video_ts") {
            Some(DiscKind::Dvd)
        } else if name.eq_ignore_ascii_case("bdmv") {
            Some(DiscKind::BluRay)
        } else {
            None
        }
    }
}

/// The title set and part a DVD title file is named for, in any case:
/// `VTS_03_2.VOB` is part 2 of title set 3. Only a title set's content
/// files (parts 1 to 9) are named; its menu (`VTS_03_0.VOB`) and the disc's
/// own (`VIDEO_TS.VOB`) are not, and nor is a name of any other shape, such
/// as an AppleDouble `._VTS_01_1.VOB`.
pub fn dvd_title_file(file_name: &str) -> Option<(u32, u32)> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("vob") {
        return None;
    }
    let prefix = stem.get(..4)?;
    if !prefix.eq_ignore_ascii_case("vts_") {
        return None;
    }
    let (set, part) = stem[4..].split_once('_')?;
    if set.len() != 2 || part.len() != 1 {
        return None;
    }
    let set = set.parse::<u32>().ok().filter(|set| *set >= 1)?;
    let part = part.parse::<u32>().ok().filter(|part| *part >= 1)?;
    Some((set, part))
}

/// The kind of disc a stream file at `path` belongs to, judged from where
/// it sits alone: a DVD title file ([`dvd_title_file`]) directly in a
/// `VIDEO_TS/` folder, or an `.m2ts` clip directly in a `BDMV/STREAM/`
/// folder. `path` may be relative to a library root or absolute.
pub fn disc_stream_kind(path: &Path) -> Option<DiscKind> {
    let file_name = path.file_name()?.to_string_lossy();
    let parent = path.parent()?;
    let parent_name = parent.file_name()?.to_string_lossy();
    if DiscKind::of_folder(&parent_name) == Some(DiscKind::Dvd) {
        return dvd_title_file(&file_name).map(|_| DiscKind::Dvd);
    }
    let disc_folder = parent.parent()?.file_name()?.to_string_lossy();
    let is_clip = parent_name.eq_ignore_ascii_case("stream")
        && DiscKind::of_folder(&disc_folder) == Some(DiscKind::BluRay)
        && !file_name.starts_with('.')
        && lowercase_extension(&file_name).as_deref() == Some("m2ts");
    is_clip.then_some(DiscKind::BluRay)
}

/// Folders that hold a title's extras rather than the title (the Plex and
/// Jellyfin conventions), and could not plausibly be anything else. Matched
/// case-insensitively below the top level, whatever the folder above them:
/// `Breaking Bad/Extras/` is a yearless show's extras, and a release folder's
/// `Sample/` holds its sample.
const EXTRAS_DIRECTORIES: &[&str] = &[
    "extras",
    "featurettes",
    "behind the scenes",
    "deleted scenes",
    "interviews",
    "sample",
    "samples",
    "bonus",
];

/// Extras folder names that can also name a category, so are excluded only
/// inside a title folder: `Movie (2019)/Trailers/` holds trailers, but a
/// top-level `Shorts/` folder is a collection of short films, and
/// `Movies/Trailers/` -- directly under a category folder -- a collection of
/// trailers.
const CATEGORY_OR_EXTRAS_DIRECTORIES: &[&str] = &["trailers", "shorts", "other", "scenes"];

/// Filename-stem suffixes that mark an extra (`Movie-trailer.mkv`), matched
/// case-insensitively.
const EXTRA_FILE_SUFFIXES: &[&str] = &[
    "-trailer",
    "-sample",
    ".sample",
    "_sample",
    "-featurette",
    "-behindthescenes",
    "-deleted",
    "-interview",
];

/// Filename stems that are an extra on their own.
const EXTRA_FILE_STEMS: &[&str] = &["sample", "trailer"];

/// What the indexer does with a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathDisposition {
    /// A video file: indexed.
    Media,
    /// A subtitle, metadata or artwork file: not indexed as media.
    Sidecar,
    /// A stream file of a disc structure ([`DiscKind`]): indexed when the
    /// disc's main title plays it, which only reading the disc can tell.
    DiscStream(DiscKind),
    /// Something the policy keeps out of the library, and why.
    Excluded(ExclusionReason),
    /// A file Beam has no use for.
    Ignored,
}

/// Why a path is kept out of the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionReason {
    /// A path component starts with `.`.
    Hidden,
    /// Under a NAS or operating-system housekeeping folder.
    SystemDirectory,
    /// Inside a DVD or Blu-ray disc structure (`VIDEO_TS/`, `BDMV/`) but no
    /// stream file of it -- a menu, an IFO, a playlist -- or inside a folder
    /// beside one (`AUDIO_TS/`, `CERTIFICATE/`).
    DiscStructure,
    /// Under an extras folder (`Trailers/`, `Featurettes/`, ...).
    ExtrasDirectory,
    /// Named as an extra (`Movie-trailer.mkv`, `sample.mkv`).
    ExtraFile,
    /// Matched by an administrator's ignore pattern.
    IgnorePattern,
}

/// An administrator's ignore pattern that is not a valid glob.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid ignore pattern {pattern:?}: {reason}")]
pub struct InvalidIgnorePattern {
    pub pattern: String,
    pub reason: String,
}

/// The rules deciding which paths under a library root are indexed.
#[derive(Debug, Clone, Default)]
pub struct PathPolicy {
    ignore: Vec<glob::Pattern>,
}

/// Case-insensitive, and `*` never crosses a `/`, so `*.partial.mkv` matches
/// only at the root and `**/*.partial.mkv` at any depth. A pattern that
/// matches a directory excludes everything beneath it: `Downloads`,
/// `Downloads/*` and `Downloads/**` all ignore everything under `Downloads`
/// (the middle one by matching each of its subdirectories).
const GLOB_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: false,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

impl PathPolicy {
    /// A policy that also excludes every path matching one of `ignore_globs`,
    /// each matched against the path relative to the library root. A pattern
    /// that matches a directory excludes everything beneath it.
    pub fn new<I, S>(ignore_globs: I) -> Result<Self, InvalidIgnorePattern>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let ignore = ignore_globs
            .into_iter()
            .map(|pattern| {
                let pattern = pattern.as_ref();
                glob::Pattern::new(pattern).map_err(|err| InvalidIgnorePattern {
                    pattern: pattern.to_string(),
                    reason: err.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { ignore })
    }

    /// What the indexer does with the file at `rel_path`, relative to its
    /// library root.
    pub fn disposition(&self, rel_path: &Path) -> PathDisposition {
        let components = normal_components(rel_path);
        let Some((file_name, dirs)) = components.split_last() else {
            return PathDisposition::Ignored;
        };
        let disc = match self.beneath(dirs) {
            Beneath::Excluded(reason) => return PathDisposition::Excluded(reason),
            Beneath::Disc { at, kind } => Some((at, kind)),
            Beneath::Open => None,
        };
        if file_name.starts_with('.') {
            return PathDisposition::Excluded(ExclusionReason::Hidden);
        }
        if let Some((at, kind)) = disc {
            if self.ignored(rel_path) {
                return PathDisposition::Excluded(ExclusionReason::IgnorePattern);
            }
            // A stream file directly where its disc keeps them: in the
            // `VIDEO_TS/` folder itself, or in the `STREAM/` folder of the
            // `BDMV/` one.
            let depth_below_root = match kind {
                DiscKind::Dvd => 1,
                DiscKind::BluRay => 2,
            };
            if dirs.len() == at + depth_below_root && disc_stream_kind(rel_path) == Some(kind) {
                return PathDisposition::DiscStream(kind);
            }
            return PathDisposition::Excluded(ExclusionReason::DiscStructure);
        }
        if is_extra_file(file_name) {
            return PathDisposition::Excluded(ExclusionReason::ExtraFile);
        }
        if self.ignored(rel_path) {
            return PathDisposition::Excluded(ExclusionReason::IgnorePattern);
        }
        match lowercase_extension(file_name).as_deref() {
            Some(ext) if VIDEO_EXTENSIONS.contains(&ext) => PathDisposition::Media,
            Some(ext) if SIDECAR_EXTENSIONS.contains(&ext) => PathDisposition::Sidecar,
            _ => PathDisposition::Ignored,
        }
    }

    /// Whether nothing beneath the directory at `rel_dir` can be indexed, so a
    /// walk need not descend into it. A disc structure's folders are not
    /// excluded: its main title is indexed (see [`Self::disc_root`]).
    pub fn excludes_directory(&self, rel_dir: &Path) -> bool {
        matches!(
            self.beneath(&normal_components(rel_dir)),
            Beneath::Excluded(_)
        )
    }

    /// The disc structure `rel_path` -- a file or a folder, relative to the
    /// library root -- is, or lies inside: the disc's root folder
    /// (`Heat (1995)/VIDEO_TS`, relative to the library root) and its kind.
    /// `None` outside any, and inside one the policy excludes (a disc in an
    /// `Extras/` folder, or matched by an ignore pattern).
    pub fn disc_root(&self, rel_path: &Path) -> Option<(PathBuf, DiscKind)> {
        let components = normal_components(rel_path);
        match self.beneath(&components) {
            Beneath::Disc { at, kind } => Some((components[..=at].iter().collect(), kind)),
            Beneath::Excluded(_) | Beneath::Open => None,
        }
    }

    /// What the directories `dirs` (root first) make of what is beneath
    /// them: the first of them that excludes it, or the disc structure whose
    /// root one of them is. Nothing inside a disc's root is judged by these
    /// rules -- its `STREAM/` or `BACKUP/` folder is the disc's, not an
    /// extras folder -- except an administrator's ignore patterns.
    fn beneath(&self, dirs: &[String]) -> Beneath {
        let mut prefix = PathBuf::new();
        for (depth, dir) in dirs.iter().enumerate() {
            let lower = dir.to_lowercase();
            if dir.starts_with('.') {
                return Beneath::Excluded(ExclusionReason::Hidden);
            }
            if SYSTEM_DIRECTORIES.contains(&lower.as_str()) {
                return Beneath::Excluded(ExclusionReason::SystemDirectory);
            }
            if DISC_COMPANION_DIRECTORIES.contains(&lower.as_str()) {
                return Beneath::Excluded(ExclusionReason::DiscStructure);
            }
            if let Some(kind) = DiscKind::of_folder(dir) {
                prefix.push(dir);
                if self.ignored(&prefix) {
                    return Beneath::Excluded(ExclusionReason::IgnorePattern);
                }
                // A pattern matching a folder inside the disc (`**/STREAM`)
                // still excludes what is beneath it.
                for inner in &dirs[depth + 1..] {
                    prefix.push(inner);
                    if self.ignored(&prefix) {
                        return Beneath::Excluded(ExclusionReason::IgnorePattern);
                    }
                }
                return Beneath::Disc { at: depth, kind };
            }
            if depth >= 1
                && (EXTRAS_DIRECTORIES.contains(&lower.as_str())
                    || (CATEGORY_OR_EXTRAS_DIRECTORIES.contains(&lower.as_str())
                        && is_title_folder(dirs, depth - 1)))
            {
                return Beneath::Excluded(ExclusionReason::ExtrasDirectory);
            }
            prefix.push(dir);
            if self.ignored(&prefix) {
                return Beneath::Excluded(ExclusionReason::IgnorePattern);
            }
        }
        Beneath::Open
    }

    fn ignored(&self, rel_path: &Path) -> bool {
        self.ignore
            .iter()
            .any(|pattern| pattern.matches_path_with(rel_path, GLOB_OPTIONS))
    }
}

/// Whether `path` names a video file: one with a video extension, or a
/// disc structure's stream file ([`disc_stream_kind`]) -- a DVD's
/// `VTS_01_1.VOB` is video though a loose `.vob` is not indexed. The name
/// and folder alone: whether the file is excluded, or played by its disc's
/// main title, is [`PathPolicy::disposition`]'s question and the disc's.
pub fn is_video_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| lowercase_extension(&name.to_string_lossy()))
        .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
        || disc_stream_kind(path).is_some()
}

/// What a library's folders make of what is beneath them; see
/// [`PathPolicy::beneath`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Beneath {
    /// Nothing beneath is indexed, for this reason.
    Excluded(ExclusionReason),
    /// The folder at `at` is the root of a disc structure of `kind`.
    Disc { at: usize, kind: DiscKind },
    /// Each file beneath is judged on its own.
    Open,
}

/// Whether `dirs[at]` is a title's folder rather than a category holding
/// titles: a folder naming a title and its year (`Movie (2019)`), a season
/// folder (and so a show's), or any folder below the top level -- a top-level
/// folder without a year (`Movies`, `TV`) is taken for a category.
fn is_title_folder(dirs: &[String], at: usize) -> bool {
    let dir = &dirs[at];
    at >= 1 || season_folder_number(dir).is_some() || parse_media_filename(dir).year.is_some()
}

fn normal_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn lowercase_extension(file_name: &str) -> Option<String> {
    Path::new(file_name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
}

fn is_extra_file(file_name: &str) -> bool {
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    EXTRA_FILE_STEMS.contains(&stem.as_str())
        || EXTRA_FILE_SUFFIXES
            .iter()
            .any(|suffix| stem.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disposition(policy: &PathPolicy, path: &str) -> PathDisposition {
        policy.disposition(Path::new(path))
    }

    #[test]
    fn what_the_default_policy_does_with_each_path() {
        use ExclusionReason::*;
        use PathDisposition::*;

        let cases = [
            ("Movie (2019)/Movie.2019.mkv", Media),
            ("Show/Season 01/Show.S01E01.MKV", Media),
            ("Movie.mts", Media),
            ("Movie (2019)/Movie.2019.en.srt", Sidecar),
            ("Movie (2019)/movie.nfo", Sidecar),
            ("Movie (2019)/poster.jpg", Sidecar),
            ("Movie (2019)/notes.txt", Ignored),
            ("Movie (2019)/README", Ignored),
            // Hidden, at any depth.
            (".hidden.mkv", Excluded(Hidden)),
            ("Movie/.Movie.mkv", Excluded(Hidden)),
            (".trash/Movie.mkv", Excluded(Hidden)),
            ("Show/.snapshots/Season 1/x.mkv", Excluded(Hidden)),
            // NAS and OS housekeeping.
            (
                "@eaDir/Movie.mkv/SYNOVIDEO_VIDEO_SCREENSHOT.jpg",
                Excluded(SystemDirectory),
            ),
            ("Movie/@eaDir/Movie.mkv", Excluded(SystemDirectory)),
            ("#recycle/Movie.mkv", Excluded(SystemDirectory)),
            ("$RECYCLE.BIN/Movie.mkv", Excluded(SystemDirectory)),
            ("System Volume Information/x.mkv", Excluded(SystemDirectory)),
            ("lost+found/x.mkv", Excluded(SystemDirectory)),
            // A disc structure copied whole, at any depth and in any case:
            // its title files are candidates for its main title, and
            // nothing else in it is media.
            (
                "Movies/Heat (1995)/VIDEO_TS/VTS_01_1.VOB",
                DiscStream(DiscKind::Dvd),
            ),
            ("VIDEO_TS/VTS_01_2.VOB", DiscStream(DiscKind::Dvd)),
            (
                "Heat (1995)/video_ts/vts_02_9.vob",
                DiscStream(DiscKind::Dvd),
            ),
            (
                "Heat.1995.DVD9/VIDEO_TS/VIDEO_TS.VOB",
                Excluded(DiscStructure),
            ),
            ("Heat (1995)/video_ts/vts_01_0.vob", Excluded(DiscStructure)),
            ("Heat (1995)/VIDEO_TS/VTS_01_0.IFO", Excluded(DiscStructure)),
            ("Heat (1995)/VIDEO_TS/._VTS_01_1.VOB", Excluded(Hidden)),
            ("Heat (1995)/VIDEO_TS/Heat.mkv", Excluded(DiscStructure)),
            (
                "Heat (1995)/VIDEO_TS/x/VTS_01_1.VOB",
                Excluded(DiscStructure),
            ),
            ("Heat (1995)/AUDIO_TS/x.mkv", Excluded(DiscStructure)),
            (
                "Heat (1995)/BDMV/STREAM/00001.m2ts",
                DiscStream(DiscKind::BluRay),
            ),
            ("BDMV/STREAM/00001.M2TS", DiscStream(DiscKind::BluRay)),
            (
                "Heat (1995)/BDMV/PLAYLIST/00001.mpls",
                Excluded(DiscStructure),
            ),
            ("Heat (1995)/BDMV/00001.m2ts", Excluded(DiscStructure)),
            (
                "Heat (1995)/BDMV/BACKUP/STREAM/00001.m2ts",
                Excluded(DiscStructure),
            ),
            (
                "Heat (1995)/CERTIFICATE/BACKUP/x.m2ts",
                Excluded(DiscStructure),
            ),
            // A disc is judged by where it is, like any other file.
            (
                "Heat (1995)/Extras/VIDEO_TS/VTS_01_1.VOB",
                Excluded(ExtrasDirectory),
            ),
            (
                ".trash/Heat (1995)/BDMV/STREAM/00001.m2ts",
                Excluded(Hidden),
            ),
            // A VOB outside one is not a video Beam indexes.
            ("Heat (1995)/Heat.vob", Ignored),
            // Extras folders below the top level.
            (
                "Movie (2019)/Extras/Making Of.mkv",
                Excluded(ExtrasDirectory),
            ),
            (
                "Movie (2019)/Behind The Scenes/x.mkv",
                Excluded(ExtrasDirectory),
            ),
            ("Movie (2019)/Featurettes/x.mkv", Excluded(ExtrasDirectory)),
            (
                "Show/Season 1/Deleted Scenes/x.mkv",
                Excluded(ExtrasDirectory),
            ),
            ("Movie (2019)/Sample/movie.mkv", Excluded(ExtrasDirectory)),
            ("Movie (2019)/Trailers/t.mkv", Excluded(ExtrasDirectory)),
            // A part token does not make an extra a part of the film
            // (issue #233): it is excluded before anything reads it.
            (
                "Movie (2019)/Extras/Movie (2019) - CD2.mkv",
                Excluded(ExtrasDirectory),
            ),
            (
                "Movie (2019)/Trailers/Movie (2019) - Part 2.mkv",
                Excluded(ExtrasDirectory),
            ),
            ("TV/Show/Extras/Making Of.mkv", Excluded(ExtrasDirectory)),
            ("Season 1/Featurettes/x.mkv", Excluded(ExtrasDirectory)),
            // A name that can only mean extras is excluded under any folder:
            // a yearless show or movie folder, a scene release's folder.
            (
                "Breaking Bad/Extras/Making of Breaking Bad.mkv",
                Excluded(ExtrasDirectory),
            ),
            (
                "Breaking Bad/Featurettes/Inside Episode 1.mkv",
                Excluded(ExtrasDirectory),
            ),
            (
                "Stranger Things/Behind the Scenes/Making of.mkv",
                Excluded(ExtrasDirectory),
            ),
            (
                "Breaking Bad/Deleted Scenes/Scene 1.mkv",
                Excluded(ExtrasDirectory),
            ),
            ("The Matrix/Extras/Making of.mkv", Excluded(ExtrasDirectory)),
            (
                "Show.S01E01.720p-GRP/Sample/sample-show.s01e01.720p-grp.mkv",
                Excluded(ExtrasDirectory),
            ),
            ("Movies/Samples/x.mkv", Excluded(ExtrasDirectory)),
            // A name that can also mean a category needs a title folder: at
            // the top level it is a collection, and so directly under a
            // category folder.
            (
                "Breaking Bad (2008)/Trailers/t.mkv",
                Excluded(ExtrasDirectory),
            ),
            ("Show/Season 1/Scenes/x.mkv", Excluded(ExtrasDirectory)),
            ("Shorts/Short Film (2019).mkv", Media),
            ("Trailers/x.mkv", Media),
            ("Extras/x.mkv", Media),
            ("Movies/Trailers/Teaser (2019).mkv", Media),
            ("Movies/Shorts/Short Film (2019).mkv", Media),
            ("Movies/Other/Film (2019).mkv", Media),
            ("Movies/Scenes/Film (2019).mkv", Media),
            // Extras by name.
            ("Movie (2019)/Movie-trailer.mkv", Excluded(ExtraFile)),
            ("Movie (2019)/movie-sample.mkv", Excluded(ExtraFile)),
            ("Movie (2019)/movie.sample.mkv", Excluded(ExtraFile)),
            ("Movie (2019)/movie_SAMPLE.mkv", Excluded(ExtraFile)),
            ("Movie (2019)/x-featurette.mkv", Excluded(ExtraFile)),
            ("Movie (2019)/x-behindthescenes.mkv", Excluded(ExtraFile)),
            ("Movie (2019)/x-deleted.mkv", Excluded(ExtraFile)),
            ("Movie (2019)/x-interview.mkv", Excluded(ExtraFile)),
            ("Movie (2019)/sample.mkv", Excluded(ExtraFile)),
            ("Movie (2019)/Trailer.mkv", Excluded(ExtraFile)),
            // Words that merely contain those names are not extras.
            ("Samples of Life (2020).mkv", Media),
            ("The Trailer Park (2010).mkv", Media),
        ];
        let policy = PathPolicy::default();
        for (path, expected) in cases {
            assert_eq!(disposition(&policy, path), expected, "{path}");
        }
    }

    #[test]
    fn ignore_patterns_exclude_matching_files_and_everything_under_matching_directories() {
        let policy = PathPolicy::new(["Downloads", "**/*.partial.mkv", "kids/**"]).unwrap();
        let excluded = PathDisposition::Excluded(ExclusionReason::IgnorePattern);

        assert_eq!(disposition(&policy, "Downloads/x.mkv"), excluded);
        assert_eq!(disposition(&policy, "downloads/deep/x.mkv"), excluded);
        assert_eq!(disposition(&policy, "Movie/Movie.partial.mkv"), excluded);
        assert_eq!(disposition(&policy, "Kids/Film.mkv"), excluded);
        assert!(policy.excludes_directory(Path::new("Downloads")));
        assert!(!policy.excludes_directory(Path::new("Movies")));

        // `*` stops at a separator, and a pattern matches from the root.
        assert_eq!(
            disposition(&policy, "Movies/Downloads.mkv"),
            PathDisposition::Media
        );
        assert_eq!(
            disposition(&policy, "Movies/Downloads/x.mkv"),
            PathDisposition::Media
        );
    }

    /// `Dir/*` matches each subdirectory of `Dir` too, and a matched
    /// directory excludes everything beneath it: the pattern reaches the
    /// whole tree, not only the files directly in `Dir`.
    #[test]
    fn a_single_star_under_a_directory_reaches_its_whole_tree() {
        let policy = PathPolicy::new(["Stash/*"]).unwrap();
        let excluded = PathDisposition::Excluded(ExclusionReason::IgnorePattern);

        assert_eq!(disposition(&policy, "Stash/x.mkv"), excluded);
        assert_eq!(disposition(&policy, "Stash/deep/er/x.mkv"), excluded);
        assert!(policy.excludes_directory(Path::new("Stash/deep")));
        assert_eq!(
            disposition(&policy, "Movies/Stash.mkv"),
            PathDisposition::Media
        );
    }

    #[test]
    fn an_invalid_ignore_pattern_is_refused_and_named() {
        let err = PathPolicy::new(["ok/*", "bad[pattern"]).unwrap_err();
        assert_eq!(err.pattern, "bad[pattern");
    }

    #[test]
    fn excluded_directories_are_pruned_and_ordinary_ones_are_not() {
        let policy = PathPolicy::default();
        for dir in [
            ".git",
            "@eaDir",
            "Movie (2019)/Extras",
            "Show/Extras",
            "Show/.snapshots",
            "Heat (1995)/AUDIO_TS",
            "Heat (1995)/CERTIFICATE",
            "Movie (2019)/Extras/VIDEO_TS",
        ] {
            assert!(policy.excludes_directory(Path::new(dir)), "{dir}");
        }
        for dir in [
            "Movie (2019)",
            "Extras",
            "Show/Season 01",
            "Shorts",
            "Movies/Trailers",
            // A disc's folders hold its main title.
            "Heat (1995)/VIDEO_TS",
            "Heat (1995)/BDMV",
            "Heat (1995)/BDMV/STREAM",
            "Heat (1995)/BDMV/BACKUP",
        ] {
            assert!(!policy.excludes_directory(Path::new(dir)), "{dir}");
        }
    }

    /// The disc a path is, or is inside, is its first `VIDEO_TS` or `BDMV`
    /// folder -- unless the policy keeps that folder out.
    #[test]
    fn a_path_inside_a_disc_names_its_disc_root() {
        let policy = PathPolicy::new(["Stash", "**/Skipped (2001)/BDMV"]).unwrap();
        let cases: [(&str, Option<(&str, DiscKind)>); 9] = [
            (
                "Heat (1995)/VIDEO_TS/VTS_01_1.VOB",
                Some(("Heat (1995)/VIDEO_TS", DiscKind::Dvd)),
            ),
            (
                "Heat (1995)/VIDEO_TS",
                Some(("Heat (1995)/VIDEO_TS", DiscKind::Dvd)),
            ),
            (
                "Movies/Heat (1995)/bdmv/STREAM",
                Some(("Movies/Heat (1995)/bdmv", DiscKind::BluRay)),
            ),
            ("VIDEO_TS/VTS_01_1.VOB", Some(("VIDEO_TS", DiscKind::Dvd))),
            ("Heat (1995)", None),
            ("Heat (1995)/Heat.mkv", None),
            ("Heat (1995)/Extras/VIDEO_TS/VTS_01_1.VOB", None),
            ("Stash/Heat (1995)/VIDEO_TS/VTS_01_1.VOB", None),
            ("Movies/Skipped (2001)/BDMV/STREAM/00001.m2ts", None),
        ];
        for (path, expected) in cases {
            assert_eq!(
                policy.disc_root(Path::new(path)),
                expected.map(|(root, kind)| (PathBuf::from(root), kind)),
                "{path}"
            );
        }
    }

    /// An ignore pattern reaches inside a disc: a folder or a file of it the
    /// administrator ignored is not a stream the disc offers.
    #[test]
    fn an_ignore_pattern_reaches_inside_a_disc() {
        let policy = PathPolicy::new(["**/STREAM", "**/VTS_02_*.VOB"]).unwrap();
        let excluded = PathDisposition::Excluded(ExclusionReason::IgnorePattern);
        assert_eq!(
            disposition(&policy, "Heat (1995)/BDMV/STREAM/00001.m2ts"),
            excluded
        );
        assert!(policy.excludes_directory(Path::new("Heat (1995)/BDMV/STREAM")));
        assert_eq!(
            disposition(&policy, "Heat (1995)/VIDEO_TS/VTS_02_1.VOB"),
            excluded
        );
        assert_eq!(
            disposition(&policy, "Heat (1995)/VIDEO_TS/VTS_01_1.VOB"),
            PathDisposition::DiscStream(DiscKind::Dvd)
        );
    }

    #[test]
    fn a_dvd_title_file_names_its_title_set_and_part() {
        let cases: [(&str, Option<(u32, u32)>); 12] = [
            ("VTS_01_1.VOB", Some((1, 1))),
            ("vts_12_9.vob", Some((12, 9))),
            ("VTS_99_2.Vob", Some((99, 2))),
            // A menu, the disc's own files and the title set's IFO.
            ("VTS_01_0.VOB", None),
            ("VIDEO_TS.VOB", None),
            ("VTS_01_0.IFO", None),
            ("VTS_00_1.VOB", None),
            ("VTS_1_1.VOB", None),
            ("VTS_01_10.VOB", None),
            ("VTS_01_1.mkv", None),
            ("._VTS_01_1.VOB", None),
            ("VTS-01-1.VOB", None),
        ];
        for (name, expected) in cases {
            assert_eq!(dvd_title_file(name), expected, "{name}");
        }
    }

    /// A disc's stream files are video for the empty-root guard, wherever
    /// the library is mounted; a loose `.vob` is not.
    #[test]
    fn a_disc_stream_file_is_a_video_file() {
        for path in [
            "/mnt/lib/Heat (1995)/VIDEO_TS/VTS_01_1.VOB",
            "Heat (1995)/BDMV/STREAM/00001.m2ts",
            "Heat (1995)/Heat.mkv",
        ] {
            assert!(is_video_path(Path::new(path)), "{path}");
        }
        for path in [
            "Heat (1995)/Heat.vob",
            "Heat (1995)/VIDEO_TS/VIDEO_TS.VOB",
            "Heat (1995)/VIDEO_TS/VTS_01_0.IFO",
        ] {
            assert!(!is_video_path(Path::new(path)), "{path}");
        }
        assert_eq!(
            disc_stream_kind(Path::new("/mnt/lib/Heat (1995)/BDMV/STREAM/00001.m2ts")),
            Some(DiscKind::BluRay)
        );
        assert_eq!(
            disc_stream_kind(Path::new("/mnt/lib/Heat (1995)/STREAM/00001.m2ts")),
            None
        );
    }

    /// Every file the default policy calls media has a video extension, and
    /// every file with one that it does not call media is excluded -- the
    /// extension list and the disposition cannot disagree.
    #[test]
    fn media_is_exactly_the_video_extensions_that_are_not_excluded() {
        let policy = PathPolicy::default();
        for ext in VIDEO_EXTENSIONS {
            let path = format!("Movie (2019)/Movie.{ext}");
            assert!(is_video_path(Path::new(&path)), "{path}");
            assert_eq!(
                disposition(&policy, &path),
                PathDisposition::Media,
                "{path}"
            );
        }
        for ext in SIDECAR_EXTENSIONS {
            let path = format!("Movie (2019)/Movie.{ext}");
            assert!(!is_video_path(Path::new(&path)), "{path}");
        }
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// A path with any hidden component is never indexed.
            #[test]
            fn a_hidden_component_is_always_excluded(
                before in proptest::collection::vec("[A-Za-z0-9 ]{1,8}", 0..3),
                hidden in "\\.[A-Za-z0-9 ]{1,8}",
                after in proptest::collection::vec("[A-Za-z0-9 ]{1,8}", 0..3),
                ext in proptest::sample::select(VIDEO_EXTENSIONS),
            ) {
                let mut parts = before.clone();
                parts.push(hidden.clone());
                parts.extend(after.iter().cloned());
                let path = format!("{}.{ext}", parts.join("/"));
                prop_assert!(
                    matches!(
                        PathPolicy::default().disposition(Path::new(&path)),
                        PathDisposition::Excluded(_)
                    ),
                    "{path}"
                );
            }

            /// A file the policy offers as a disc's stream is inside the
            /// disc the policy names for it, of the same kind, and is a
            /// video file -- the three questions the indexer asks of one
            /// disc never disagree.
            #[test]
            fn a_disc_stream_is_inside_the_disc_the_policy_names(
                folders in proptest::collection::vec(
                    proptest::sample::select(vec![
                        "Heat (1995)", "VIDEO_TS", "video_ts", "BDMV", "STREAM", "Extras",
                        ".hidden", "AUDIO_TS", "BACKUP", "Movies",
                    ]),
                    0..5,
                ),
                file in proptest::sample::select(vec![
                    "VTS_01_1.VOB", "vts_02_9.vob", "VTS_01_0.VOB", "VIDEO_TS.VOB",
                    "00001.m2ts", "00800.M2TS", "Heat.mkv", "._VTS_01_1.VOB",
                ]),
            ) {
                let mut path = PathBuf::from_iter(&folders);
                path.push(file);
                if let PathDisposition::DiscStream(kind) =
                    PathPolicy::default().disposition(&path)
                {
                    let (root, root_kind) = PathPolicy::default()
                        .disc_root(&path)
                        .expect("a disc stream is inside a disc");
                    prop_assert_eq!(root_kind, kind);
                    prop_assert!(path.starts_with(&root));
                    prop_assert_eq!(disc_stream_kind(&path), Some(kind));
                    prop_assert!(is_video_path(&path));
                }
            }

            #[test]
            fn disposition_never_panics(path in ".*") {
                let _ = PathPolicy::default().disposition(Path::new(&path));
            }
        }
    }
}
