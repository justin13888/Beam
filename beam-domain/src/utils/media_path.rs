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

use crate::utils::filename::{
    ParsedFilename, episode_title_after, is_noise_only, normalized_stem, parse_media_filename,
};
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
    /// The name is `<title> - <n>`, the fansub spelling of an absolutely
    /// numbered episode, but nothing around it names the show: it is at the
    /// library root, or its folder names another title (decision D182-4).
    /// Indexing it as a movie would turn a season into dozens of films.
    AmbiguousAbsoluteNumber { number: u32 },
    /// The name is `<title> - <n>.<d>` (`Show - 12.5`), the fansub spelling
    /// of a recap or special between two episodes. It has no episode number
    /// of its own, and reading it as episode `n` would put two files on one
    /// episode.
    FractionalAbsoluteNumber { whole: u32, tenth: u32 },
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

/// A season word and its number anywhere in a folder name: `Season 01`,
/// `Breaking Bad Season 1`, `Season 1 (2008)`, `Staffel 2`.
static SEASON_WORD_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:season|series|saison|staffel|temporada)[ ._-]*(\d{1,4})\b")
        .expect("valid regex")
});

/// A lone `S<n>` with no episode after it: `S02`, or a season pack's
/// `Show.S02.1080p.BluRay`.
static SEASON_SHORT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bS(\d{1,4})\b").expect("valid regex"));

/// A range of seasons: `S01-S05`, `S01-05`, `Season 1-5`, `Seasons 1 to 5`,
/// optionally after `Complete` (`Complete S01-S05`). A multi-season pack's
/// folder carries one after the show's name; everything from it on is not
/// the title.
static SEASON_RANGE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:complete[ ._-]+)?(?:s(\d{1,2})[ ._-]*(?:-|to)[ ._-]*s?(\d{1,2})|(?:seasons?|series|saison|staffel|temporada)[ ._-]*(\d{1,2})[ ._-]*(?:-|to)[ ._-]*(\d{1,2}))\b",
    )
    .expect("valid regex")
});

/// Where a season range starts in `text`, if it carries one whose last
/// season is after its first.
fn season_range_start(text: &str) -> Option<usize> {
    SEASON_RANGE_REGEX.captures_iter(text).find_map(|caps| {
        let first: u32 = caps.get(1).or_else(|| caps.get(3))?.as_str().parse().ok()?;
        let last: u32 = caps.get(2).or_else(|| caps.get(4))?.as_str().parse().ok()?;
        (last > first).then(|| caps.get(0).expect("group 0 always present").start())
    })
}

static SPECIALS_FOLDER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^specials?$").expect("valid regex"));

/// `<title> - <n>` with an optional `v2` revision: the fansub convention.
static ABSOLUTE_DASH_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?) - (\d{1,4})(?:v\d)?(?: |$)").expect("valid regex"));

/// A `- <n>.<d>` in a raw stem, before separators are normalised: the
/// fractional episode `Show - 12.5`. The digit after the point must end the
/// number, so `Show - 12.1080p` is not one.
static ABSOLUTE_DASH_FRACTION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r" - (\d{1,4})\.(\d)(?:v\d)?(?:[ ._\[\(]|$)").expect("valid regex")
});

/// A bare `E<n>` or `EP<n>`, optionally after a title.
static ABSOLUTE_BARE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(.+?) )?EP?(\d{1,4})(?:v\d)?(?: |$)").expect("valid regex")
});

/// `Episode 3` or `Ep 3`: an episode number with no season, which only a
/// season folder can complete.
static EPISODE_WORD_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:episode|ep)[ -]*(\d{1,4})\b").expect("valid regex"));

/// Three or four digits: `501` for season 5, episode 1.
static SEASON_EPISODE_DIGITS_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{3,4}$").expect("valid regex"));

static YEAR_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:19|20)\d{2}$").expect("valid regex"));

/// A season folder: which season it designates, and the text before its
/// season token (`The.Office.US.` in `The.Office.US.S02.1080p`), which may
/// name the show.
struct SeasonFolder<'a> {
    season: u32,
    prefix: &'a str,
}

fn season_folder(name: &str) -> Option<SeasonFolder<'_>> {
    let name = name.trim();
    // A multi-season pack is not one season's folder: its first season
    // would be read as every file's.
    if season_range_start(name).is_some() {
        return None;
    }
    if SPECIALS_FOLDER_REGEX.is_match(name) {
        return Some(SeasonFolder {
            season: 0,
            prefix: "",
        });
    }
    let caps = SEASON_WORD_REGEX
        .captures(name)
        .or_else(|| SEASON_SHORT_REGEX.captures(name))?;
    let whole = caps.get(0).expect("group 0 always present");
    Some(SeasonFolder {
        season: caps[1].parse().ok()?,
        prefix: &name[..whole.start()],
    })
}

/// The season a folder name designates: a season word and number anywhere in
/// it (`Season 01`, `Series 2`, `Saison 3`, `Staffel 4`, `Temporada 5`,
/// `Breaking Bad Season 1`, `Season 1 (2008)`), a lone `S06` with no episode
/// after it (`S06`, a season pack's `Show.S06.1080p`), or `Specials` (season
/// 0). Case and the separators between word and number are ignored. A range
/// of seasons (`Show.S01-S05`, `Show Season 1-5`) designates none.
pub fn season_folder_number(name: &str) -> Option<u32> {
    season_folder(name).map(|folder| folder.season)
}

/// The title and year a folder or name spells, if it spells a title at all.
/// A season range ends the title, as a season token does in a season
/// folder: `Breaking.Bad.S01-S05.COMPLETE.1080p` is *Breaking Bad*.
fn title_of(text: &str) -> Option<TitleGuess> {
    let text = season_range_start(text).map_or(text, |start| &text[..start]);
    let ParsedFilename { title, year, .. } = parse_media_filename(text);
    (!title.is_empty()).then_some(TitleGuess { title, year })
}

/// Whether two titles are the same title once identity-normalised.
fn same_title(a: &str, b: &str) -> bool {
    normalize_title(a) == normalize_title(b)
}

/// Words a box set's folder adds after the show's name: `Breaking Bad
/// Complete Series`, `The Wire The Complete Collection`.
const BOX_SET_WORDS: &[&str] = &["the", "complete", "series", "collection"];

/// Whether `folder` is `show` followed by nothing but box-set words.
fn is_box_set_of(folder: &str, show: &str) -> bool {
    let folder = normalize_title(folder);
    let show = normalize_title(show);
    folder
        .strip_prefix(show.as_str())
        .and_then(|rest| rest.strip_prefix(' '))
        .is_some_and(|rest| rest.split(' ').all(|word| BOX_SET_WORDS.contains(&word)))
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

    let parent = dirs.last().map(String::as_str);
    let parent_season_folder = parent.and_then(season_folder);
    let parent_season = parent_season_folder.as_ref().map(|folder| folder.season);
    let filename_series = (!parsed.title.is_empty()).then(|| TitleGuess {
        title: parsed.title.clone(),
        year: parsed.year,
    });
    // The show the folders name. Above a season folder: the series folder
    // (the season folder's parent), unless the season folder's own text
    // before its season token names a different title -- a season pack
    // under a category folder -- or there is no series folder. A series
    // folder that is a box set of the filename's show (`Breaking Bad
    // Complete Series`) names that show. Otherwise the parent folder.
    let folder_series: Option<TitleGuess> = match &parent_season_folder {
        Some(folder) => {
            let series_dir = dirs.len().checked_sub(2).and_then(|i| title_of(&dirs[i]));
            match (series_dir, title_of(folder.prefix)) {
                (Some(dir), Some(prefix)) if !same_title(&dir.title, &prefix.title) => Some(prefix),
                (Some(dir), _) => match &filename_series {
                    Some(file) if is_box_set_of(&dir.title, &file.title) => Some(TitleGuess {
                        title: file.title.clone(),
                        year: dir.year.or(file.year),
                    }),
                    _ => Some(dir),
                },
                (None, prefix) => prefix,
            }
        }
        None => parent.and_then(title_of),
    };
    let series = || -> TitleGuess {
        let chosen = match (
            &parent_season_folder,
            folder_series.clone(),
            filename_series.clone(),
        ) {
            // A season folder's series is the folders' (unchanged by D182-C1).
            (Some(_), Some(folder), _) => Some(folder),
            // Flat: the filename's series wins when it names another title
            // than the parent folder -- `TV Shows/Breaking.Bad.S01E01.mkv`
            // (decision D182-C1).
            (None, Some(folder), Some(file)) if !same_title(&folder.title, &file.title) => {
                Some(file)
            }
            (_, Some(folder), _) => Some(folder),
            (_, None, file) => file,
        };
        chosen.unwrap_or_else(|| TitleGuess {
            title: UNKNOWN_SHOW.to_string(),
            year: None,
        })
    };

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

    if let Some(absolute) = absolute_number(&stem) {
        let series_named = absolute.title.is_empty()
            || folder_series
                .as_ref()
                .is_some_and(|folder| same_title(&folder.title, &absolute.title));
        let names_show = !dirs.is_empty() && (parent_season.is_some() || series_named);
        // A year where the number would be is the release year, unless a
        // season folder of the show the title names holds it: there it is
        // the episode (`One Piece/Season 1/One Piece - 1999.mkv`).
        let year_is_episode = !absolute.year_shaped
            || (parent_season.is_some() && series_named && !absolute.title.is_empty());
        // The dash form is ambiguous with a movie's part number
        // (`Movie (2019) - 1`): outside a season folder it needs a number of
        // at least two digits. A year the folder and filename share says
        // nothing either way -- `Chernobyl (2019)/Chernobyl (2019) - 01` is
        // an episode -- so it is not consulted.
        let dash_plausible = !absolute.dash || parent_season.is_some() || absolute.digits >= 2;
        if year_is_episode && dash_plausible {
            if let Some(tenth) = absolute.tenth {
                return MediaInference::Unclassifiable(
                    UnclassifiableReason::FractionalAbsoluteNumber {
                        whole: absolute.number,
                        tenth,
                    },
                );
            }
            if names_show {
                // The folder was just checked against the title (or is a
                // season folder), so it -- not the whole `<title> - <n>`
                // stem -- names the show.
                return MediaInference::Episode(EpisodeInference {
                    series: folder_series.clone().unwrap_or_else(series),
                    season: parent_season.unwrap_or(1),
                    first_episode: absolute.number,
                    last_episode: None,
                    air_date: None,
                    episode_title: absolute.episode_title,
                    numbering: EpisodeNumbering::Absolute,
                    contradicted_season_folder: None,
                });
            }
            if absolute.dash && !absolute.year_shaped {
                return MediaInference::Unclassifiable(
                    UnclassifiableReason::AmbiguousAbsoluteNumber {
                        number: absolute.number,
                    },
                );
            }
        }
    }

    if let Some(season) = parent_season {
        if let Some((episode, episode_title)) = season_folder_episode(&stem, season) {
            return MediaInference::Episode(EpisodeInference {
                series: series(),
                season,
                first_episode: episode,
                last_episode: None,
                air_date: None,
                episode_title,
                numbering: EpisodeNumbering::Standard,
                contradicted_season_folder: None,
            });
        }
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
    // The parent folder (never the library root) fills what the filename
    // leaves out (decision D182-C2): its title when the filename's is empty
    // or nothing but release noise, and its year when the filename names the
    // same title without one -- `Kill Bill (2003)/Kill Bill.mkv`.
    let folder = parent.and_then(title_of);
    let title = match folder {
        Some(folder) if title.is_empty() || is_noise_only(&title) => TitleGuess {
            title: folder.title,
            year: year.or(folder.year),
        },
        Some(folder) if year.is_none() && same_title(&folder.title, &title) => TitleGuess {
            title,
            year: folder.year,
        },
        _ => TitleGuess {
            title: if title.is_empty() { stem } else { title },
            year,
        },
    };
    MediaInference::Movie(MovieInference { title, edition })
}

/// An absolute episode number found in a stem.
struct AbsoluteNumber {
    /// The title before the number; empty for a bare `E12`.
    title: String,
    number: u32,
    /// How many digits the number was written with (`012` is three).
    digits: usize,
    /// Whether the number is written `<title> - <n>`, rather than `E<n>`.
    dash: bool,
    /// Whether the number could be a release year (1900-2099).
    year_shaped: bool,
    /// The digit after a decimal point (`5` in `Show - 12.5`): the number
    /// is fractional.
    tenth: Option<u32>,
    episode_title: Option<String>,
}

/// An absolute episode number in `stem`: `<title> - <n>` or a bare `E<n>`.
fn absolute_number(stem: &str) -> Option<AbsoluteNumber> {
    let normalized = normalized_stem(stem);
    let (caps, dash) = match ABSOLUTE_DASH_REGEX.captures(&normalized) {
        Some(caps) => (caps, true),
        None => (ABSOLUTE_BARE_REGEX.captures(&normalized)?, false),
    };
    let digits = caps.get(2)?;
    let number: u32 = digits.as_str().parse().ok()?;
    // Normalising read the point as a separator (`Show - 12 5`), so the
    // raw stem says whether the number was fractional.
    let tenth = if dash {
        ABSOLUTE_DASH_FRACTION_REGEX
            .captures_iter(stem)
            .find(|fraction| fraction[1].parse::<u32>().ok() == Some(number))
            .and_then(|fraction| fraction[2].parse().ok())
    } else {
        None
    };
    Some(AbsoluteNumber {
        title: caps
            .get(1)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default(),
        number,
        digits: digits.as_str().len(),
        dash,
        year_shaped: YEAR_REGEX.is_match(digits.as_str()),
        tenth,
        episode_title: episode_title_after(&normalized, digits.end()),
    })
}

/// An episode number that only a season folder makes readable (decision
/// D182-C5): `Episode 3` / `Ep 3`, or three or four digits whose leading
/// digits are the folder's season (`501` in `Season 5`). Only the words
/// before the first release-noise token are searched.
fn season_folder_episode(stem: &str, season: u32) -> Option<(u32, Option<String>)> {
    let normalized = normalized_stem(stem);
    if let Some(caps) = EPISODE_WORD_REGEX.captures(&normalized) {
        let number = caps.get(1).expect("group 1 always present");
        let episode = number.as_str().parse().ok()?;
        return Some((episode, episode_title_after(&normalized, number.end())));
    }
    let mut offset = 0;
    for token in normalized.split(' ') {
        let end = offset + token.len();
        offset = end + 1;
        if is_noise_only(token) {
            break;
        }
        if !SEASON_EPISODE_DIGITS_REGEX.is_match(token) {
            continue;
        }
        let (season_digits, episode_digits) = token.split_at(token.len() - 2);
        if season_digits.parse::<u32>().ok() == Some(season)
            && let Ok(episode) = episode_digits.parse::<u32>()
            && episode > 0
        {
            return Some((episode, episode_title_after(&normalized, end)));
        }
    }
    None
}

#[cfg(test)]
#[path = "media_path_corpus.rs"]
mod corpus;
