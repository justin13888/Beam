//! A corpus of real-world library layouts and what each path is.
//!
//! Every row is a path relative to a library root, as it appears in actual
//! collections (scene releases, Plex/Jellyfin naming, fansub releases), and a
//! one-line description of the expected inference. The description format is
//! the test's own, so a row reads as the answer a person would give.

use std::path::Path;

use super::*;

/// `episode <series>|<year> s<season> e<first>[-<last>] [<title>] <numbering>
/// [@<air date>] [!folder<n>]`, `movie <title>|<year> [ed=<edition>]`,
/// `unclassifiable season <n>`, `unclassifiable absolute <n>`, or
/// `unclassifiable fractional <n>.<d>`.
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
        MediaInference::Unclassifiable(UnclassifiableReason::AmbiguousAbsoluteNumber {
            number,
        }) => {
            format!("unclassifiable absolute {number}")
        }
        MediaInference::Unclassifiable(UnclassifiableReason::FractionalAbsoluteNumber {
            whole,
            tenth,
        }) => format!("unclassifiable fractional {whole}.{tenth}"),
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
    // A season token anywhere in the folder name (D1): the series is still
    // the series folder, never the season folder.
    (
        "Breaking Bad/Breaking Bad Season 1/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    (
        "Breaking Bad/Season 1 (2008)/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    (
        "Breaking Bad/Season 01 - Pilot Season/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    (
        "Breaking Bad (2008)/Breaking Bad Season 2/Breaking.Bad.S02E01.mkv",
        "episode Breaking Bad|2008 s2 e1 Standard",
    ),
    (
        "Doctor Who/Series 11/Doctor.Who.S11E01.mkv",
        "episode Doctor Who|- s11 e1 Standard",
    ),
    // A box set's folder names the show and then the box: the filename's
    // show, when the rest is nothing but box-set words. Any other word keeps
    // the folder's title.
    (
        "Breaking Bad Complete Series/Season 1/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    (
        "The Wire - The Complete Collection/Season 2/The.Wire.S02E01.mkv",
        "episode The Wire|- s2 e1 Standard",
    ),
    (
        "Firefly The Series Collection/Season 1/Firefly.S01E01.mkv",
        "episode Firefly|- s1 e1 Standard",
    ),
    // A box set's folder that names only the box: the filename's show.
    (
        "The Complete Series/Season 1/Show.S01E01.mkv",
        "episode Show|- s1 e1 Standard",
    ),
    (
        "TV/Complete Collection/Season 2/Firefly.S02E01.mkv",
        "episode Firefly|- s2 e1 Standard",
    ),
    (
        "Doctor Who Classic/Season 1/Doctor.Who.S01E01.mkv",
        "episode Doctor Who Classic|- s1 e1 Standard",
    ),
    // A multi-season pack: its season range ends the show's name, as a
    // season token does, so the pack names the same show as its flat and
    // per-season forms.
    (
        "Breaking.Bad.S01-S05.COMPLETE.1080p.BluRay/Season 1/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    (
        "Breaking.Bad.S01-S05.1080p.BluRay.x264-GRP/Season 1/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    (
        "The.Wire.S01-S05.1080p/Season 01/The.Wire.S01E01.mkv",
        "episode The Wire|- s1 e1 Standard",
    ),
    (
        "Breaking Bad Seasons 1-5/Season 1/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    (
        "Breaking Bad Seasons 1 to 5/Season 2/Breaking.Bad.S02E01.mkv",
        "episode Breaking Bad|- s2 e1 Standard",
    ),
    (
        "Breaking Bad (2008) Season 1-5/Season 1/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|2008 s1 e1 Standard",
    ),
    (
        "Breaking.Bad.S01-05.720p/Season 3/Breaking.Bad.S03E01.mkv",
        "episode Breaking Bad|- s3 e1 Standard",
    ),
    (
        "Breaking Bad Complete S01-S05/Season 1/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    (
        "TV/Breaking Bad/Breaking.Bad.S01-S05.1080p/Season 1/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    // Flat in the pack: the range is no one season's folder, so no file's
    // season contradicts it.
    (
        "Breaking.Bad.S01-S05.1080p.BluRay/Breaking.Bad.S03E01.mkv",
        "episode Breaking Bad|- s3 e1 Standard",
    ),
    // A season pack: the text before the season token names the show when
    // there is no series folder, or when the folder above is a category.
    (
        "The.Office.US.S02.1080p.BluRay.x264-GRP/The.Office.US.S02E01.1080p.mkv",
        "episode The Office US|- s2 e1 Standard",
    ),
    (
        "TV/The.Office.US.S02.1080p.BluRay.x264-GRP/The.Office.US.S02E01.mkv",
        "episode The Office US|- s2 e1 Standard",
    ),
    (
        "The Office (US)/The.Office.US.S02.720p/The.Office.US.S02E03.mkv",
        "episode The Office US|- s2 e3 Standard",
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
    // ... unless the filename names another show than the folder (D182-C1).
    (
        "TV Shows/Breaking.Bad.S01E01.mkv",
        "episode Breaking Bad|- s1 e1 Standard",
    ),
    ("Kids/Bluey.S01E01.mkv", "episode Bluey|- s1 e1 Standard"),
    // Scene names drop a title's apostrophes; the folder's spelling is still
    // the same show, so it keeps the show and its year (D10).
    (
        "Grey's Anatomy/Season 2/Greys.Anatomy.S02E01.mkv",
        "episode Grey's Anatomy|- s2 e1 Standard",
    ),
    (
        "Grey's Anatomy/Greys.Anatomy.S03.1080p.WEB-GRP/Greys.Anatomy.S03E01.mkv",
        "episode Grey's Anatomy|- s3 e1 Standard",
    ),
    (
        "Grey's Anatomy (2005)/Greys.Anatomy.S01E01.mkv",
        "episode Grey's Anatomy|2005 s1 e1 Standard",
    ),
    (
        "Bob's Burgers/Bobs.Burgers.S01E01.mkv",
        "episode Bob's Burgers|- s1 e1 Standard",
    ),
    (
        "Schitt\u{2019}s Creek (2015)/Schitts.Creek.S01E01.mkv",
        "episode Schitt\u{2019}s Creek|2015 s1 e1 Standard",
    ),
    (
        "The Handmaid's Tale/The.Handmaids.Tale.S01E01.mkv",
        "episode The Handmaid's Tale|- s1 e1 Standard",
    ),
    // A split marker (D182-C5).
    ("Show/Show.S01.E01.mkv", "episode Show|- s1 e1 Standard"),
    ("Show S01 E02.mkv", "episode Show|- s1 e2 Standard"),
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
    // Multi-episode files, one marker or several back to back (D6).
    (
        "Show/Show.S01E01.S01E02.mkv",
        "episode Show|- s1 e1-2 Standard",
    ),
    ("Show.1x01.1x02.mkv", "episode Show|- s1 e1-2 Standard"),
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
    // A year-shaped number is an episode only in a season folder of the show
    // the title names; elsewhere it is the release year.
    (
        "One Piece (1999)/Season 1/One Piece - 1999.mkv",
        "episode One Piece|1999 s1 e1999 Absolute",
    ),
    ("Show/Show - 2019.mkv", "movie Show|2019"),
    ("Show/Season 1/Other - 2019.mkv", "unclassifiable season 1"),
    // A fractional number is a recap or special between two episodes, not
    // the first of them with a title of `5`.
    (
        "Show (1998)/[G] Show - 12.5 [1080p].mkv",
        "unclassifiable fractional 12.5",
    ),
    (
        "Show/Season 1/Show - 12.5.mkv",
        "unclassifiable fractional 12.5",
    ),
    ("Show/Show - 12.1080p.mkv", "episode Show|- s1 e12 Absolute"),
    // A `<title> - <n>` nothing around names as a show is not a movie either
    // (D182-C4): a folder naming another title -- romaji against English --
    // or the root.
    (
        "Other Folder/[G] Show - 012 [1080p].mkv",
        "unclassifiable absolute 12",
    ),
    ("[G] Show - 012 [1080p].mkv", "unclassifiable absolute 12"),
    (
        "Frieren (2023)/[SubsPlease] Sousou no Frieren - 12 (1080p).mkv",
        "unclassifiable absolute 12",
    ),
    // The dash is a movie's when the number is one digit outside a season
    // folder (D8). A year the folder and filename share does not make a
    // two-digit number a movie's part: a show's folder carries its year too.
    ("Movie (2019)/Movie (2019) - 1.mkv", "movie Movie - 1|2019"),
    ("Movie (2019)/Movie - 1.mkv", "movie Movie - 1|-"),
    (
        "Chernobyl (2019)/Chernobyl (2019) - 01.mkv",
        "episode Chernobyl|2019 s1 e1 Absolute",
    ),
    (
        "One Piece (1999)/One Piece (1999) - 1071.mkv",
        "episode One Piece|1999 s1 e1071 Absolute",
    ),
    ("Show/Show - 5.mkv", "movie Show - 5|-"),
    (
        "Show/Season 1/Show - 5.mkv",
        "episode Show|- s1 e5 Absolute",
    ),
    // A season folder holding a file with no episode number.
    (
        "Show Name/Season 01/Behind the Scenes.mkv",
        "unclassifiable season 1",
    ),
    ("Show/Season 1/05.mkv", "unclassifiable season 1"),
    ("Show/Specials/Making Of.mkv", "unclassifiable season 0"),
    // ... and names only a season folder makes readable (D182-C5).
    (
        "Seinfeld/Season 5/501 - The Glasses.mkv",
        "episode Seinfeld|- s5 e1 [The Glasses] Standard",
    ),
    (
        "Seinfeld/Season 5/Seinfeld.502.mkv",
        "episode Seinfeld|- s5 e2 Standard",
    ),
    (
        "Show/Season 5/Episode 1.mkv",
        "episode Show|- s5 e1 Standard",
    ),
    (
        "Show/Season 2/Ep 03 - Title.mkv",
        "episode Show|- s2 e3 [Title] Standard",
    ),
    ("Show/Season 5/601.mkv", "unclassifiable season 5"),
    // Outside one, three digits are not an episode.
    ("Show/Show.101.mkv", "movie Show 101|-"),
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
    ("Movie.Name.1920x1080.mkv", "movie Movie Name|-"),
    // The folder fills in what the filename leaves out (D182-C2): the year
    // of the same title, or the title of a noise-only name.
    ("Kill Bill (2003)/Kill Bill.mkv", "movie Kill Bill|2003"),
    ("Dune (2021)/Dune.2160p.mkv", "movie Dune|2021"),
    ("Movie (2019)/1080p.BluRay.x264.mkv", "movie Movie|2019"),
    ("Movie (2019)/[GRP] REPACK 1080p.mkv", "movie Movie|2019"),
    // ... but not another title's year, and never the library root's.
    (
        "Kill Bill (2003)/Kill Bill - Vol 1.mkv",
        "movie Kill Bill - Vol 1|-",
    ),
    ("Avatar.mkv", "movie Avatar|-"),
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
    // Edition words after a parenthesised year (D4), and noise words that
    // open a title (D5).
    (
        "Movie (2019)/Movie (2019) Director's Cut.mkv",
        "movie Movie|2019 ed=Director's Cut",
    ),
    (
        "Blade Runner (1982) The Final Cut.mkv",
        "movie Blade Runner|1982 ed=Final Cut",
    ),
    (
        "Uncut Gems (2019)/Uncut Gems (2019).mkv",
        "movie Uncut Gems|2019",
    ),
    ("Uncut.Gems.2019.1080p.mkv", "movie Uncut Gems|2019"),
    ("IMAX Hubble (2010).mkv", "movie IMAX Hubble|2010"),
    ("S1m0ne (2002)/S1m0ne.2002.mkv", "movie S1m0ne|2002"),
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
        ("Season 01 (2019)", Some(1)),
        ("Breaking Bad Season 1", Some(1)),
        ("Season 01 - Pilot Season", Some(1)),
        ("Doctor Who Series 11", Some(11)),
        ("The.Office.US.S02.1080p.BluRay.x264-GRP", Some(2)),
        ("Seasons", None),
        // A range of seasons is a multi-season pack, not one season.
        ("Breaking.Bad.S01-S05.1080p", None),
        ("Breaking.Bad.S01-05", None),
        ("Breaking Bad Season 1-5", None),
        ("Seasons 1 to 5", None),
        ("Show.S02E01", None),
        ("S1m0ne (2002)", None),
        ("A Series of Unfortunate Events", None),
        ("Show Name", None),
        ("S", None),
    ];
    for (name, expected) in cases {
        assert_eq!(season_folder_number(name), expected, "{name}");
    }
}

/// Disc structures copied whole, as rippers leave them. Inference would read
/// each file as a film of its own (`VTS 01 1`, `00001`) and merge every
/// disc's same-numbered file into one, so the path policy keeps them out of
/// the library before inference is ever asked (playing a disc as its title
/// is issue #189's).
#[test]
fn disc_structures_never_reach_inference() {
    use crate::utils::path_policy::{ExclusionReason, PathDisposition, PathPolicy};

    let policy = PathPolicy::default();
    for path in [
        "Movies/Heat (1995)/VIDEO_TS/VTS_01_1.VOB",
        "Movies/Heat (1995)/VIDEO_TS/VTS_01_0.VOB",
        "Movies/Heat (1995)/VIDEO_TS/VIDEO_TS.VOB",
        "Heat.1995.DVD9/VIDEO_TS/VTS_01_1.VOB",
        "Heat (1995)/BDMV/STREAM/00001.m2ts",
        "Heat.1995.COMPLETE.BLURAY/BDMV/STREAM/00800.m2ts",
        "Heat.1995.COMPLETE.BLURAY/CERTIFICATE/BACKUP/x.m2ts",
    ] {
        assert_eq!(
            policy.disposition(Path::new(path)),
            PathDisposition::Excluded(ExclusionReason::DiscStructure),
            "{path}"
        );
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

/// A folder keeps a title's apostrophes and a scene name drops them: every
/// layout of one show -- season folder, season pack, flat, bare scene file --
/// keys to the same show whichever way each spells it.
#[test]
fn identity_keys_ignore_apostrophes() {
    let key = |path: &str| match infer_media(Path::new(path)) {
        MediaInference::Episode(episode) => episode.series.identity_key(),
        other => panic!("{path} is not an episode: {other:?}"),
    };
    for (folder, scene) in [
        ("Grey's Anatomy", "Greys.Anatomy"),
        ("Bob\u{2019}s Burgers", "Bobs.Burgers"),
        ("The Handmaid's Tale", "The.Handmaids.Tale"),
    ] {
        let keys: std::collections::BTreeSet<String> = [
            format!("{folder}/Season 2/{scene}.S02E01.mkv"),
            format!("{folder}/{scene}.S03.1080p.WEB-GRP/{scene}.S03E01.mkv"),
            format!("{folder}/{scene}.S01E01.mkv"),
            format!("{scene}.S04E01.mkv"),
        ]
        .iter()
        .map(|path| key(path))
        .collect();
        assert_eq!(keys.len(), 1, "{folder}: {keys:?}");
    }
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

        /// A season folder is recognised by its season token wherever it
        /// sits in the name, and the show is always the series folder's --
        /// never the season folder's own name.
        #[test]
        fn a_season_folder_names_the_series_folders_show(
            show in "Q[a-z]{2,8}( Q[a-z]{2,8})?",
            season in 1u32..30,
            episode in 1u32..30,
            before in proptest::sample::select(vec!["", "SHOW ", "SHOW - "]),
            word in proptest::sample::select(vec!["Season ", "season", "Series_", "Staffel.", "S"]),
            after in proptest::sample::select(vec!["", " (2008)", " - Pilot Season", " 1080p"]),
        ) {
            let folder = format!("{}{word}{season:02}{after}", before.replace("SHOW", &show));
            prop_assert_eq!(season_folder_number(&folder), Some(season), "{}", folder);
            let path = format!(
                "{show}/{folder}/{}.S{season:02}E{episode:02}.mkv",
                show.replace(' ', ".")
            );
            match infer_media(Path::new(&path)) {
                MediaInference::Episode(inferred) => {
                    prop_assert_eq!(&inferred.series.title, &show, "{}", path);
                    prop_assert_eq!(inferred.season, season, "{}", path);
                    prop_assert_eq!(inferred.contradicted_season_folder, None, "{}", path);
                }
                other => prop_assert!(false, "{path} is not an episode: {other:?}"),
            }
        }

        /// A flat file's show is the one its filename names, whatever
        /// folder -- a category, a collection -- it sits in.
        #[test]
        fn a_flat_file_names_its_own_show(
            category in "Z[a-z]{2,8}",
            show in "Q[a-z]{2,8}",
            season in 1u32..30,
            episode in 1u32..30,
        ) {
            let path = format!("{category}/{show}.S{season:02}E{episode:02}.mkv");
            match infer_media(Path::new(&path)) {
                MediaInference::Episode(inferred) => {
                    prop_assert_eq!(&inferred.series.title, &show, "{}", path);
                }
                other => prop_assert!(false, "{path} is not an episode: {other:?}"),
            }
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
