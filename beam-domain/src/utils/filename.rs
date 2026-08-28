//! Scene-release filename parsing.
//!
//! Extracts a clean title, release year, and (for episodes) season/episode
//! numbers from the kind of filenames real media collections actually have,
//! e.g. `Movie.Name.2019.2160p.WEB-DL.x265-GROUP.mkv` or
//! `Some.Show.S01E02.720p.HDTV.x264-ABC.mkv`. Pure, deterministic, and
//! network-free -- this replaces the bare `SxxEyy` regex the indexer used to
//! rely on for the whole classification job.

use std::sync::LazyLock;

use regex::Regex;

/// The result of parsing a media filename stem (no extension).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFilename {
    /// Cleaned title, with release-group/quality/codec noise stripped.
    pub title: String,
    /// Release year, if a standalone 19xx/20xx year token was found.
    pub year: Option<u32>,
    /// Season number, if an `SxxEyy` marker was found.
    pub season: Option<u32>,
    /// Episode number, if an `SxxEyy` marker was found.
    pub episode: Option<u32>,
}

static BRACKET_GROUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[^\]]*\]|\{[^}]*\}").expect("valid regex"));

static PAREN_GROUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(([^)]*)\)").expect("valid regex"));

static EPISODE_MARKER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)S(\d+)E(\d+)").expect("valid regex"));

static YEAR_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:19|20)\d{2}$").expect("valid regex"));

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
    !cleaned.is_empty() && NOISE_TOKEN_PREFIXES.iter().any(|p| cleaned.starts_with(p))
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

/// Parses a media filename stem (i.e. without its extension) into a clean
/// title plus whatever year/season/episode information could be extracted.
pub fn parse_media_filename(stem: &str) -> ParsedFilename {
    // 1. Strip bracket/brace groups entirely (release group tags, e.g. "[Group]").
    let without_brackets = BRACKET_GROUP_REGEX.replace_all(stem, "");

    // 2. Parens: a bare-year group is captured and removed; anything else
    // keeps its content but loses the parens.
    let mut paren_year: Option<u32> = None;
    let without_parens =
        PAREN_GROUP_REGEX.replace_all(&without_brackets, |caps: &regex::Captures| {
            let content = caps[1].trim();
            if YEAR_TOKEN_REGEX.is_match(content) {
                paren_year = content.parse().ok();
                String::new()
            } else {
                format!(" {content} ")
            }
        });

    // 3. Normalize separators.
    let normalized = normalize_separators(&without_parens);
    let fallback_title = normalized.clone();

    // 4. Episode marker.
    if let Some(caps) = EPISODE_MARKER_REGEX.captures(&normalized) {
        let season = caps[1].parse().ok();
        let episode = caps[2].parse().ok();
        let match_start = caps.get(0).expect("group 0 always present").start();
        let title_tokens: Vec<&str> = normalized[..match_start].split_whitespace().collect();
        let title_tokens = truncate_at_noise(&title_tokens);
        let title = finalize_title(title_tokens, &fallback_title);
        return ParsedFilename {
            title,
            year: paren_year,
            season,
            episode,
        };
    }

    // 5. No episode marker: extract a year from the token stream unless a
    // parenthesized year was already found.
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let (year, title_tokens): (Option<u32>, &[&str]) = if paren_year.is_some() {
        (paren_year, &tokens[..])
    } else {
        match tokens
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, t)| YEAR_TOKEN_REGEX.is_match(t))
            .next_back()
        {
            Some((idx, t)) => (t.parse().ok(), &tokens[..idx]),
            None => (None, &tokens[..]),
        }
    };

    let title_tokens = truncate_at_noise(title_tokens);
    let title = finalize_title(title_tokens, &fallback_title);

    ParsedFilename {
        title,
        year,
        season: None,
        episode: None,
    }
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

        // An `SxxEyy` marker anywhere in the stem must be found, whatever
        // surrounds it.
        #[test]
        fn an_embedded_marker_is_always_found(
            prefix in "[A-Za-z][A-Za-z0-9. ]{0,20}",
            season in 1u32..40,
            episode in 1u32..40,
            suffix in "[A-Za-z0-9. -]{0,20}",
        ) {
            let stem = format!("{prefix}.S{season:02}E{episode:02}.{suffix}");
            let parsed = parse_media_filename(&stem);
            prop_assert_eq!(parsed.season, Some(season));
            prop_assert_eq!(parsed.episode, Some(episode));
        }
    }
}
