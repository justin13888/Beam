//! Which files under a library root the indexer indexes.
//!
//! A library root holds more than media: hidden files, NAS housekeeping
//! folders, subtitles and artwork beside the video, trailers and samples, and
//! whatever else an administrator has told Beam to leave alone. The policy
//! decides from a path relative to the root alone -- nothing is read from
//! disk -- so the full scan and the watcher decide identically.

use std::path::{Component, Path};

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

/// The folders of a DVD or Blu-ray disc structure copied whole. What is
/// inside them is not a title -- a DVD splits one film into
/// `VTS_01_1.VOB`, `VTS_01_2.VOB`, ...; a Blu-ray's `BDMV/STREAM/` holds
/// `00001.m2ts` and friends -- so indexing the files as they stand invents
/// films named `VTS 01 1` or `00001`, and merges every disc's same-numbered
/// file into one. Matched case-insensitively at any depth; playing a disc
/// structure as its enclosing title is issue #234's.
const DISC_STRUCTURE_DIRECTORIES: &[&str] = &["video_ts", "audio_ts", "bdmv", "certificate"];

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
    /// Inside a DVD or Blu-ray disc structure (`VIDEO_TS/`, `BDMV/`).
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
        if let Some(reason) = self.directory_exclusion(dirs) {
            return PathDisposition::Excluded(reason);
        }
        if file_name.starts_with('.') {
            return PathDisposition::Excluded(ExclusionReason::Hidden);
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
    /// walk need not descend into it.
    pub fn excludes_directory(&self, rel_dir: &Path) -> bool {
        self.directory_exclusion(&normal_components(rel_dir))
            .is_some()
    }

    /// Why the directories `dirs` (root first) exclude what is beneath them,
    /// if they do.
    fn directory_exclusion(&self, dirs: &[String]) -> Option<ExclusionReason> {
        let mut prefix = std::path::PathBuf::new();
        for (depth, dir) in dirs.iter().enumerate() {
            let lower = dir.to_lowercase();
            if dir.starts_with('.') {
                return Some(ExclusionReason::Hidden);
            }
            if SYSTEM_DIRECTORIES.contains(&lower.as_str()) {
                return Some(ExclusionReason::SystemDirectory);
            }
            if DISC_STRUCTURE_DIRECTORIES.contains(&lower.as_str()) {
                return Some(ExclusionReason::DiscStructure);
            }
            if depth >= 1
                && (EXTRAS_DIRECTORIES.contains(&lower.as_str())
                    || (CATEGORY_OR_EXTRAS_DIRECTORIES.contains(&lower.as_str())
                        && is_title_folder(dirs, depth - 1)))
            {
                return Some(ExclusionReason::ExtrasDirectory);
            }
            prefix.push(dir);
            if self.ignored(&prefix) {
                return Some(ExclusionReason::IgnorePattern);
            }
        }
        None
    }

    fn ignored(&self, rel_path: &Path) -> bool {
        self.ignore
            .iter()
            .any(|pattern| pattern.matches_path_with(rel_path, GLOB_OPTIONS))
    }
}

/// Whether `path` has a video extension. The extension alone: whether the
/// file is excluded is [`PathPolicy::disposition`]'s question.
pub fn is_video_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| lowercase_extension(&name.to_string_lossy()))
        .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
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
            // none of its files is a title on its own.
            (
                "Movies/Heat (1995)/VIDEO_TS/VTS_01_1.VOB",
                Excluded(DiscStructure),
            ),
            (
                "Heat.1995.DVD9/VIDEO_TS/VIDEO_TS.VOB",
                Excluded(DiscStructure),
            ),
            ("Heat (1995)/AUDIO_TS/x.mkv", Excluded(DiscStructure)),
            ("VIDEO_TS/VTS_01_2.VOB", Excluded(DiscStructure)),
            ("Heat (1995)/video_ts/vts_01_0.vob", Excluded(DiscStructure)),
            (
                "Heat (1995)/BDMV/STREAM/00001.m2ts",
                Excluded(DiscStructure),
            ),
            ("BDMV/STREAM/00001.m2ts", Excluded(DiscStructure)),
            (
                "Heat (1995)/CERTIFICATE/BACKUP/x.m2ts",
                Excluded(DiscStructure),
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
            "Heat (1995)/VIDEO_TS",
            "Heat (1995)/BDMV",
        ] {
            assert!(policy.excludes_directory(Path::new(dir)), "{dir}");
        }
        for dir in [
            "Movie (2019)",
            "Extras",
            "Show/Season 01",
            "Shorts",
            "Movies/Trailers",
        ] {
            assert!(!policy.excludes_directory(Path::new(dir)), "{dir}");
        }
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

            #[test]
            fn disposition_never_panics(path in ".*") {
                let _ = PathPolicy::default().disposition(Path::new(&path));
            }
        }
    }
}
