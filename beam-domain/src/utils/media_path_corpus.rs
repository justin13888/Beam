//! A corpus of real-world library layouts and what each path is.
//!
//! Every row is a path relative to a library root, as it appears in actual
//! collections (scene releases, Plex/Jellyfin naming, fansub releases), and a
//! one-line description of the expected inference. The description format is
//! the test's own, so a row reads as the answer a person would give.

use std::path::Path;

use super::*;

/// `episode <series>|<year> s<season> e<first>[-<last>] [<title>] <numbering>
/// [@<air date>] [!folder<n>]`, `movie <title>|<year> [ed=<edition>]`, or
/// `unclassifiable season <n>`.
fn describe(inference: &MediaInference) -> String {
    fn year(year: Option<u32>) -> String {
        year.map_or_else(|| "-".to_string(), |y| y.to_string())
    }
    match inference {
        MediaInference::Episode(EpisodeInference {
            series,
            season,
            first_episode,
            last_episode,
            air_date,
            episode_title,
            numbering,
            contradicted_season_folder,
        }) => {
            let mut out = format!(
                "episode {}|{} s{season} e{first_episode}",
                series.title,
                year(series.year)
            );
            if let Some(last) = last_episode {
                out.push_str(&format!("-{last}"));
            }
            if let Some(title) = episode_title {
                out.push_str(&format!(" [{title}]"));
            }
            out.push_str(&format!(" {numbering:?}"));
            if let Some(date) = air_date {
                out.push_str(&format!(" @{date}"));
            }
            if let Some(folder) = contradicted_season_folder {
                out.push_str(&format!(" !folder{folder}"));
            }
            out
        }
        MediaInference::Movie(MovieInference { title, edition }) => {
            let mut out = format!("movie {}|{}", title.title, year(title.year));
            if let Some(edition) = edition {
                out.push_str(&format!(" ed={edition}"));
            }
            out
        }
        MediaInference::Unclassifiable(UnclassifiableReason::NoEpisodeNumberInSeasonFolder {
            season,
        }) => format!("unclassifiable season {season}"),
    }
}

const CORPUS: &[(&str, &str)] = &[
    // Season-folder layouts: the show is the season folder's parent.
    (
        "Show Name/Season 01/Show.Name.S01E02.The.Title.1080p.WEB.mkv",
        "episode Show Name|- s1 e2 [The Title] Standard",
    ),
    (
        "Show Name/Season 1/Show Name - S01E02.mkv",
        "episode Show Name|- s1 e2 Standard",
    ),
    (
        "Show Name/S01/Show.Name.S01E03.mkv",
        "episode Show Name|- s1 e3 Standard",
    ),
    (
        "Show Name/Season 01/S01E02.mkv",
        "episode Show Name|- s1 e2 Standard",
    ),
    (
        "Show Name/Staffel 2/Show.Name.S02E01.mkv",
        "episode Show Name|- s2 e1 Standard",
    ),
    (
        "Show Name/Temporada 3/Show.Name.3x04.mkv",
        "episode Show Name|- s3 e4 Standard",
    ),
    (
        "Show Name/Saison 1/Show.Name.S01E05.mkv",
        "episode Show Name|- s1 e5 Standard",
    ),
    (
        "Show Name/series-2/Show.Name.S02E01.mkv",
        "episode Show Name|- s2 e1 Standard",
    ),
    (
        "Show (2005)/Season 1/Show.(2005).S01E01.Pilot.mkv",
        "episode Show|2005 s1 e1 [Pilot] Standard",
    ),
    (
        "Show Name/Season 01/Show Name - 1x02 - Title.mkv",
        "episode Show Name|- s1 e2 [Title] Standard",
    ),
    // Specials.
    (
        "Show Name/Specials/Show.Name.S00E01.mkv",
        "episode Show Name|- s0 e1 Standard",
    ),
    (
        "Show Name/Season 0/Show.Name.S00E02.mkv",
        "episode Show Name|- s0 e2 Standard",
    ),
    (
        "Show Name/Special/Show.Name.S00E03.mkv",
        "episode Show Name|- s0 e3 Standard",
    ),
    // A season folder straight under the root: the series is the filename's.
    (
        "Season 01/Show.Name.S01E02.mkv",
        "episode Show Name|- s1 e2 Standard",
    ),
    (
        "Season 01/S01E02.mkv",
        "episode Unknown Show|- s1 e2 Standard",
    ),
    // The filename's marker beats a season folder that disagrees.
    (
        "Show Name/Season 01/Show.Name.S02E05.mkv",
        "episode Show Name|- s2 e5 Standard !folder1",
    ),
    // Flat layouts: the parent folder, or at the root the filename.
    ("Show.Name.S01E02.mkv", "episode Show Name|- s1 e2 Standard"),
    (
        "Show Name (2019)/Show.Name.S01E02.mkv",
        "episode Show Name|2019 s1 e2 Standard",
    ),
    (
        "Show Name/Show.Name.1x02.Title.mkv",
        "episode Show Name|- s1 e2 [Title] Standard",
    ),
    ("Show.2019.S01E02.mkv", "episode Show|2019 s1 e2 Standard"),
    ("1923.S01E01.mkv", "episode 1923|- s1 e1 Standard"),
    ("1923/1923.S01E01.mkv", "episode 1923|- s1 e1 Standard"),
    (
        "Show.S01E01.1080p.WEB-S00E00.mkv",
        "episode Show|- s1 e1 Standard",
    ),
    ("Show/Show.S01E01v2.mkv", "episode Show|- s1 e1 Standard"),
    (
        "My Show/My.Show.S10E100.mkv",
        "episode My Show|- s10 e100 Standard",
    ),
    (
        "Show/Show.S01E01.Pilot.2019.mkv",
        "episode Show|- s1 e1 [Pilot] Standard",
    ),
    // Multi-episode files.
    (
        "Show Name/Season 01/Show.Name.S01E01E02.mkv",
        "episode Show Name|- s1 e1-2 Standard",
    ),
    (
        "Show Name/Season 01/Show.Name.S01E01-E03.mkv",
        "episode Show Name|- s1 e1-3 Standard",
    ),
    (
        "Show Name/Season 01/Show.Name.S01E01-03.mkv",
        "episode Show Name|- s1 e1-3 Standard",
    ),
    // Date-based episodes: the season is the year, the episode MMDD.
    (
        "The Daily Show/The.Daily.Show.2024.03.01.Guest.720p.mkv",
        "episode The Daily Show|- s2024 e301 [Guest] Daily @2024-03-01",
    ),
    (
        "Show Name/Season 2024/Show.2024.03.01.mkv",
        "episode Show Name|- s2024 e301 Daily @2024-03-01",
    ),
    (
        "Show Name/Season 03/Show.Name.2024-12-25.mkv",
        "episode Show Name|- s2024 e1225 Daily @2024-12-25 !folder3",
    ),
    // Absolute numbering, only where the layout makes it unambiguous.
    (
        "Show (1998)/[G] Show - 012 [1080p].mkv",
        "episode Show|1998 s1 e12 Absolute",
    ),
    (
        "Show (1998)/Season 2/[G] Show - 03v2 [1080p].mkv",
        "episode Show|1998 s2 e3 Absolute",
    ),
    (
        "Anime (2004)/Season 1/[SubsPlease] Anime - 07 (1080p) [ABCD].mkv",
        "episode Anime|2004 s1 e7 Absolute",
    ),
    ("Show/Show - 05.mkv", "episode Show|- s1 e5 Absolute"),
    ("Show/E05.mkv", "episode Show|- s1 e5 Absolute"),
    ("Show/EP12.mkv", "episode Show|- s1 e12 Absolute"),
    // ... and not where it is not: a folder naming another title, the root,
    // or a year where the number would be.
    (
        "Other Folder/[G] Show - 012 [1080p].mkv",
        "movie Show - 012|-",
    ),
    ("[G] Show - 012 [1080p].mkv", "movie Show - 012|-"),
    ("Show/Show - 2019.mkv", "movie Show|2019"),
    // A season folder holding a file with no episode number.
    (
        "Show Name/Season 01/Behind the Scenes.mkv",
        "unclassifiable season 1",
    ),
    ("Show/Season 1/05.mkv", "unclassifiable season 1"),
    ("Show/Specials/Making Of.mkv", "unclassifiable season 0"),
    // Movies: identity is title and year.
    (
        "Apollo 13 (1995)/Apollo.13.1995.1080p.BluRay.mkv",
        "movie Apollo 13|1995",
    ),
    ("Movies/1917.2019.1080p.mkv", "movie 1917|2019"),
    ("Blade.Runner.2049.2017.mkv", "movie Blade Runner 2049|2017"),
    ("Dune (1984)/Dune.1984.mkv", "movie Dune|1984"),
    ("Dune (2021)/Dune.2021.2160p.mkv", "movie Dune|2021"),
    ("Movie.1080p.2019.x265.mkv", "movie Movie|2019"),
    (
        "Some.Movie.2019.2160p.UHD.BluRay.x265-GROUP.mkv",
        "movie Some Movie|2019",
    ),
    ("Movies/Avatar.mkv", "movie Avatar|-"),
    ("Wall-E (2008).mkv", "movie Wall-E|2008"),
    ("Kids/Wall-E (2008).mkv", "movie Wall-E|2008"),
    (
        "Movie Title (2010)/Movie Title (2010) - 1080p.mkv",
        "movie Movie Title|2010",
    ),
    // Editions.
    (
        "Movie (2019) {edition-Final Cut}.mkv",
        "movie Movie|2019 ed=Final Cut",
    ),
    (
        "Movie.2019.Directors.Cut.1080p.mkv",
        "movie Movie|2019 ed=Director's Cut",
    ),
    (
        "Movie (2019)/Movie.2019.Extended.Remastered.mkv",
        "movie Movie|2019 ed=Extended, Remastered",
    ),
    (
        "Movie (2019)/Movie (2019) {edition-Director's Cut}.mkv",
        "movie Movie|2019 ed=Director's Cut",
    ),
];

#[test]
fn the_corpus_infers_what_each_path_is() {
    let failures: Vec<String> = CORPUS
        .iter()
        .filter_map(|(path, expected)| {
            let actual = describe(&infer_media(Path::new(path)));
            (actual != *expected)
                .then(|| format!("{path}\n  expected {expected}\n  actual   {actual}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn season_folder_names() {
    let cases = [
        ("Season 01", Some(1)),
        ("season1", Some(1)),
        ("SEASON_2", Some(2)),
        ("Series 3", Some(3)),
        ("Saison 4", Some(4)),
        ("Staffel 5", Some(5)),
        ("Temporada 6", Some(6)),
        ("S07", Some(7)),
        ("s2024", Some(2024)),
        ("Specials", Some(0)),
        ("special", Some(0)),
        ("Season 01 (2019)", None),
        ("Seasons", None),
        ("Show Name", None),
        ("S", None),
    ];
    for (name, expected) in cases {
        assert_eq!(season_folder_number(name), expected, "{name}");
    }
}

/// Two releases of one show -- one in season folders, one flat -- key to the
/// same show, which is what makes the season-folder rule matter: the old
/// parent-folder rule keyed the first to a show called "Season 01".
#[test]
fn a_season_folder_and_a_flat_layout_key_the_same_show() {
    let key = |path: &str| match infer_media(Path::new(path)) {
        MediaInference::Episode(episode) => episode.series.identity_key(),
        other => panic!("{path} is not an episode: {other:?}"),
    };
    assert_eq!(
        key("Show Name/Season 01/Show.Name.S01E01.mkv"),
        key("Show Name/Show.Name.S01E02.mkv")
    );
}

mod properties {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn inference_never_panics(path in ".*") {
            let _ = infer_media(Path::new(&path));
        }

        /// Whatever the filename, a file in a season folder is never a movie:
        /// it is an episode or it is unclassifiable.
        #[test]
        fn a_file_in_a_season_folder_is_never_a_movie(
            show in "[A-Za-z ]{1,12}",
            season in 0u32..30,
            stem in "[^/]{0,30}",
        ) {
            let path = format!("{show}/Season {season}/{stem}.mkv");
            prop_assert!(
                !matches!(infer_media(Path::new(&path)), MediaInference::Movie(_)),
                "{path}"
            );
        }

        /// A daily episode's number is its month and day.
        #[test]
        fn a_daily_episode_number_is_month_and_day(
            date in (1990i32..2030, 1u32..13, 1u32..29)
                .prop_map(|(y, m, d)| NaiveDate::from_ymd_opt(y, m, d).expect("valid")),
        ) {
            let path = format!("News/News.{}.mkv", date.format("%Y.%m.%d"));
            match infer_media(Path::new(&path)) {
                MediaInference::Episode(episode) => {
                    prop_assert_eq!(episode.season as i32, date.year());
                    prop_assert_eq!(episode.first_episode, date.month() * 100 + date.day());
                    prop_assert_eq!(episode.air_date, Some(date));
                }
                other => prop_assert!(false, "{path} is not an episode: {other:?}"),
            }
        }
    }
}
