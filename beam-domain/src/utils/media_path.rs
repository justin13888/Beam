//! What a media file is, inferred from its path inside a library.
//!
//! [`crate::utils::filename`] reads one filename stem. A real library also says
//! things with its folders: `Show Name/Season 01/Show.Name.S01E02.mkv` names
//! the show in the series folder, not in the season folder the file sits in;
//! `Show (1998)/[Group] Show - 012 [1080p].mkv` is episode 12 of an absolutely
//! numbered series only because the folder names the same show. This module
//! combines the two. Pure and deterministic: the input is the path relative to
//! the library root, and nothing is read from disk.

use std::path::{Component, Path};
use std::sync::LazyLock;

use chrono::{Datelike, NaiveDate};
use regex::Regex;

use crate::utils::filename::{ParsedFilename, normalized_stem, parse_media_filename};
use crate::utils::identity::{normalize_title, title_identity_key};

/// The version of the rules [`infer_media`] classifies by. Stored on every
/// file row the indexer classifies; a row carrying an older version is
/// reclassified from its path by the next scan, so a change to these rules
/// reaches files indexed before it. Bump it whenever a path would classify
/// differently. Rows indexed before versions existed carry `0`.
pub const CLASSIFIER_VERSION: u16 = 1;

/// A title and year as a path spells them -- what a movie or show is keyed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleGuess {
    pub title: String,
    pub year: Option<u32>,
}

impl TitleGuess {
    /// The identity key of the title this guess names; see
    /// [`crate::utils::identity`].
    pub fn identity_key(&self) -> String {
        title_identity_key(&self.title, self.year)
    }
}

/// How an episode's number was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpisodeNumbering {
    /// `S01E02` or `1x02`.
    Standard,
    /// An air date: the season is the year and the episode `MMDD`.
    Daily,
    /// One running number across the series (`Show - 012`), typical of anime.
    Absolute,
}

/// An episode, and the show it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeInference {
    pub series: TitleGuess,
    pub season: u32,
    pub first_episode: u32,
    /// The last episode of a multi-episode file, greater than `first_episode`.
    pub last_episode: Option<u32>,
    pub air_date: Option<NaiveDate>,
    /// The episode's own title, when the filename carries one.
    pub episode_title: Option<String>,
    pub numbering: EpisodeNumbering,
    /// The season a season folder names, when the filename says otherwise.
    /// The filename wins; this is kept so the indexer can say so.
    pub contradicted_season_folder: Option<u32>,
}

/// A movie, and which edition of it the file is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovieInference {
    pub title: TitleGuess,
    pub edition: Option<String>,
}

/// Why a file could not be classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnclassifiableReason {
    /// The file sits in a season folder, so it is an episode, but nothing in
    /// its name says which one. Indexing it as a movie -- what the path alone
    /// would otherwise suggest -- would invent a film.
    NoEpisodeNumberInSeasonFolder { season: u32 },
}

/// What a library path is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaInference {
    Episode(EpisodeInference),
    Movie(MovieInference),
    Unclassifiable(UnclassifiableReason),
}

/// The show name used when neither a folder nor the filename names one.
pub const UNKNOWN_SHOW: &str = "Unknown Show";

static SEASON_FOLDER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:season|series|saison|staffel|temporada)[ ._-]*|s)(\d{1,4})$")
        .expect("valid regex")
});

static SPECIALS_FOLDER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^specials?$").expect("valid regex"));

/// `<title> - <n>` with an optional `v2` revision: the fansub convention.
static ABSOLUTE_DASH_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?) - (\d{1,4})(?:v\d)?(?: |$)").expect("valid regex"));

/// A bare `E<n>` or `EP<n>`, optionally after a title.
static ABSOLUTE_BARE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(.+?) )?EP?(\d{1,4})(?:v\d)?(?: |$)").expect("valid regex")
});

static YEAR_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:19|20)\d{2}$").expect("valid regex"));

/// The season a folder name designates: `Season 01`, `Series 2`, `Saison 3`,
/// `Staffel 4`, `Temporada 5`, `S06`, and `Specials` (season 0). Case and
/// separators between word and number are ignored; anything else in the name
/// makes it an ordinary folder.
pub fn season_folder_number(name: &str) -> Option<u32> {
    let name = name.trim();
    if SPECIALS_FOLDER_REGEX.is_match(name) {
        return Some(0);
    }
    SEASON_FOLDER_REGEX
        .captures(name)
        .and_then(|caps| caps[1].parse().ok())
}

/// Infer what the file at `rel_path` -- relative to its library root -- is.
pub fn infer_media(rel_path: &Path) -> MediaInference {
    let components: Vec<String> = rel_path
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let Some((file_name, dirs)) = components.split_last() else {
        return MediaInference::Movie(MovieInference {
            title: TitleGuess {
                title: String::new(),
                year: None,
            },
            edition: None,
        });
    };
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parsed = parse_media_filename(&stem);

    let parent_season = dirs.last().and_then(|dir| season_folder_number(dir));
    // The series folder: the season folder's parent, or the parent itself
    // when it is not a season folder. A season folder directly under the root
    // has none, and the series comes from the filename.
    let series_dir: Option<&str> = match parent_season {
        Some(_) => dirs
            .len()
            .checked_sub(2)
            .and_then(|i| dirs.get(i))
            .map(String::as_str),
        None => dirs.last().map(String::as_str),
    };
    let series = || series_guess(series_dir, &parsed);

    if let (Some(season), Some(episode)) = (parsed.season, parsed.episode) {
        return MediaInference::Episode(EpisodeInference {
            series: series(),
            season,
            first_episode: episode,
            last_episode: parsed.last_episode,
            air_date: None,
            episode_title: parsed.episode_title.clone(),
            numbering: EpisodeNumbering::Standard,
            contradicted_season_folder: parent_season.filter(|folder| *folder != season),
        });
    }

    if let Some(air_date) = parsed.air_date {
        let season = air_date.year().unsigned_abs();
        return MediaInference::Episode(EpisodeInference {
            series: series(),
            season,
            first_episode: air_date.month() * 100 + air_date.day(),
            last_episode: None,
            air_date: Some(air_date),
            episode_title: parsed.episode_title.clone(),
            numbering: EpisodeNumbering::Daily,
            contradicted_season_folder: parent_season.filter(|folder| *folder != season),
        });
    }

    if !dirs.is_empty()
        && let Some((title, number, episode_title)) = absolute_number(&stem)
    {
        let series_named = || {
            title.is_empty()
                || series_dir.is_some_and(|dir| {
                    normalize_title(&parse_media_filename(dir).title) == normalize_title(&title)
                })
        };
        if parent_season.is_some() || series_named() {
            return MediaInference::Episode(EpisodeInference {
                series: series(),
                season: parent_season.unwrap_or(1),
                first_episode: number,
                last_episode: None,
                air_date: None,
                episode_title,
                numbering: EpisodeNumbering::Absolute,
                contradicted_season_folder: None,
            });
        }
    }

    if let Some(season) = parent_season {
        return MediaInference::Unclassifiable(
            UnclassifiableReason::NoEpisodeNumberInSeasonFolder { season },
        );
    }

    let ParsedFilename {
        title,
        year,
        edition,
        ..
    } = parsed;
    MediaInference::Movie(MovieInference {
        title: TitleGuess {
            title: if title.is_empty() { stem } else { title },
            year,
        },
        edition,
    })
}

/// The show a series folder (or, without one, the filename) names.
fn series_guess(series_dir: Option<&str>, parsed: &ParsedFilename) -> TitleGuess {
    if let Some(dir) = series_dir {
        let folder = parse_media_filename(dir);
        if !folder.title.is_empty() {
            return TitleGuess {
                title: folder.title,
                year: folder.year,
            };
        }
    }
    if parsed.title.is_empty() {
        TitleGuess {
            title: UNKNOWN_SHOW.to_string(),
            year: None,
        }
    } else {
        TitleGuess {
            title: parsed.title.clone(),
            year: parsed.year,
        }
    }
}

/// An absolute episode number in `stem`: the title before it (empty for a
/// bare `E12`), the number, and any episode title after it. A four-digit
/// year is never an episode number.
fn absolute_number(stem: &str) -> Option<(String, u32, Option<String>)> {
    let normalized = normalized_stem(stem);
    let caps = ABSOLUTE_DASH_REGEX
        .captures(&normalized)
        .or_else(|| ABSOLUTE_BARE_REGEX.captures(&normalized))?;
    let digits = caps.get(2)?;
    if YEAR_REGEX.is_match(digits.as_str()) {
        return None;
    }
    let number = digits.as_str().parse().ok()?;
    let title = caps
        .get(1)
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default();
    let episode_title = crate::utils::filename::episode_title_after(&normalized, digits.end());
    Some((title, number, episode_title))
}

#[cfg(test)]
#[path = "media_path_corpus.rs"]
mod corpus;
