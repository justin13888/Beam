//! What a file is, from its path and from what the library already says
//! about it beside the path (issue #184): the Kodi `.nfo` files next to it,
//! and the tags inside its container.
//!
//! Priority is NFO, then path, then container tags. An NFO is written by a
//! person or a media manager on purpose, so the kind it declares -- a
//! `<movie>`, an `<episodedetails>` with its season and episode -- wins over
//! what the path merely suggests. Container tags are stamped by whatever tool
//! last touched the file, often with nothing more than the filename, so they
//! only fill what the path leaves open.
//!
//! What a title is *matched* by never changes: its identity key is derived
//! from the path alone (decision D184-3), exactly as the indexer's key
//! re-derivation derives it, so a title keyed here is the title that pass
//! finds. An NFO or a tag supplies what a new title is *shown* as -- its
//! display title and year, until enrichment replaces them -- and the provider
//! id it is pinned to.

use std::path::Path;

use crate::models::pin::ProviderPin;
use crate::utils::filename::is_noise_only;
use crate::utils::media_path::{
    EpisodeInference, EpisodeNumbering, MediaInference, MovieInference, TitleGuess, UNKNOWN_SHOW,
    UnclassifiableReason, hinted_series, infer_media, movie_reading,
};
use crate::utils::nfo::{Nfo, NfoKind};

/// The file-level container tags classification reads, as FFmpeg reports
/// them (`title`; an MP4's `show`, `season_number`, `episode_sort`; a `date`
/// or `year`), keys compared case-insensitively.
///
/// Stored on the file as its probe read them (`files.container_tags`), so a
/// reclassification at a [`crate::utils::media_path::CLASSIFIER_VERSION`]
/// bump reads the same tags without probing the file again.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ContainerTags {
    pub title: Option<String>,
    pub show: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub year: Option<u32>,
}

/// The earliest and latest release years a tag is believed about.
const TAG_YEARS: std::ops::RangeInclusive<u32> = 1870..=2100;

impl ContainerTags {
    /// Read the tags classification uses out of a file's tags.
    pub fn from_tags<'a>(tags: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut read = Self::default();
        for (key, value) in tags {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.to_ascii_lowercase().as_str() {
                "title" => read.title = Some(value.to_string()),
                "show" => read.show = Some(value.to_string()),
                "season_number" => read.season = whole_number(value),
                "episode_sort" => read.episode = whole_number(value),
                "date" | "year" | "date_released" => {
                    read.year = read.year.or_else(|| leading_year(value));
                }
                _ => {}
            }
        }
        read
    }
}

fn whole_number(text: &str) -> Option<u32> {
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The year a date tag starts with: `2019`, `2019-05-01`, `2019-05-01T00:00:00Z`.
fn leading_year(text: &str) -> Option<u32> {
    let digits = text.get(..4)?;
    let rest_is_date = text[4..].chars().next().is_none_or(|c| !c.is_ascii_digit());
    whole_number(digits)
        .filter(|_| rest_is_date)
        .filter(|year| TAG_YEARS.contains(year))
}

/// Everything beside a file's path that says what it is.
#[derive(Debug, Clone, Copy)]
pub struct Hints<'a> {
    /// The NFO describing the file itself: `<stem>.nfo` beside it, else
    /// `movie.nfo` in its folder.
    pub file_nfo: Option<&'a Nfo>,
    /// The `tvshow.nfo` in its series folder.
    pub show_nfo: Option<&'a Nfo>,
    pub tags: &'a ContainerTags,
}

impl Hints<'_> {
    /// No hints: classification is path inference alone.
    pub const NONE: Hints<'static> = Hints {
        file_nfo: None,
        show_nfo: None,
        tags: &ContainerTags {
            title: None,
            show: None,
            season: None,
            episode: None,
            year: None,
        },
    };
}

/// What a file is, and how a title created for it is shown and pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// What the file is. The movie title or the episode's series in it is
    /// always the path's reading -- the identity key a title is found or
    /// created by.
    pub inference: MediaInference,
    /// The display title and year the movie or show is created with; `None`
    /// for a file that is neither.
    pub display: Option<TitleGuess>,
    /// The provider id the movie or show is pinned to.
    pub pin: Option<ProviderPin>,
}

/// Whether an NFO of kind `kind` can describe a title of kind `wanted`: one
/// of that kind, or one that does not say (a URL-only NFO).
fn describes(nfo: Option<&Nfo>, wanted: NfoKind) -> Option<&Nfo> {
    nfo.filter(|nfo| nfo.kind.is_none_or(|kind| kind == wanted))
}

/// The episode numbers an episode NFO gives: the season and first episode of
/// its first `<episodedetails>`, and the last episode of a multi-episode file
/// in that season.
fn nfo_episode(nfo: &Nfo) -> Option<(u32, u32, Option<u32>)> {
    let first = nfo.episodes.first()?;
    let (season, episode) = (first.season?, first.episode?);
    let last = nfo
        .episodes
        .iter()
        .filter(|e| e.season == Some(season))
        .filter_map(|e| e.episode)
        .max()
        .filter(|last| *last > episode);
    Some((season, episode, last))
}

/// Whether a container title is worth showing: not release noise, and not
/// just the filename an encoder stamped in.
fn meaningful_tag_title<'a>(title: Option<&'a str>, rel_path: &Path) -> Option<&'a str> {
    let stem = rel_path.file_stem().map(|s| s.to_string_lossy());
    title.filter(|title| {
        !is_noise_only(title)
            && stem
                .as_deref()
                .is_none_or(|stem| !stem.eq_ignore_ascii_case(title))
    })
}

/// Classify the file at `rel_path`, relative to its library root, from its
/// path and `hints`.
pub fn classify(rel_path: &Path, hints: &Hints<'_>) -> Classification {
    let Hints {
        file_nfo,
        show_nfo,
        tags,
    } = *hints;
    let mut inference = infer_media(rel_path);
    let path_series = |inference: &MediaInference| match inference {
        MediaInference::Episode(episode) => episode.series.clone(),
        _ => hinted_series(rel_path),
    };

    // 1. The file's own NFO decides what it is.
    let nfo_decided = match file_nfo.and_then(|nfo| nfo.kind.map(|kind| (nfo, kind))) {
        Some((nfo, NfoKind::Movie)) => {
            let mut movie = match inference {
                MediaInference::Movie(movie) => movie,
                _ => movie_reading(rel_path),
            };
            if let Some(edition) = &nfo.edition {
                movie.edition = Some(edition.clone());
            }
            inference = MediaInference::Movie(movie);
            true
        }
        Some((nfo, NfoKind::Episodes)) => match nfo_episode(nfo) {
            Some((season, first_episode, last_episode)) => {
                let path_title = match &inference {
                    MediaInference::Episode(episode) => episode.episode_title.clone(),
                    _ => None,
                };
                let first = nfo.episodes.first();
                inference = MediaInference::Episode(EpisodeInference {
                    series: path_series(&inference),
                    season,
                    first_episode,
                    last_episode,
                    air_date: first.and_then(|e| e.aired),
                    episode_title: first.and_then(|e| e.title.clone()).or(path_title),
                    numbering: EpisodeNumbering::Standard,
                    contradicted_season_folder: None,
                });
                true
            }
            None => false,
        },
        Some((_, NfoKind::TvShow)) | None => false,
    };

    // 2. Container tags fill what the path leaves open.
    if !nfo_decided {
        let tagged_episode = match (&inference, tags.season, tags.episode) {
            // A season folder says the season; only the episode was missing.
            (
                MediaInference::Unclassifiable(
                    UnclassifiableReason::NoEpisodeNumberInSeasonFolder { season },
                ),
                _,
                Some(episode),
            ) => Some((*season, episode)),
            // A movie reading is the path's fallback, not a statement: a
            // file tagged with a show, season and episode is that episode.
            (
                MediaInference::Movie(_) | MediaInference::Unclassifiable(_),
                Some(season),
                Some(episode),
            ) if tags.show.is_some() => Some((season, episode)),
            _ => None,
        };
        if let Some((season, first_episode)) = tagged_episode {
            inference = MediaInference::Episode(EpisodeInference {
                series: path_series(&inference),
                season,
                first_episode,
                last_episode: None,
                air_date: None,
                episode_title: None,
                numbering: EpisodeNumbering::Standard,
                contradicted_season_folder: None,
            });
        }
    }
    if let MediaInference::Episode(episode) = &mut inference
        && episode.episode_title.is_none()
    {
        episode.episode_title =
            meaningful_tag_title(tags.title.as_deref(), rel_path).map(str::to_string);
    }

    // 3. How the title is shown, and what pins it.
    let (display, pin) = match &inference {
        MediaInference::Movie(MovieInference { title, .. }) => {
            let nfo = describes(file_nfo, NfoKind::Movie);
            let display = TitleGuess {
                title: nfo
                    .and_then(|nfo| nfo.title.clone())
                    .unwrap_or_else(|| title.title.clone()),
                year: nfo.and_then(|nfo| nfo.year).or(title.year).or(tags.year),
            };
            (Some(display), nfo.and_then(|nfo| nfo.ids.pin()))
        }
        MediaInference::Episode(EpisodeInference { series, .. }) => {
            let show = describes(show_nfo, NfoKind::TvShow);
            let episode_nfo = file_nfo.filter(|nfo| nfo.kind == Some(NfoKind::Episodes));
            let display = TitleGuess {
                title: show
                    .and_then(|nfo| nfo.title.clone())
                    .or_else(|| episode_nfo.and_then(|nfo| nfo.show_title.clone()))
                    // A tag names the show only when the path names none.
                    .or_else(|| tags.show.clone().filter(|_| series.title == UNKNOWN_SHOW))
                    .unwrap_or_else(|| series.title.clone()),
                year: show.and_then(|nfo| nfo.year).or(series.year),
            };
            (Some(display), show.and_then(|nfo| nfo.ids.pin()))
        }
        MediaInference::Unclassifiable(_) => (None, None),
    };
    Classification {
        inference,
        display,
        pin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::nfo::parse_nfo;
    use proptest::prelude::*;

    fn nfo(text: &str) -> Nfo {
        parse_nfo(text.as_bytes()).expect("a valid NFO")
    }

    fn guess(title: &str, year: Option<u32>) -> TitleGuess {
        TitleGuess {
            title: title.to_string(),
            year,
        }
    }

    fn episode_of(classification: &Classification) -> &EpisodeInference {
        match &classification.inference {
            MediaInference::Episode(episode) => episode,
            other => panic!("expected an episode, got {other:?}"),
        }
    }

    fn movie_of(classification: &Classification) -> &MovieInference {
        match &classification.inference {
            MediaInference::Movie(movie) => movie,
            other => panic!("expected a movie, got {other:?}"),
        }
    }

    #[test]
    fn an_episode_nfo_overrides_a_movie_looking_name() {
        let path = Path::new("Lost/Pilot Part One.mkv");
        assert!(matches!(infer_media(path), MediaInference::Movie(_)));
        let file_nfo = nfo(
            "<episodedetails><title>Pilot (1)</title><season>1</season><episode>1</episode></episodedetails>\
             <episodedetails><title>Pilot (2)</title><season>1</season><episode>2</episode></episodedetails>",
        );
        let classified = classify(
            path,
            &Hints {
                file_nfo: Some(&file_nfo),
                ..Hints::NONE
            },
        );
        let episode = episode_of(&classified);
        assert_eq!(
            episode.series,
            guess("Lost", None),
            "the folder keys the show"
        );
        assert_eq!(
            (episode.season, episode.first_episode, episode.last_episode),
            (1, 1, Some(2))
        );
        assert_eq!(episode.episode_title.as_deref(), Some("Pilot (1)"));
        assert_eq!(classified.pin, None, "an episode NFO pins nothing");
    }

    #[test]
    fn an_episode_nfo_wins_over_the_marker_in_the_name() {
        let path = Path::new("Show/Season 1/Show.S01E05.mkv");
        let file_nfo =
            nfo("<episodedetails><season>1</season><episode>6</episode></episodedetails>");
        let classified = classify(
            path,
            &Hints {
                file_nfo: Some(&file_nfo),
                ..Hints::NONE
            },
        );
        assert_eq!(episode_of(&classified).first_episode, 6);
        assert_eq!(episode_of(&classified).series, guess("Show", None));
    }

    #[test]
    fn a_movie_nfo_makes_a_movie_keyed_by_the_path_and_shown_as_the_nfo_says() {
        let path = Path::new("Films/The.Matrix.S01E01.mkv");
        let file_nfo = nfo(
            r#"<movie><title>The Matrix</title><year>1999</year><edition>Remastered</edition>
               <uniqueid type="tmdb" default="true">603</uniqueid></movie>"#,
        );
        let classified = classify(
            path,
            &Hints {
                file_nfo: Some(&file_nfo),
                ..Hints::NONE
            },
        );
        let movie = movie_of(&classified);
        assert_eq!(
            movie.title,
            movie_reading(path).title,
            "the key is the path's"
        );
        assert_eq!(movie.edition.as_deref(), Some("Remastered"));
        assert_eq!(classified.display, Some(guess("The Matrix", Some(1999))));
        assert_eq!(classified.pin, Some(ProviderPin::Tmdb(603)));
    }

    #[test]
    fn a_tvshow_nfo_shows_and_pins_the_series_its_folder_keys() {
        let path = Path::new("GoT/Season 01/GoT.S01E01.mkv");
        let show_nfo = nfo(r#"<tvshow><title>Game of Thrones</title><year>2011</year>
               <uniqueid type="tvdb">121361</uniqueid><uniqueid type="tmdb">1399</uniqueid></tvshow>"#);
        let classified = classify(
            path,
            &Hints {
                show_nfo: Some(&show_nfo),
                ..Hints::NONE
            },
        );
        assert_eq!(episode_of(&classified).series, guess("GoT", None));
        assert_eq!(
            classified.display,
            Some(guess("Game of Thrones", Some(2011)))
        );
        assert_eq!(classified.pin, Some(ProviderPin::Tmdb(1399)));
    }

    #[test]
    fn an_nfo_of_the_other_kind_neither_shows_nor_pins() {
        let path = Path::new("Heat (1995)/Heat (1995).mkv");
        let tv_url = nfo("https://www.themoviedb.org/tv/1399");
        let classified = classify(
            path,
            &Hints {
                file_nfo: Some(&tv_url),
                ..Hints::NONE
            },
        );
        assert_eq!(classified.pin, None, "a show's TMDB id never pins a movie");
        assert_eq!(classified.display, Some(guess("Heat", Some(1995))));

        let movie_url = nfo("https://www.themoviedb.org/movie/949");
        let pinned = classify(
            path,
            &Hints {
                file_nfo: Some(&movie_url),
                ..Hints::NONE
            },
        );
        assert_eq!(pinned.pin, Some(ProviderPin::Tmdb(949)));
    }

    #[test]
    fn tags_make_an_episode_of_a_file_the_path_could_not_place() {
        let tags = ContainerTags::from_tags([
            ("show", "The Office"),
            ("season_number", "2"),
            ("episode_sort", "3"),
            ("title", "The Dundies"),
        ]);
        let classified = classify(
            Path::new("The Office/The Dundies.m4v"),
            &Hints {
                tags: &tags,
                ..Hints::NONE
            },
        );
        let episode = episode_of(&classified);
        assert_eq!(episode.series, guess("The Office", None));
        assert_eq!((episode.season, episode.first_episode), (2, 3));
        assert_eq!(
            episode.episode_title, None,
            "a title that is the filename is the encoder's, not the episode's"
        );

        let season_folder = classify(
            Path::new("Show/Season 4/Finale.mkv"),
            &Hints {
                tags: &ContainerTags::from_tags([("episode_sort", "12")]),
                ..Hints::NONE
            },
        );
        assert_eq!(
            (
                episode_of(&season_folder).season,
                episode_of(&season_folder).first_episode
            ),
            (4, 12),
            "the season folder's season, the tag's episode"
        );
    }

    #[test]
    fn tags_never_override_the_path() {
        let tags = ContainerTags::from_tags([
            ("show", "Something Else"),
            ("season_number", "9"),
            ("episode_sort", "9"),
            ("date", "1980"),
        ]);
        let path = Path::new("Breaking Bad/Season 1/Breaking.Bad.S01E02.mkv");
        let classified = classify(
            path,
            &Hints {
                tags: &tags,
                ..Hints::NONE
            },
        );
        assert_eq!(classified.inference, infer_media(path));
        assert_eq!(
            classified.display.map(|d| d.title),
            Some("Breaking Bad".to_string()),
            "a tag never renames a show the path names"
        );
        let unnamed = classify(
            Path::new("S01E02.mkv"),
            &Hints {
                tags: &tags,
                ..Hints::NONE
            },
        );
        assert_eq!(
            unnamed.display.map(|d| d.title),
            Some("Something Else".to_string()),
            "a tag names a show the path does not"
        );

        let movie = classify(
            Path::new("Heat (1995)/Heat (1995).mkv"),
            &Hints {
                tags: &ContainerTags::from_tags([("year", "2001")]),
                ..Hints::NONE
            },
        );
        assert_eq!(
            movie.display,
            Some(guess("Heat", Some(1995))),
            "path year before tag year"
        );
        let yearless = classify(
            Path::new("Heat.mkv"),
            &Hints {
                tags: &ContainerTags::from_tags([("DATE", "1995-12-15")]),
                ..Hints::NONE
            },
        );
        assert_eq!(
            yearless.display,
            Some(guess("Heat", Some(1995))),
            "a tag fills a missing year"
        );
    }

    #[test]
    fn a_tag_title_of_release_noise_is_ignored() {
        let classified = classify(
            Path::new("Show/Season 1/Show.S01E01.mkv"),
            &Hints {
                tags: &ContainerTags::from_tags([("title", "1080p.x264")]),
                ..Hints::NONE
            },
        );
        assert_eq!(episode_of(&classified).episode_title, None);
    }

    #[test]
    fn container_tag_values_are_read_only_when_they_say_what_they_claim() {
        let tags = ContainerTags::from_tags([
            ("SEASON_NUMBER", "two"),
            ("episode_sort", "-1"),
            ("date", "20190501"),
            ("show", "   "),
        ]);
        assert_eq!(tags, ContainerTags::default());
        assert_eq!(
            ContainerTags::from_tags([("date", "2019-05-01T00:00:00Z")]).year,
            Some(2019)
        );
    }

    proptest! {
        #[test]
        fn with_no_hints_classification_is_path_inference(
            dirs in proptest::collection::vec("[A-Za-z0-9 ._()-]{1,16}", 0..3),
            name in "[A-Za-z0-9 ._()-]{1,24}",
        ) {
            let mut path = std::path::PathBuf::new();
            for dir in &dirs {
                path.push(dir);
            }
            path.push(format!("{name}.mkv"));
            let classified = classify(&path, &Hints::NONE);
            prop_assert_eq!(&classified.inference, &infer_media(&path));
            prop_assert_eq!(classified.pin, None);
            // What a title is shown as is what it is keyed by.
            match &classified.inference {
                MediaInference::Movie(movie) => {
                    prop_assert_eq!(classified.display, Some(movie.title.clone()))
                }
                MediaInference::Episode(episode) => {
                    prop_assert_eq!(classified.display, Some(episode.series.clone()))
                }
                MediaInference::Unclassifiable(_) => prop_assert_eq!(classified.display, None),
            }
        }

        #[test]
        fn hints_never_change_the_key_a_title_is_matched_by(
            dirs in proptest::collection::vec("[A-Za-z0-9 ._()-]{1,16}", 1..3),
            name in "[A-Za-z0-9 ._()-]{1,24}",
            title in "[A-Za-z ]{1,12}",
            year in 1900u32..2030,
        ) {
            let mut path = std::path::PathBuf::new();
            for dir in &dirs {
                path.push(dir);
            }
            path.push(format!("{name}.mkv"));
            let file_nfo = nfo(&format!("<movie><title>{title}</title><year>{year}</year></movie>"));
            let classified = classify(&path, &Hints { file_nfo: Some(&file_nfo), ..Hints::NONE });
            prop_assert_eq!(movie_of(&classified).title.clone(), movie_reading(&path).title);
        }
    }
}
