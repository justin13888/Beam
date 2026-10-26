//! What a library's own files say about their titles: NFO pins and
//! container tags (#184), parts stacked into one source (#233), and a dated
//! folder whose discs sit in `Disc N` subfolders (#234).

use beam_test_support::e2e::client::api;

use crate::common::{Admin, Container, Video, movie_of, show_of};

const MATRIX_NFO: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<movie>
  <title>The Matrix</title>
  <year>1999</year>
  <uniqueid type="imdb">tt0133093</uniqueid>
  <uniqueid type="tmdb" default="true">603</uniqueid>
</movie>"#;

/// An NFO beside a file names its title and pins it to a provider id, with
/// no provider configured; container tags place a file as an episode its
/// path could not.
#[tokio::test]
async fn an_nfo_pins_a_movie_and_container_tags_place_an_episode() {
    let mut admin = Admin::start().await;
    let tree = &mut admin.stack.library;
    tree.add_video("films/Matrix/matrix.1080p.mp4", &Video::new(440));
    tree.write("films/Matrix/movie.nfo", MATRIX_NFO);
    tree.add_video(
        "shows/The Office/The Dundies.mp4",
        &Video::new(550)
            .tag("show", "The Office")
            .tag("season_number", "2")
            .tag("episode_sort", "1")
            .tag("title", "Dundies Night"),
    );
    let films = admin.add_library("Films", "films").await;
    let shows = admin.add_library("Shows", "shows").await;
    admin.scan_ok(films).await;
    admin.scan_ok(shows).await;

    let titles = admin.titles().await;
    let matrix = titles
        .iter()
        .find(|title| !title.is_show)
        .expect("the movie is browsed");
    assert_eq!(
        (matrix.name.as_str(), matrix.year),
        ("The Matrix", Some(1999)),
        "the NFO names the movie, not its file name"
    );
    let enrichment = admin
        .api
        .get_media_enrichment(matrix.id)
        .await
        .expect("read the movie's enrichment")
        .into_inner();
    assert_eq!(enrichment.pinned_ref.as_deref(), Some("tmdb:603"));
    assert_eq!(
        enrichment.pin_source,
        Some(api::types::MediaEnrichmentpinSource::Nfo)
    );
    assert_eq!(
        enrichment.matched_ref, None,
        "fetched by its pin, never matched"
    );

    let office = titles
        .iter()
        .find(|title| title.is_show)
        .expect("the show is browsed");
    assert_eq!(office.name, "The Office");
    let show = show_of(
        admin
            .api
            .get_media_detail(office.id)
            .await
            .expect("read the show")
            .into_inner(),
    );
    let placed: Vec<(i64, Vec<(i64, String)>)> = show
        .seasons
        .iter()
        .map(|season| {
            (
                season.season_number,
                season
                    .episodes
                    .iter()
                    .map(|episode| (episode.episode_number, episode.title.clone()))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        placed,
        [(2, vec![(1, "Dundies Night".to_owned())])],
        "season, episode and title come from the tags"
    );

    admin.finish().await;
}

/// Parts of one movie in one folder are one source that plays them in turn;
/// a dated folder holding a DVD rip per `Disc N` subfolder is one title of
/// one source, the discs' main titles its parts.
#[tokio::test]
async fn parts_stack_into_one_source_and_disc_folders_into_one_title() {
    let mut admin = Admin::start().await;
    let tree = &mut admin.stack.library;
    let cd1 = Video::new(440).seconds(6);
    let cd2 = Video::new(550).seconds(4);
    tree.add_video("films/Heat (1995)/Heat (1995) - CD1.mp4", &cd1);
    tree.add_video("films/Heat (1995)/Heat (1995) - CD2.mp4", &cd2);
    let dvd = |tone| Video::new(tone).container(Container::Vob);
    tree.add_video("films/Alien (1979)/Disc 1/VIDEO_TS/VTS_01_1.VOB", &dvd(660));
    tree.add_video("films/Alien (1979)/Disc 2/VIDEO_TS/VTS_01_1.VOB", &dvd(770));
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;

    let titles = admin.titles().await;
    let names: Vec<(&str, Option<i64>)> = titles
        .iter()
        .map(|title| (title.name.as_str(), title.year))
        .collect();
    assert_eq!(
        names,
        [("Alien", Some(1979)), ("Heat", Some(1995))],
        "two titles, named by their folders"
    );
    let files = admin.files(library).await;
    let id_of = |path: &str| {
        files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("no file {path}"))
            .id
    };

    let heat = admin
        .api
        .get_media_sources(titles[1].id)
        .await
        .expect("read Heat's sources")
        .into_inner()
        .items;
    assert_eq!(heat.len(), 1, "two parts, one source");
    let parts: Vec<_> = heat[0]
        .parts
        .iter()
        .map(|part| (part.file_id, part.part_number))
        .collect();
    assert_eq!(
        parts,
        [
            (id_of("Heat (1995)/Heat (1995) - CD1.mp4"), Some(1)),
            (id_of("Heat (1995)/Heat (1995) - CD2.mp4"), Some(2)),
        ]
    );
    assert_eq!(heat[0].file_id, parts[0].0, "the source starts with part 1");
    assert_eq!(heat[0].disc_structure, None);
    let total = heat[0].duration_secs.expect("both parts were probed");
    assert!(
        (total - (cd1.duration_secs() + cd2.duration_secs())).abs() < 1.0,
        "the source lasts as long as its parts together ({total}s)"
    );
    let movie = movie_of(
        admin
            .api
            .get_media_detail(titles[1].id)
            .await
            .expect("read Heat")
            .into_inner(),
    );
    assert_eq!(movie.source_count, Some(1));

    let alien = admin
        .api
        .get_media_sources(titles[0].id)
        .await
        .expect("read Alien's sources")
        .into_inner()
        .items;
    assert_eq!(alien.len(), 1, "two discs, one source");
    assert_eq!(
        alien[0].disc_structure,
        Some(api::types::MediaSourcediscStructure::Dvd)
    );
    let discs: Vec<_> = alien[0]
        .parts
        .iter()
        .map(|part| (part.file_id, part.part_number))
        .collect();
    assert_eq!(
        discs,
        [
            (id_of("Alien (1979)/Disc 1/VIDEO_TS/VTS_01_1.VOB"), Some(1)),
            (id_of("Alien (1979)/Disc 2/VIDEO_TS/VTS_01_1.VOB"), Some(2)),
        ],
        "the discs play in order"
    );

    admin.finish().await;
}
