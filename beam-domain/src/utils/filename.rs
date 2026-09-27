//! Scene-release filename parsing.
//!
//! Extracts a clean title, release year, and (for episodes) season/episode
//! numbers from the kind of filenames real media collections actually have,
//! e.g. `Movie.Name.2019.2160p.WEB-DL.x265-GROUP.mkv` or
//! `Some.Show.S01E02.720p.HDTV.x264-ABC.mkv`. Pure, deterministic, and
//! network-free. This module reads one filename stem; what the folders around
//! it add -- the series folder, a season folder, absolute numbering -- is
//! [`crate::utils::media_path`]'s job.

use std::sync::LazyLock;

use chrono::NaiveDate;
use regex::Regex;

/// The result of parsing a media filename stem (no extension).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFilename {
    /// Cleaned title, with release-group/quality/codec noise stripped. For an
    /// episode, the text before the marker -- usually the show's name.
    pub title: String,
    /// Release year: a parenthesised year, a standalone 19xx/20xx token after
    /// a movie title, or (for an episode) the year token directly before the
    /// marker (`Show.2019.S01E02`).
    pub year: Option<u32>,
    /// Season number, if an `SxxEyy` or `1x02` marker was found.
    pub season: Option<u32>,
    /// Episode number, if an `SxxEyy` or `1x02` marker was found.
    pub episode: Option<u32>,
    /// The last episode of a multi-episode file (`S01E01E02`, `S01E01-E03`,
    /// `S01E01-03`). Always greater than `episode`, and less than
    /// [`MAX_EPISODE_RANGE`] past it: a larger "range" is a year or a
    /// resolution, not a range.
    pub last_episode: Option<u32>,
    /// The episode's own title: the words after the marker (or after a date),
    /// up to the first release-noise or year token.
    pub episode_title: Option<String>,
    /// The air date of a date-based episode (`Show.2024.03.01.mkv`). Only
    /// read when the stem carries no `SxxEyy` or `1x02` marker.
    pub air_date: Option<NaiveDate>,
    /// A movie edition: a Plex/Jellyfin `{edition-...}` tag, else the edition
    /// words (`Director's Cut`, `Extended`, ...) found after the title. Only
    /// read when the stem is not an episode.
    pub edition: Option<String>,
}

static EDITION_TAG_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\{edition-([^}]*)\}").expect("valid regex"));

static BRACKET_GROUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[^\]]*\]|\{[^}]*\}").expect("valid regex"));

static PAREN_GROUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(([^)]*)\)").expect("valid regex"));

/// An `SxxEyy` marker, with an optional multi-episode tail: more `Eyy`s
/// (`S01E01E02`, `S01E01-E02`) or a bare `-yy` (`S01E01-03`). The season and
/// episode may be split by one separator (`S01.E01`, `S01 E01`), which the
/// separator normalisation has already made a space.
///
/// `\b` so the marker has to start a word. Without it the pattern matched
/// inside one: `as0E0 S01E01` parsed as season 0, episode 0. Only the leading
/// boundary is anchored, so `S01E01v2` still parses; the bare `-yy` range
/// needs a trailing one, or `S01E01-720p` would read as episodes 1 to 720.
static EPISODE_MARKER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bS(\d{1,4}) ?E(\d{1,4})((?:-?E\d{1,4})*)(?:-(\d{1,4})\b)?")
        .expect("valid regex")
});

/// A `1x02` marker. Word-bounded on both sides, and at most two season
/// digits, so a resolution (`1920x1080`) can never match.
static CROSS_MARKER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})x(\d{2,3})\b").expect("valid regex"));

/// A date-based episode: `2024-03-01`, `2024.03.01` (normalised to spaces).
static AIR_DATE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b((?:19|20)\d{2})[ -](\d{2})[ -](\d{2})\b").expect("valid regex")
});

static DIGITS_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").expect("valid regex"));

static YEAR_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:19|20)\d{2}$").expect("valid regex"));

/// A `WIDTHxHEIGHT` resolution (`1920x1080`), matched against a cleaned token.
static RESOLUTION_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{3,4}x\d{3,4}$").expect("valid regex"));

/// How far past its first episode a multi-episode range may run. A larger
/// "last episode" is almost always something else -- a year, a resolution.
pub const MAX_EPISODE_RANGE: u32 = 50;

/// Prefixes (matched against a lowercased, alphanumeric-only token) that mark
/// release-scene noise: resolutions, sources, codecs, and edition tags. A
/// title is truncated at the first token that starts with one of these.
const NOISE_TOKEN_PREFIXES: &[&str] = &[
    // Resolutions
    "2160p",
    "1080p",
    "720p",
    "480p",
    "4k",
    "uhd", // Sources
    "webdl",
    "webrip",
    "bluray",
    "bdrip",
    "brrip",
    "hdtv",
    "dvdrip",
    "remux",
    // Video/audio codecs and HDR formats
    "hdr10",
    "hdr",
    "dv",
    "dolby",
    "x264",
    "x265",
    "h264",
    "h265",
    "hevc",
    "av1",
    "xvid",
    "aac",
    "ac3",
    "eac3",
    "ddp51",
    "ddp71",
    "dts",
    "truehd",
    "atmos",
    "10bit",
    "8bit",
    // Edition / release tags
    "proper",
    "repack",
    "extended",
    "unrated",
    "remastered",
    "internal",
    "limited",
    "complete",
    "multi",
    "dubbed",
    "subbed",
    "uncut",
    "imax",
];

/// The editions recognised from the words after a movie's title, in the fixed
/// order they are joined in. Each entry is the edition's display name and the
/// cleaned token sequences that spell it.
const EDITION_WORDS: &[(&str, &[&[&str]])] = &[
    (
        "Director's Cut",
        &[&["directors", "cut"], &["director", "s", "cut"], &["dc"]],
    ),
    ("Extended", &[&["extended"]]),
    ("Theatrical", &[&["theatrical"]]),
    ("Unrated", &[&["unrated"]]),
    ("Uncut", &[&["uncut"]]),
    ("IMAX", &[&["imax"]]),
    ("Remastered", &[&["remastered"]]),
    ("Special Edition", &[&["special", "edition"]]),
    ("Ultimate", &[&["ultimate"]]),
    ("Final Cut", &[&["final", "cut"]]),
    ("Criterion", &[&["criterion"]]),
    ("Anniversary", &[&["anniversary"]]),
];

/// Lowercases a token and strips everything but ASCII alphanumerics, so
/// noise-token matching is insensitive to hyphens/punctuation (e.g.
/// `"x265-GROUP"` and `"WEB-DL"` both compare cleanly).
fn clean_token(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_noise_token(token: &str) -> bool {
    let cleaned = clean_token(token);
    !cleaned.is_empty()
        && (NOISE_TOKEN_PREFIXES.iter().any(|p| cleaned.starts_with(p))
            || RESOLUTION_TOKEN_REGEX.is_match(&cleaned))
}

fn is_year_token(token: &str) -> bool {
    YEAR_TOKEN_REGEX.is_match(token)
}

/// A token with no letter or digit at all -- the `-` of `Show - S01E02`.
fn is_punctuation_token(token: &str) -> bool {
    !token.chars().any(char::is_alphanumeric)
}

/// Replaces `.` and `_` with spaces and collapses runs of whitespace.
fn normalize_separators(s: &str) -> String {
    s.chars()
        .map(|c| if c == '.' || c == '_' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncates `tokens` at the first noise token found, returning the tokens
/// strictly before it (or all tokens, if none are noise).
fn truncate_at_noise<'a>(tokens: &'a [&'a str]) -> &'a [&'a str] {
    match tokens.iter().position(|t| is_noise_token(t)) {
        Some(idx) => &tokens[..idx],
        None => tokens,
    }
}

/// A movie's title tokens: everything before the first noise token that
/// follows a title word. Noise words that open the name are title words
/// (`Uncut Gems`, `IMAX Hubble`): a release tag never comes first. Empty only
/// when every token is noise.
fn movie_title_span<'a>(tokens: &'a [&'a str]) -> &'a [&'a str] {
    let Some(first_word) = tokens.iter().position(|t| !is_noise_token(t)) else {
        return &[];
    };
    let end = tokens[first_word..]
        .iter()
        .position(|t| is_noise_token(t))
        .map_or(tokens.len(), |offset| first_word + offset);
    &tokens[..end]
}

/// Drops punctuation-only tokens from both ends.
fn trim_punctuation<'a>(tokens: &'a [&'a str]) -> &'a [&'a str] {
    let start = tokens
        .iter()
        .position(|t| !is_punctuation_token(t))
        .unwrap_or(tokens.len());
    let end = tokens
        .iter()
        .rposition(|t| !is_punctuation_token(t))
        .map_or(start, |last| last + 1);
    &tokens[start..end]
}

/// An episode marker found in the normalised stem.
#[derive(Debug, Clone, Copy)]
struct Marker {
    start: usize,
    end: usize,
    season: u32,
    episode: u32,
    last_episode: Option<u32>,
}

/// A range end is kept only when it is a plausible range.
fn plausible_range_end(first: u32, last: u32) -> Option<u32> {
    (last > first && last < first.saturating_add(MAX_EPISODE_RANGE)).then_some(last)
}

/// Every word-initial `SxxEyy` and `1x02` marker in `normalized`, in order.
fn find_markers(normalized: &str) -> Vec<Marker> {
    let mut markers: Vec<Marker> = Vec::new();
    for caps in EPISODE_MARKER_REGEX.captures_iter(normalized) {
        let whole = caps.get(0).expect("group 0 always present");
        let (Ok(season), Ok(episode)) = (caps[1].parse::<u32>(), caps[2].parse::<u32>()) else {
            continue;
        };
        let range_end = caps
            .get(4)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .or_else(|| {
                caps.get(3).and_then(|extra| {
                    DIGITS_REGEX
                        .find_iter(extra.as_str())
                        .last()
                        .and_then(|m| m.as_str().parse::<u32>().ok())
                })
            });
        markers.push(Marker {
            start: whole.start(),
            end: whole.end(),
            season,
            episode,
            last_episode: range_end.and_then(|last| plausible_range_end(episode, last)),
        });
    }
    for caps in CROSS_MARKER_REGEX.captures_iter(normalized) {
        let whole = caps.get(0).expect("group 0 always present");
        let (Ok(season), Ok(episode)) = (caps[1].parse::<u32>(), caps[2].parse::<u32>()) else {
            continue;
        };
        markers.push(Marker {
            start: whole.start(),
            end: whole.end(),
            season,
            episode,
            last_episode: None,
        });
    }
    markers.sort_by_key(|m| m.start);
    markers
}

/// Byte offset of the first noise token in `normalized` (single-space
/// separated), or its length if there is none.
fn first_noise_offset(normalized: &str) -> usize {
    let mut offset = 0;
    for token in normalized.split(' ') {
        if is_noise_token(token) {
            return offset;
        }
        offset += token.len() + 1;
    }
    normalized.len()
}

/// Which of several markers is the real one: the last word-initial marker
/// before the first release-noise token (decision D182-1). A title can carry
/// something marker-shaped ahead of the real marker, and a release tail can
/// carry one after the noise (`WEB-S00E00`); the real marker is the last one
/// before the tail starts. With no marker before the noise, the first one
/// after it is taken.
///
/// Markers written back to back for one season with rising episodes
/// (`S01E01.S01E02`, `1x01.1x02`) are one multi-episode file, not a title and
/// its marker: the run ending at the chosen marker becomes one range, from
/// its first episode to its last.
fn choose_marker(markers: &[Marker], normalized: &str) -> Option<Marker> {
    let noise = first_noise_offset(normalized);
    let chosen_at = markers
        .iter()
        .rposition(|m| m.start < noise)
        .or_else(|| (!markers.is_empty()).then_some(0))?;
    let chosen = markers[chosen_at];
    let mut first = chosen_at;
    while first > 0 {
        let (prev, next) = (markers[first - 1], markers[first]);
        let adjacent = prev.end <= next.start
            && normalized[prev.end..next.start]
                .chars()
                .all(|c| c == ' ' || c == '-');
        let rising = prev.last_episode.unwrap_or(prev.episode) < next.episode;
        if !(adjacent && rising && prev.season == chosen.season) {
            break;
        }
        first -= 1;
    }
    if first == chosen_at {
        return Some(chosen);
    }
    let opening = markers[first];
    let last = chosen.last_episode.unwrap_or(chosen.episode);
    // A run too wide to be one file is not merged: the chosen marker stands.
    Some(match plausible_range_end(opening.episode, last) {
        Some(last) => Marker {
            start: opening.start,
            end: chosen.end,
            season: chosen.season,
            episode: opening.episode,
            last_episode: Some(last),
        },
        None => chosen,
    })
}

/// The words after a marker or date, up to the first noise or year token,
/// with punctuation-only tokens dropped from both ends (`S01E02 - Pilot` ->
/// `Pilot`). The rest of a token the marker ended inside (`S01E01v2`) is not
/// part of the title.
pub(crate) fn episode_title_after(normalized: &str, end: usize) -> Option<String> {
    let rest = &normalized[end..];
    let rest = if rest.starts_with(char::is_alphanumeric) {
        rest.find(' ').map_or("", |space| &rest[space..])
    } else {
        rest
    };
    let tokens: Vec<&str> = rest
        .split_whitespace()
        // A glued leading dash (`-Pilot`) is punctuation, not the word.
        .map(|t| t.trim_start_matches('-'))
        .filter(|t| !t.is_empty())
        .collect();
    let stop = tokens
        .iter()
        .position(|t| is_noise_token(t) || is_year_token(t))
        .unwrap_or(tokens.len());
    let title = trim_punctuation(&tokens[..stop]).join(" ");
    (!title.is_empty()).then_some(title)
}

/// The edition named by the words after a movie's title, in
/// [`EDITION_WORDS`] order.
fn edition_from_tail(tail: &[&str]) -> Option<String> {
    let cleaned: Vec<String> = tail
        .iter()
        .flat_map(|t| t.split(['-', '\'']))
        .map(clean_token)
        .filter(|t| !t.is_empty())
        .collect();
    let found: Vec<&str> = EDITION_WORDS
        .iter()
        .filter(|(_, spellings)| {
            spellings.iter().any(|spelling| {
                cleaned
                    .windows(spelling.len())
                    .any(|window| window.iter().zip(spelling.iter()).all(|(a, b)| a == b))
            })
        })
        .map(|(name, _)| *name)
        .collect();
    (!found.is_empty()).then(|| found.join(", "))
}

/// The title tokens before an episode marker or air date, and the show year
/// they carry: a year token directly before the marker is the show's year
/// (`Show.2019.S01E02`), unless it is the only token (`1923.S01E01`, where it
/// is the title).
fn series_title_before<'a>(tokens: &'a [&'a str]) -> (&'a [&'a str], Option<u32>) {
    let tokens = trim_punctuation(truncate_at_noise(tokens));
    match tokens {
        [rest @ .., last] if !rest.is_empty() && is_year_token(last) => {
            (trim_punctuation(rest), last.parse().ok())
        }
        _ => (tokens, None),
    }
}

/// Parses a media filename stem (i.e. without its extension) into a clean
/// title plus whatever year/season/episode information could be extracted.
pub fn parse_media_filename(stem: &str) -> ParsedFilename {
    // 0. A Plex/Jellyfin edition tag, read before brace groups are stripped.
    let edition_tag = EDITION_TAG_REGEX
        .captures(stem)
        .map(|caps| caps[1].trim().to_string())
        .filter(|edition| !edition.is_empty());

    // 1. Strip bracket/brace groups entirely (release group tags, e.g. "[Group]").
    let without_brackets = BRACKET_GROUP_REGEX.replace_all(stem, "");

    // 2. Parens: a bare-year group is captured and removed; anything else
    // keeps its content but loses the parens. The last year group is the
    // release year, and where it sat is remembered: what follows it may be
    // the release's tail rather than its title.
    let paren_year_group = PAREN_GROUP_REGEX
        .captures_iter(&without_brackets)
        .filter_map(|caps| {
            let content = caps[1].trim();
            let whole = caps.get(0).expect("group 0 always present");
            YEAR_TOKEN_REGEX
                .is_match(content)
                .then(|| {
                    content
                        .parse::<u32>()
                        .ok()
                        .map(|year| (year, whole.start()))
                })
                .flatten()
        })
        .last();
    let paren_year = paren_year_group.map(|(year, _)| year);

    // 3. Normalize separators.
    let normalized = normalized_without_brackets(&without_brackets);

    // 4. Episode marker.
    if let Some(marker) = choose_marker(&find_markers(&normalized), &normalized) {
        let before: Vec<&str> = normalized[..marker.start].split_whitespace().collect();
        let (title_tokens, token_year) = series_title_before(&before);
        // No fallback to the whole stem: `S01E02.mkv` names no show, and
        // reading one out of it would invent a show called `S01E02`.
        return ParsedFilename {
            title: title_tokens.join(" "),
            year: paren_year.or(token_year),
            season: Some(marker.season),
            episode: Some(marker.episode),
            last_episode: marker.last_episode,
            episode_title: episode_title_after(&normalized, marker.end),
            air_date: None,
            edition: None,
        };
    }

    // 5. A date-based episode.
    let dated = AIR_DATE_REGEX.captures_iter(&normalized).find_map(|caps| {
        let date = NaiveDate::from_ymd_opt(
            caps[1].parse().ok()?,
            caps[2].parse().ok()?,
            caps[3].parse().ok()?,
        )?;
        let whole = caps.get(0).expect("group 0 always present");
        Some((date, whole.start(), whole.end()))
    });
    if let Some((air_date, start, end)) = dated {
        let before: Vec<&str> = normalized[..start].split_whitespace().collect();
        let (title_tokens, token_year) = series_title_before(&before);
        return ParsedFilename {
            title: title_tokens.join(" "),
            year: paren_year.or(token_year),
            season: None,
            episode: None,
            last_episode: None,
            episode_title: episode_title_after(&normalized, end),
            air_date: Some(air_date),
            edition: None,
        };
    }

    // 6. No episode marker: extract a year from the token stream unless a
    // parenthesized year was already found.
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let (year, title_end): (Option<u32>, usize) = if let Some((year, start)) = paren_year_group {
        // The title ends at the year group when edition words follow it
        // (`Movie (2019) Director's Cut`); otherwise the words after it are
        // still the title.
        let at = normalized_without_brackets(&without_brackets[..start])
            .split_whitespace()
            .count()
            .min(tokens.len());
        let edition_after = at > 0 && edition_from_tail(&tokens[at..]).is_some();
        (Some(year), if edition_after { at } else { tokens.len() })
    } else {
        match tokens
            .iter()
            .enumerate()
            .skip(1)
            .rfind(|(_, t)| is_year_token(t))
        {
            Some((idx, t)) => (t.parse().ok(), idx),
            None => (None, tokens.len()),
        }
    };

    let title_tokens = movie_title_span(&tokens[..title_end]);
    let title = finalize_title(trim_punctuation(title_tokens), &normalized);
    // Edition words are read only after a title word: a name that is all
    // noise has no title for them to follow.
    let edition = edition_tag.or_else(|| {
        (!title_tokens.is_empty())
            .then(|| edition_from_tail(&tokens[title_tokens.len()..]))
            .flatten()
    });

    ParsedFilename {
        title,
        year,
        season: None,
        episode: None,
        last_episode: None,
        episode_title: None,
        air_date: None,
        edition,
    }
}

/// `stem` with bracket and brace groups removed, parenthesised years dropped
/// (other parentheses unwrapped), and separators normalised to single spaces
/// -- the form [`parse_media_filename`] searches for markers in.
pub(crate) fn normalized_stem(stem: &str) -> String {
    normalized_without_brackets(&BRACKET_GROUP_REGEX.replace_all(stem, ""))
}

/// [`normalized_stem`] of text whose bracket and brace groups are already
/// gone. A year group becomes a space, so the words either side of it stay
/// two words and a prefix of the text normalises to a prefix of the tokens.
fn normalized_without_brackets(text: &str) -> String {
    let without_parens = PAREN_GROUP_REGEX.replace_all(text, |caps: &regex::Captures| {
        let content = caps[1].trim();
        if YEAR_TOKEN_REGEX.is_match(content) {
            " ".to_string()
        } else {
            format!(" {content} ")
        }
    });
    normalize_separators(&without_parens)
}

/// Whether a parsed title is nothing but release noise: the whole-stem
/// fallback of a name like `REPACK.1080p` rather than a title.
pub(crate) fn is_noise_only(title: &str) -> bool {
    let mut tokens = title.split_whitespace().peekable();
    tokens.peek().is_some() && tokens.all(|t| is_noise_token(t) || is_punctuation_token(t))
}

fn finalize_title(tokens: &[&str], fallback: &str) -> String {
    let joined = tokens.join(" ");
    if joined.trim().is_empty() {
        fallback.trim().to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(title: &str, year: Option<u32>) -> ParsedFilename {
        ParsedFilename {
            title: title.to_string(),
            year,
            season: None,
            episode: None,
            last_episode: None,
            episode_title: None,
            air_date: None,
            edition: None,
        }
    }

    #[test]
    fn scene_release_with_year_and_noise() {
        assert_eq!(
            parse_media_filename("Movie.Name.2019.2160p.WEB-DL.x265-GROUP"),
            parsed("Movie Name", Some(2019))
        );
    }

    #[test]
    fn simple_dotted_title_with_year() {
        assert_eq!(
            parse_media_filename("The.Matrix.Reloaded.2003"),
            parsed("The Matrix Reloaded", Some(2003))
        );
    }

    #[test]
    fn parenthesized_year() {
        assert_eq!(
            parse_media_filename("movie (2024)"),
            parsed("movie", Some(2024))
        );
    }

    #[test]
    fn no_year_present() {
        assert_eq!(parse_media_filename("Avatar"), parsed("Avatar", None));
    }

    #[test]
    fn leading_year_then_release_year() {
        assert_eq!(
            parse_media_filename("1917.2019.1080p"),
            parsed("1917", Some(2019))
        );
    }

    #[test]
    fn leading_year_then_release_year_other_case() {
        assert_eq!(
            parse_media_filename("2012.2009.720p"),
            parsed("2012", Some(2009))
        );
    }

    #[test]
    fn year_embedded_in_title_vs_release_year() {
        assert_eq!(
            parse_media_filename("Blade.Runner.2049.2017"),
            parsed("Blade Runner 2049", Some(2017))
        );
    }

    #[test]
    fn lone_year_becomes_title() {
        assert_eq!(parse_media_filename("2019"), parsed("2019", None));
    }

    #[test]
    fn episode_marker_dotted() {
        let result = parse_media_filename("Some.Show.S01E02.720p.HDTV.x264-ABC");
        assert_eq!(result.title, "Some Show");
        assert_eq!(result.season, Some(1));
        assert_eq!(result.episode, Some(2));
        assert_eq!(result.episode_title, None);
    }

    #[test]
    fn episode_marker_lowercase() {
        let result = parse_media_filename("show.s02e10");
        assert_eq!(result.title, "show");
        assert_eq!(result.season, Some(2));
        assert_eq!(result.episode, Some(10));
    }

    #[test]
    fn episode_marker_with_spaces() {
        let result = parse_media_filename("Series S01E01 720p");
        assert_eq!(result.title, "Series");
        assert_eq!(result.season, Some(1));
        assert_eq!(result.episode, Some(1));
    }

    /// A title that happens to contain `s<digits>e<digits>` inside a word used
    /// to win, because the search is case-insensitive and took the first hit.
    /// The file was then indexed under season 0, episode 0, silently.
    #[test]
    fn a_marker_in_the_title_does_not_beat_the_real_one() {
        let result = parse_media_filename("as0E0.S01E01.1080p");
        assert_eq!(result.season, Some(1));
        assert_eq!(result.episode, Some(1));
        assert_eq!(result.title, "as0E0");
    }

    /// The marker has to start a word, so one glued to the end of a title word
    /// is not a marker at all -- and the stem parses as a title with no
    /// episode rather than as a wrong episode.
    #[test]
    fn a_marker_glued_inside_a_word_is_not_a_marker() {
        let result = parse_media_filename("Reruns01e02");
        assert_eq!(result.season, None);
        assert_eq!(result.episode, None);
    }

    /// Where a release suffix contributes a second genuine candidate after the
    /// quality noise, the marker before the noise wins (decision D182-1).
    #[test]
    fn a_marker_in_the_release_tail_does_not_beat_the_real_one() {
        let result = parse_media_filename("Show.S01E01.1080p.WEB-S00E00");
        assert_eq!(result.season, Some(1));
        assert_eq!(result.episode, Some(1));
    }

    /// Of two word-initial markers before any noise, the last is the real one:
    /// the first is part of the title.
    #[test]
    fn of_two_markers_before_the_noise_the_last_wins() {
        let result = parse_media_filename("Room.1x04.S02E03.Title.720p");
        assert_eq!(result.season, Some(2));
        assert_eq!(result.episode, Some(3));
        assert_eq!(result.title, "Room 1x04");
        assert_eq!(result.episode_title.as_deref(), Some("Title"));
    }

    /// A season-0 marker is legitimate -- specials are numbered that way --
    /// so the fix must not be "reject implausible numbers".
    #[test]
    fn a_specials_marker_still_parses() {
        let result = parse_media_filename("Show.S00E03.1080p");
        assert_eq!(result.season, Some(0));
        assert_eq!(result.episode, Some(3));
    }

    #[test]
    fn the_episode_title_is_the_text_after_the_marker() {
        let cases = [
            ("Show.Name.S01E02.The.Title.1080p.WEB", Some("The Title")),
            ("Show - S01E02 - The Title", Some("The Title")),
            ("Show.S01E02-Pilot", Some("Pilot")),
            ("Show.S01E02v2.Pilot", Some("Pilot")),
            ("Show S01E02", None),
            ("Show.S01E02.2019", None),
            ("Show.S01E02.-.720p", None),
        ];
        for (stem, expected) in cases {
            assert_eq!(
                parse_media_filename(stem).episode_title.as_deref(),
                expected,
                "{stem}"
            );
        }
    }

    #[test]
    fn a_cross_marker_parses_but_a_resolution_does_not() {
        let result = parse_media_filename("Show.1x02.Title");
        assert_eq!((result.season, result.episode), (Some(1), Some(2)));
        assert_eq!(result.title, "Show");
        assert_eq!(result.episode_title.as_deref(), Some("Title"));

        let result = parse_media_filename("Movie.2019.1920x1080");
        assert_eq!((result.season, result.episode), (None, None));
        assert_eq!(result.title, "Movie");
    }

    /// A `WIDTHxHEIGHT` resolution is release noise, wherever it sits.
    #[test]
    fn a_width_by_height_resolution_is_noise() {
        assert_eq!(
            parse_media_filename("Movie.Name.1920x1080.x264"),
            parsed("Movie Name", None)
        );
        assert_eq!(
            parse_media_filename("Movie Name 720x480"),
            parsed("Movie Name", None)
        );
        let result = parse_media_filename("Show.S01E02.Title.1280x720");
        assert_eq!(result.episode_title.as_deref(), Some("Title"));
    }

    #[test]
    fn multi_episode_ranges() {
        let cases = [
            ("Show.S01E01E02", Some(2)),
            ("Show.S01E01-E03", Some(3)),
            ("Show.S01E01-03", Some(3)),
            ("Show.S01E01E02E03", Some(3)),
            // Separate markers written back to back are one range.
            ("Show.S01E01.S01E02", Some(2)),
            ("Show.1x01.1x02", Some(2)),
            ("Show.S01E01-S01E02.720p", Some(2)),
            ("Show.S01E01E02.S01E03", Some(3)),
            ("Show.S01.E01.S01.E02", Some(2)),
            // Not ranges: backwards, too wide, or a resolution.
            ("Show.S01E05-E03", None),
            ("Show.S01E01-2019", None),
            ("Show.S01E01-720p", None),
            ("Show.S01E01", None),
        ];
        for (stem, expected) in cases {
            let result = parse_media_filename(stem);
            assert_eq!(
                result.episode,
                Some(if stem.contains("E05") { 5 } else { 1 })
            );
            assert_eq!(result.last_episode, expected, "{stem}");
        }
    }

    /// Only back-to-back markers of one season with rising episodes merge:
    /// anything else is a title marker and the real one (decision D182-1).
    #[test]
    fn markers_that_do_not_form_a_run_stay_apart() {
        let cases = [
            // Another season.
            ("Show.S01E01.S02E02", "Show S01E01", (2, 2, None)),
            // Falling episodes.
            ("Show.S01E03.S01E02", "Show S01E03", (1, 2, None)),
            // A word between them.
            (
                "Show.S01E01.Title.S01E02",
                "Show S01E01 Title",
                (1, 2, None),
            ),
            // Too wide to be one file.
            ("Show.S01E01.S01E60", "Show S01E01", (1, 60, None)),
        ];
        for (stem, title, (season, episode, last)) in cases {
            let result = parse_media_filename(stem);
            assert_eq!(result.title, title, "{stem}");
            assert_eq!(
                (result.season, result.episode, result.last_episode),
                (Some(season), Some(episode), last),
                "{stem}"
            );
        }
    }

    /// `S01.E01` and `S01 E01` are the `S01E01` marker with a separator in it.
    #[test]
    fn a_split_marker_parses() {
        for stem in [
            "Show.S01.E02.Title",
            "Show S01 E02 Title",
            "Show_s01_e02_Title",
        ] {
            let result = parse_media_filename(stem);
            assert_eq!(
                (result.season, result.episode),
                (Some(1), Some(2)),
                "{stem}"
            );
            assert_eq!(result.title, "Show", "{stem}");
            assert_eq!(result.episode_title.as_deref(), Some("Title"), "{stem}");
        }
        // A season on its own is not a marker.
        let result = parse_media_filename("Show.S01.Extras");
        assert_eq!((result.season, result.episode), (None, None));
    }

    #[test]
    fn a_year_before_the_marker_is_the_show_year_unless_it_is_the_title() {
        let result = parse_media_filename("Show.2019.S01E02");
        assert_eq!((result.title.as_str(), result.year), ("Show", Some(2019)));

        let result = parse_media_filename("1923.S01E01");
        assert_eq!((result.title.as_str(), result.year), ("1923", None));
    }

    #[test]
    fn a_date_based_episode() {
        let result = parse_media_filename("The.Daily.Show.2024.03.01.Guest.Name.720p");
        assert_eq!(result.title, "The Daily Show");
        assert_eq!(result.air_date, NaiveDate::from_ymd_opt(2024, 3, 1));
        assert_eq!(result.episode_title.as_deref(), Some("Guest Name"));
        assert_eq!((result.season, result.episode), (None, None));

        let result = parse_media_filename("Show 2024-03-01");
        assert_eq!(result.air_date, NaiveDate::from_ymd_opt(2024, 3, 1));

        // Not a date: February has no 30th.
        let result = parse_media_filename("Show.2024.02.30");
        assert_eq!(result.air_date, None);
    }

    #[test]
    fn a_marker_takes_precedence_over_a_date() {
        let result = parse_media_filename("Show.S01E02.2024.03.01");
        assert_eq!((result.season, result.episode), (Some(1), Some(2)));
        assert_eq!(result.air_date, None);
    }

    #[test]
    fn editions() {
        let cases = [
            ("Movie (2019) {edition-Final Cut}", Some("Final Cut")),
            ("Movie.2019.Directors.Cut.1080p", Some("Director's Cut")),
            ("Movie.2019.Director's.Cut", Some("Director's Cut")),
            (
                "Movie.2019.Extended.Remastered.1080p",
                Some("Extended, Remastered"),
            ),
            (
                "Movie.2019.Remastered.Extended",
                Some("Extended, Remastered"),
            ),
            ("Movie.2019.Special.Edition", Some("Special Edition")),
            ("Movie.Extended.1080p", Some("Extended")),
            ("Movie.2019.1080p", None),
            // An edition word in the title is not an edition.
            ("The.Ultimate.Gift.2006", None),
            // Edition words after a parenthesised year (Plex/Radarr naming).
            ("Movie (2019) Director's Cut", Some("Director's Cut")),
            ("Blade Runner (1982) The Final Cut", Some("Final Cut")),
            ("Movie (2019) Extended", Some("Extended")),
            // A noise word opening the title is a title word, not an edition.
            ("Uncut Gems (2019)", None),
            ("Uncut.Gems.2019.1080p", None),
            ("IMAX Hubble (2010)", None),
            ("Uncut.Gems.2019.Directors.Cut", Some("Director's Cut")),
            // A name that is all noise has no title for an edition to follow.
            ("Extended.1080p", None),
            // Episodes carry no edition.
            ("Show.S01E01.Extended", None),
        ];
        for (stem, expected) in cases {
            assert_eq!(
                parse_media_filename(stem).edition.as_deref(),
                expected,
                "{stem}"
            );
        }
    }

    /// Edition words after a parenthesised year are the edition, not the
    /// title, so every edition of a film keys the same title; other words
    /// after the year are still the title. A noise word that opens a title is
    /// part of it.
    #[test]
    fn the_title_around_a_year_and_opening_noise_words() {
        let cases = [
            ("Movie (2019) Director's Cut", "Movie", Some(2019)),
            (
                "Blade Runner (1982) The Final Cut",
                "Blade Runner",
                Some(1982),
            ),
            ("Kill Bill (2003) Vol 1", "Kill Bill Vol 1", Some(2003)),
            ("Uncut Gems (2019)", "Uncut Gems", Some(2019)),
            ("Uncut.Gems.2019.1080p", "Uncut Gems", Some(2019)),
            ("IMAX Hubble (2010)", "IMAX Hubble", Some(2010)),
            (
                "REPACK.Movie.Name.2019.1080p",
                "REPACK Movie Name",
                Some(2019),
            ),
        ];
        for (stem, title, year) in cases {
            let result = parse_media_filename(stem);
            assert_eq!(
                (result.title.as_str(), result.year),
                (title, year),
                "{stem}"
            );
        }
    }

    #[test]
    fn bracket_groups_stripped() {
        assert_eq!(
            parse_media_filename("[Group] Title [1080p]"),
            parsed("Title", None)
        );
    }

    #[test]
    fn noise_words_only_falls_back_to_normalized_stem() {
        let result = parse_media_filename("REPACK.PROPER.HDR10.10bit");
        assert_eq!(result.title, "REPACK PROPER HDR10 10bit");
        assert_eq!(result.year, None);
    }

    #[test]
    fn underscores_normalized_to_spaces() {
        assert_eq!(
            parse_media_filename("Some_Movie_2020"),
            parsed("Some Movie", Some(2020))
        );
    }

    #[test]
    fn parent_dir_show_with_year_and_resolution() {
        assert_eq!(
            parse_media_filename("Breaking Bad (2008) [1080p]"),
            parsed("Breaking Bad", Some(2008))
        );
    }

    #[test]
    fn empty_stem_does_not_panic() {
        let result = parse_media_filename("");
        assert_eq!(result.title, "");
        assert_eq!(result.year, None);
    }

    #[test]
    fn noise_before_year_is_truncated() {
        // A noise token appearing before the winning year token must still
        // be cut from the title.
        assert_eq!(
            parse_media_filename("Movie.1080p.2019.x265"),
            parsed("Movie", Some(2019))
        );
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use proptest::prelude::*;

    // Filenames are attacker-adjacent input: they come from whatever is on
    // the disk, in whatever encoding, at whatever length. The table-driven
    // tests above pin the behaviour for realistic names; these pin the
    // invariants that must hold for *every* name, including the ones nobody
    // thought to enumerate.
    proptest! {
        #[test]
        fn parsing_never_panics(stem in ".*") {
            let _ = parse_media_filename(&stem);
        }

        #[test]
        fn parsing_is_deterministic(stem in ".*") {
            prop_assert_eq!(
                parse_media_filename(&stem),
                parse_media_filename(&stem)
            );
        }

        #[test]
        fn the_title_never_grows_beyond_the_input(stem in ".*") {
            let parsed = parse_media_filename(&stem);
            prop_assert!(
                parsed.title.chars().count() <= stem.chars().count(),
                "title {:?} is longer than the stem {:?} it came from",
                parsed.title,
                stem
            );
        }

        #[test]
        fn the_title_is_trimmed(stem in ".*") {
            let parsed = parse_media_filename(&stem);
            prop_assert_eq!(parsed.title.trim(), parsed.title.as_str());
        }

        #[test]
        fn a_year_is_always_a_plausible_release_year(stem in ".*") {
            if let Some(year) = parse_media_filename(&stem).year {
                prop_assert!(
                    (1900..=2099).contains(&year),
                    "implausible year {year} from {stem:?}"
                );
            }
        }

        #[test]
        fn season_and_episode_are_reported_together_or_not_at_all(stem in ".*") {
            let parsed = parse_media_filename(&stem);
            prop_assert_eq!(
                parsed.season.is_some(),
                parsed.episode.is_some(),
                "half an SxxEyy marker parsed from {:?}: {:?}",
                stem,
                parsed
            );
        }

        /// A range always ends after it starts, and never runs implausibly far.
        #[test]
        fn a_range_ends_after_its_first_episode(stem in ".*") {
            let parsed = parse_media_filename(&stem);
            if let Some(last) = parsed.last_episode {
                let first = parsed.episode.expect("a range has a first episode");
                prop_assert!(last > first && last < first + MAX_EPISODE_RANGE, "{parsed:?}");
            }
        }

        // An `SxxEyy` marker anywhere in the stem must be found, whatever
        // surrounds it -- provided nothing around it is also a marker.
        //
        // Neither the prefix nor the suffix can spell a marker: the prefix
        // excludes `e`/`E` (so it cannot end a word with one) and both exclude
        // `s`/`S` and `x`/`X`. With two candidates in one stem, *which* one
        // wins is a policy (decision D182-1), pinned by
        // `k_markers_without_noise_resolve_to_the_last` below and by
        // `a_marker_in_the_release_tail_does_not_beat_the_real_one`.
        #[test]
        fn an_embedded_marker_is_always_found(
            prefix in "[A-DF-RT-WYZa-df-rt-wyz][A-DF-RT-WYZa-df-rt-wyz0-9. ]{0,20}",
            season in 1u32..40,
            episode in 1u32..40,
            suffix in "[A-RT-WYZa-rt-wyz0-9. -]{0,20}",
        ) {
            let stem = format!("{prefix}.S{season:02}E{episode:02}.{suffix}");
            let parsed = parse_media_filename(&stem);
            prop_assert_eq!(parsed.season, Some(season));
            prop_assert_eq!(parsed.episode, Some(episode));
        }

        // With several word-initial markers and no release noise, the last is
        // the real one. Words start with `j`, `k` or `q`, which no noise
        // prefix does, and cannot spell a marker or a year. A word separates
        // each marker from the next, so none of them form a run.
        #[test]
        fn k_markers_without_noise_resolve_to_the_last(
            words in proptest::collection::vec("[jkq][a-z]{2,5}", 1..4),
            markers in proptest::collection::vec((0u32..40, 0u32..40), 1..4),
        ) {
            let mut parts: Vec<String> = words.clone();
            for (n, (season, episode)) in markers.iter().enumerate() {
                if n > 0 {
                    parts.push("jkq".to_string());
                }
                parts.push(format!("S{season:02}E{episode:02}"));
            }
            let stem = parts.join(".");
            let parsed = parse_media_filename(&stem);
            let (season, episode) = *markers.last().expect("at least one marker");
            prop_assert_eq!(parsed.season, Some(season));
            prop_assert_eq!(parsed.episode, Some(episode));
        }

        // Back-to-back markers of one season with rising episodes are one
        // file: it starts at the first and ends at the last, however many
        // there are and however they are written.
        #[test]
        fn a_run_of_rising_markers_is_one_range(
            season in 1u32..30,
            first in 1u32..40,
            steps in proptest::collection::vec(1u32..5, 1..4),
            cross in any::<bool>(),
        ) {
            let mut episodes = vec![first];
            for step in &steps {
                episodes.push(episodes.last().expect("non-empty") + step);
            }
            let markers: Vec<String> = episodes
                .iter()
                .map(|episode| if cross {
                    format!("{season}x{episode:02}")
                } else {
                    format!("S{season:02}E{episode:02}")
                })
                .collect();
            let stem = format!("Show.{}.720p", markers.join("."));
            let parsed = parse_media_filename(&stem);
            let last = *episodes.last().expect("non-empty");
            prop_assert_eq!(parsed.season, Some(season));
            prop_assert_eq!(parsed.episode, Some(first));
            prop_assert_eq!(parsed.last_episode, Some(last));
            prop_assert_eq!(parsed.title, "Show");
        }

        // `.`, `_` and space all separate words, so which one a release uses
        // changes nothing.
        #[test]
        fn parsing_is_separator_invariant(
            tokens in proptest::collection::vec("[A-Za-z0-9-]{1,8}", 1..8),
        ) {
            let dotted = parse_media_filename(&tokens.join("."));
            prop_assert_eq!(&dotted, &parse_media_filename(&tokens.join("_")));
            prop_assert_eq!(&dotted, &parse_media_filename(&tokens.join(" ")));
        }
    }
}
