//! What a media file is, inferred from its path inside a library.
//!
//! [`crate::utils::filename`] reads one filename stem. A real library also says
//! things with its folders: `Show Name/Season 01/Show.Name.S01E02.mkv` names
//! the show in the series folder, not in the season folder the file sits in;
//! `Show (1998)/[Group] Show - 012 [1080p].mkv` is episode 12 of an absolutely
//! numbered series only because the folder names the same show. This module
//! combines the two. Pure and deterministic: the input is the path relative to
//! the library root, and nothing is read from disk.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use chrono::{Datelike, NaiveDate};
use regex::Regex;

use crate::utils::filename::{
    ParsedFilename, episode_title_after, is_edition_only, is_noise_only, normalized_stem,
    parse_media_filename, part_token,
};
use crate::utils::identity::{normalize_title, title_identity_key};
use crate::utils::path_policy::DiscKind;

/// The version of the rules [`infer_media`] classifies by, and of the title
/// fold ([`crate::utils::identity`]) that turns its titles into identity
/// keys. Stored on every file row the indexer classifies, and beside every
/// title's identity key; a row or key carrying an older version is
/// re-derived from its paths by the next scan, so a change to these rules
/// reaches files and titles indexed before it. Bump it whenever a path would
/// classify differently or a title would key differently. Rows and keys
/// stored before versions existed carry `0`.
///
/// - `1`: path inference v2 and the identity-key fold (issues #182, #183).
/// - `2`: an NFO beside the file, and its container tags, are read too
///   ([`crate::utils::classification`], issue #184). Keys are derived exactly
///   as by `1`, so the re-derivation this bump triggers changes none; the
///   reclassification it triggers is what applies NFOs already on disk.
/// - `3`: a multi-part movie's part token (`- CD1`, `- Part 2`, `.pt1`,
///   `disc1`) is read as its part rather than kept in its title (issue #233).
///   `Movie (2019) - CD1` keyed `movie cd1|2019` and each part was a film of
///   its own; every part now keys `movie|2019`, so the re-derivation this
///   bump triggers merges them into one title, and the reclassification
///   records each file's part.
///
/// Reading a disc structure's files as its enclosing folder's film (issue
/// #234) moved no version: every build at version 3 kept those files out of
/// the library, so no row at that version is one, and a row an older build
/// made of one is below it and reclassified when its disc's main title plays
/// it again.
pub const CLASSIFIER_VERSION: u16 = 3;

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

/// A movie, which edition of it the file is, and -- for a movie split across
/// files -- which part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovieInference {
    pub title: TitleGuess,
    pub edition: Option<String>,
    /// The part of a multi-part movie the file is (`Movie (2019) - CD2`),
    /// from 1 (issue #233). Every part keys the same title and edition; the
    /// parts of one edition are one source, played in part order.
    pub part_number: Option<u32>,
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
    /// The file sits in a folder that is nothing but a range of seasons
    /// (`Season 1-2`), so it is an episode of the show above, but its name
    /// carries no season and episode marker: the range cannot supply the
    /// season a season folder would.
    NoEpisodeMarkerInMultiSeasonFolder,
    /// The file is inside a DVD or Blu-ray disc structure (issue #234) that
    /// no folder names a film for: the disc is at the library root, or in a
    /// season folder -- a show's disc, whose episodes its title sets do not
    /// tell apart by any name.
    DiscWithoutTitleFolder,
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

/// A folder named for nothing but one disc of a set: `Disc 1`, `DISC1`,
/// `CD2`, `Disk_2`.
static BARE_DISC_FOLDER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:cd|disc|disk)[ ._-]?(\d{1,2})$").expect("valid regex"));

/// A folder that says only which piece of a release it is, and so names no
/// film: which disc -- `Disc 1`, `Disc One`, `DVD 1`, `BD1`, `Blu-ray 2`,
/// `Blu-ray Disc 1`, `Disc 1 of 2`, with or without a label after it (`Disc
/// 1 - Feature`, `DISC 1 [Feature]`) -- or which side, volume or part (`Side
/// A`, `Vol 1`, `Part Two`). A part or volume with words after it is left a
/// title: `Part 1 - The Fellowship of the Ring` names a film. A disc label's
/// words are captured (`tail` after a dash or colon, `bracketed` with its
/// bracket), because words with a year name a film after all
/// ([`dated_piece_tail`]).
static PIECE_FOLDER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    let number = r"(?:\d{1,2}|one|two|three|four|five|six|seven|eight|nine|ten)";
    let medium = r"(?:cd|dis[ck]|dvd|bd|blu[ -]?ray)";
    let disc = format!(
        r"(?:{medium}[ ._-]*)?{medium}[ ._-]*{number}(?:[ ._-]*of[ ._-]*{number})?(?:\s*[-:]\s*(?P<tail>.*)|\s*(?P<bracketed>[\[(].*))?"
    );
    let side = format!(r"side[ ._-]*(?:[a-d]|{number})");
    let volume = format!(r"(?:vol(?:ume)?|part|pt)[ ._-]*{number}");
    Regex::new(&format!(r"(?i)^(?:{disc}|{side}|{volume})$")).expect("valid regex")
});

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

/// Whether a folder is nothing but a range of seasons -- `Season 1-2`,
/// `Seasons 1 to 10`, `Complete S01-S05` -- with no title before it: a
/// multi-season pack inside the series folder above, which names the show.
fn is_bare_season_range(name: &str) -> bool {
    let name = name.trim();
    season_range_start(name).is_some_and(|start| title_of(&name[..start]).is_none())
}

/// Whether two titles are the same title once identity-normalised.
fn same_title(a: &str, b: &str) -> bool {
    normalize_title(a) == normalize_title(b)
}

/// Words a box set's folder adds after the show's name: `Breaking Bad
/// Complete Series`, `The Wire The Complete Collection`.
const BOX_SET_WORDS: &[&str] = &["the", "complete", "series", "collection"];

/// Whether `folder` is `show` followed by nothing but box-set words, or
/// nothing but box-set words at all (`The Complete Series`): either way the
/// folder names the box, and `show` the show.
fn is_box_set_of(folder: &str, show: &str) -> bool {
    let folder = normalize_title(folder);
    let show = normalize_title(show);
    let box_words = folder
        .strip_prefix(show.as_str())
        .and_then(|rest| rest.strip_prefix(' '))
        .unwrap_or(folder.as_str());
    box_words
        .split(' ')
        .all(|word| BOX_SET_WORDS.contains(&word))
}

/// Infer what the file at `rel_path` -- relative to its library root -- is.
/// A path's folders (root first) and its file's stem, or `None` for a path
/// with no file name.
fn split_path(rel_path: &Path) -> Option<(Vec<String>, String)> {
    let mut components: Vec<String> = rel_path
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let file_name = components.pop()?;
    let stem = Path::new(&file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some((components, stem))
}

/// The series the filename names: its parsed title, if it has one.
fn filename_series_of(parsed: &ParsedFilename) -> Option<TitleGuess> {
    (!parsed.title.is_empty()).then(|| TitleGuess {
        title: parsed.title.clone(),
        year: parsed.year,
    })
}

/// The show the folders above a file name, and -- when the file's folder is
/// a season folder or a bare season range -- the text before its season
/// token.
///
/// Above a season folder or a bare season range: the series folder (that
/// folder's parent), unless the season folder's own text before its season
/// token names a different title -- a season pack under a category folder --
/// or there is no series folder. A series folder that is a box set of the
/// filename's show (`Breaking Bad Complete Series`) names that show.
/// Otherwise the parent folder.
fn folder_series<'a>(
    dirs: &'a [String],
    filename_series: &Option<TitleGuess>,
) -> (Option<&'a str>, Option<TitleGuess>) {
    let parent = dirs.last().map(String::as_str);
    let season_prefix: Option<&str> = match parent.and_then(season_folder) {
        Some(folder) => Some(folder.prefix),
        // A folder of nothing but a season range (`Breaking Bad/Season
        // 1-2/`) holds seasons of the show above it: a season folder whose
        // season is unknown.
        None => parent.is_some_and(is_bare_season_range).then_some(""),
    };
    let series = match season_prefix {
        Some(season_prefix) => {
            let series_dir = dirs.len().checked_sub(2).and_then(|i| title_of(&dirs[i]));
            match (series_dir, title_of(season_prefix)) {
                (Some(dir), Some(prefix)) if !same_title(&dir.title, &prefix.title) => Some(prefix),
                (Some(dir), _) => match filename_series {
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
    (season_prefix, series)
}

/// The show a file at `rel_path` is an episode of when something other than
/// its path -- an NFO, its container tags -- says it is an episode: the show
/// its folders name (the series folder above a season folder, else its
/// parent folder), else the title its filename spells. The filename never
/// overrides a folder here, as it does for a path that is itself an episode's
/// (`TV Shows/Breaking.Bad.S01E01.mkv`): a name with no episode marker
/// (`Show/01 Pilot.m4v`) is the episode's, not the show's.
pub(crate) fn hinted_series(rel_path: &Path) -> TitleGuess {
    let unknown = || TitleGuess {
        title: UNKNOWN_SHOW.to_string(),
        year: None,
    };
    let Some((dirs, stem)) = split_path(rel_path) else {
        return unknown();
    };
    let filename_series = filename_series_of(&parse_media_filename(&stem));
    let (_, folder) = folder_series(&dirs, &filename_series);
    folder.or(filename_series).unwrap_or_else(unknown)
}

/// The movie a file at `rel_path` is when something other than its path --
/// an NFO -- says it is a movie: what [`infer_media`] reads a path with no
/// episode marker as.
pub(crate) fn movie_reading(rel_path: &Path) -> MovieInference {
    match split_path(rel_path) {
        Some((dirs, stem)) => {
            let parsed = parse_media_filename(&stem);
            movie_of(parsed, stem, dirs.last().map(String::as_str))
        }
        None => MovieInference {
            title: TitleGuess {
                title: String::new(),
                year: None,
            },
            edition: None,
            part_number: None,
        },
    }
}

/// Infer what the file at `rel_path` -- relative to its library root -- is.
pub fn infer_media(rel_path: &Path) -> MediaInference {
    let Some((dirs, stem)) = split_path(rel_path) else {
        return MediaInference::Movie(movie_reading(rel_path));
    };
    let dirs = dirs.as_slice();
    if let Some(at) = dirs
        .iter()
        .position(|dir| DiscKind::of_folder(dir).is_some())
    {
        return disc_title(&dirs[..at]);
    }
    let parsed = parse_media_filename(&stem);

    let parent = dirs.last().map(String::as_str);
    let parent_season = parent.and_then(season_folder).map(|folder| folder.season);
    let parent_is_bare_range = parent.is_some_and(is_bare_season_range);
    let filename_series = filename_series_of(&parsed);
    // The show the folders name; see `folder_series`.
    let (season_prefix, folder_series) = folder_series(dirs, &filename_series);
    let series = || -> TitleGuess {
        let chosen = match (
            season_prefix,
            folder_series.clone(),
            filename_series.clone(),
        ) {
            // A season folder's series is the folders' (unchanged by D182-C1),
            // and so is a bare season range's.
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
    if parent_is_bare_range {
        return MediaInference::Unclassifiable(
            UnclassifiableReason::NoEpisodeMarkerInMultiSeasonFolder,
        );
    }

    MediaInference::Movie(movie_of(parsed, stem, parent))
}

/// What a file inside a disc structure is (issue #234), given the folders
/// above the disc's root (`VIDEO_TS/`, `BDMV/`), root first: the movie a
/// folder above it names, read as a filename would be and completed from the
/// folder above it the same way (decision D234-8). The nearest folder that
/// names a film *with a year* names it -- so `Heat (1995)/VIDEO_TS`,
/// `Heat (1995)/Disc 1/VIDEO_TS` and `Heat (1995)/Bonus Feature/VIDEO_TS`
/// are all *Heat (1995)*, and no label a folder inside a film's carries can
/// make two films' discs one title. Only when no folder above names a year
/// does the nearest folder naming anything name the film (`Heat/VIDEO_TS`).
///
/// A folder that says only which piece of a release it is (`Disc 1`, `Disc
/// One`, `DVD 1`, `Disc 1 of 2`, `Side A`, `Vol 1`), only which edition
/// (`Theatrical`, `Extended Edition`), or is nothing but release noise
/// (`DVD9`) names no film at all, so `Heat/Disc One/VIDEO_TS` is *Heat* too
/// -- unless a disc label's words carry a year, when they name the film
/// themselves: `Movies/Disc 1 - Heat (1995)/VIDEO_TS` is *Heat (1995)*.
/// No file inside a disc is named for its title, so its own name is never
/// read.
///
/// Its part is not a path's to say either: which of the disc's files its
/// main title plays, and in what order, is read from the disc itself. A disc
/// structure is always a movie: a show's disc holds its episodes in title
/// sets no path tells apart, so one with a season folder anywhere above it
/// (`Show/Season 1/Disc 1/VIDEO_TS`), like one with no folder naming a film
/// around it at all, names no title.
fn disc_title(enclosing: &[String]) -> MediaInference {
    let untitled = MediaInference::Unclassifiable(UnclassifiableReason::DiscWithoutTitleFolder);
    if enclosing
        .iter()
        .any(|folder| season_folder(folder).is_some() || is_bare_season_range(folder))
    {
        return untitled;
    }
    let mut films = enclosing
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(at, folder)| {
            let film = film_named_by(folder)?;
            let parent = at.checked_sub(1).map(|above| enclosing[above].as_str());
            Some(movie_of(
                parse_media_filename(film),
                film.to_owned(),
                parent,
            ))
        })
        .peekable();
    let Some(nearest) = films.peek().cloned() else {
        return untitled;
    };
    let movie = films
        .find(|movie| movie.title.year.is_some())
        .unwrap_or(nearest);
    MediaInference::Movie(MovieInference {
        part_number: None,
        ..movie
    })
}

/// Whether a folder's name says only which piece of a release it is -- a
/// disc, side, volume or part -- and so names no film ([`PIECE_FOLDER_REGEX`]).
fn names_a_piece_only(name: &str) -> bool {
    PIECE_FOLDER_REGEX.is_match(name.trim())
}

/// The words of a folder above a disc that name its film, if any: the whole
/// name, unless it is only a piece label, only an edition, or release noise.
/// A disc label whose words carry a year names the film they spell
/// ([`dated_piece_tail`]), so `Disc 1 - Heat (1995)` is *Heat (1995)* and
/// never one title with `Disc 1 - Ronin (1998)`.
fn film_named_by(folder: &str) -> Option<&str> {
    let film = if names_a_piece_only(folder) {
        dated_piece_tail(folder)?
    } else if is_edition_only(folder) {
        return None;
    } else {
        folder
    };
    title_of(film)
        .is_some_and(|guess| !is_noise_only(&guess.title))
        .then_some(film)
}

/// The words after a disc label's number (`Heat (1995)` in `Disc 1 - Heat
/// (1995)` or `Disc 1 [Heat (1995)]`), when they hold a year: a year is what
/// tells a film's name from a label's own words (`Disc 1 - Feature`), as it
/// is for an edition's ([`is_edition_only`]).
fn dated_piece_tail(folder: &str) -> Option<&str> {
    let caps = PIECE_FOLDER_REGEX.captures(folder.trim())?;
    let tail = match (caps.name("tail"), caps.name("bracketed")) {
        (Some(tail), _) => tail.as_str(),
        (None, Some(bracketed)) => {
            let bracketed = bracketed.as_str();
            let inner = &bracketed[1..];
            inner
                .strip_suffix(']')
                .or_else(|| inner.strip_suffix(')'))
                .unwrap_or(inner)
        }
        (None, None) => return None,
    };
    let tail = tail.trim();
    parse_media_filename(tail).year.is_some().then_some(tail)
}

/// The disc a folder's name is nothing but: `Disc 2` is disc 2.
fn bare_disc_number(name: &str) -> Option<u32> {
    let caps = BARE_DISC_FOLDER_REGEX.captures(name.trim())?;
    caps[1].parse().ok().filter(|disc| *disc >= 1)
}

/// Where a disc folder sits in a set of discs of one film (decision
/// D234-7): which disc of the set it is, and what every disc of the set
/// shares -- the folder they are in, and what their names say besides the
/// disc number. `Heat (1995)/Disc 2` is disc 2 of the set of bare `Disc N`
/// folders in `Heat (1995)`; `Movies/Heat (1995) - CD2` is disc 2 of the set
/// of `Heat (1995) - CDn` folders in `Movies`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DiscSetPlace {
    /// The folder the set's disc folders are in, as the path given was
    /// (relative to a library root, or absolute).
    pub container: PathBuf,
    /// What the disc folder's name says besides its disc number, folded so
    /// that `Heat (1995) - Disc 1` and `Heat.1995.DISC2` agree: empty for a
    /// bare `Disc N`.
    pub name: String,
    /// Which disc of the set it is, from 1.
    pub disc: u32,
}

/// Where `folder` -- a folder enclosing a disc structure's root, relative to
/// a library root or absolute -- sits in a set of discs, if its name numbers
/// a disc: a bare `Disc 2`, `DISC2`, `CD2` or `Disk 2`, or a film's name with
/// a disc token (`Heat (1995) - Disc 2`, `Heat.1995.CD2`) as a filename's
/// part token is read ([`parse_media_filename`]).
pub fn disc_set_member(folder: &Path) -> Option<DiscSetPlace> {
    let name = folder.file_name()?.to_string_lossy();
    let container = folder.parent()?.to_path_buf();
    if let Some(disc) = bare_disc_number(&name) {
        return Some(DiscSetPlace {
            container,
            name: String::new(),
            disc,
        });
    }
    let token = part_token(&name).filter(|token| token.confirmed)?;
    // Every run of letters and digits, lowercased: separators, brackets and
    // dashes differ between the discs of one set as often as not.
    let folded: Vec<String> = token
        .stripped
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    (!folded.is_empty()).then(|| DiscSetPlace {
        container,
        name: folded.join(" "),
        disc: token.number,
    })
}

/// Where the disc structure rooted at `disc_root` (a `VIDEO_TS/` or `BDMV/`
/// folder) sits in a set of discs: [`disc_set_member`] of the folder
/// enclosing it.
pub fn disc_set_place(disc_root: &Path) -> Option<DiscSetPlace> {
    disc_set_member(disc_root.parent()?)
}

/// The movie a filename parse names, completed from its parent folder.
fn movie_of(parsed: ParsedFilename, stem: String, parent: Option<&str>) -> MovieInference {
    let ParsedFilename {
        title,
        year,
        edition,
        part,
        ..
    } = parsed;
    // The parent folder (never the library root) fills what the filename
    // leaves out (decision D182-C2): its title when the filename's is empty
    // or nothing but release noise, and its year when the filename names the
    // same title without one -- `Kill Bill (2003)/Kill Bill.mkv`. It never
    // makes a `part` or `pt` token a part (decision D233-2): one year-folder
    // holds distinct films as often as one film's pieces --
    // `Che (2008)/Che Part 1.mkv` and `Che Part 2.mkv` are two films, and so
    // is `The Godfather (1972)/The Godfather Part 2.mkv` beside its
    // predecessor.
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
    MovieInference {
        title,
        edition,
        part_number: part,
    }
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
