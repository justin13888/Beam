//! Sources: one track model, ranked (issue #189).
//!
//! Driven through `DbMetadataService::get_media_sources` and the detail view
//! over in-memory repository fakes, with the shared builders from
//! `metadata_tests`.

use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use super::metadata_tests::tests::{
    assert_urls_name, make_media_file, make_movie, seed_episode, service, service_with_sidecars,
};
use crate::services::metadata::MetadataService;
use beam_domain::models::{MediaFileContent, MovieEntry};
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;

/// A video stream `height` lines tall at `bit_rate`, as the indexer
/// records one.
fn video_stream(
    file_id: Uuid,
    index: u32,
    codec: &str,
    height: u32,
    bit_rate: Option<u64>,
    frame_rate: Option<f64>,
) -> beam_domain::models::CreateMediaStream {
    use beam_domain::models::stream::{StreamMetadata, StreamType, VideoStreamMetadata};
    beam_domain::models::CreateMediaStream {
        file_id,
        index,
        stream_type: StreamType::Video,
        codec: codec.to_string(),
        metadata: StreamMetadata::Video(VideoStreamMetadata {
            width: height * 16 / 9,
            height,
            frame_rate,
            bit_rate,
            color_space: None,
            color_range: None,
            hdr_format: None,
        }),
    }
}

fn audio_stream(
    file_id: Uuid,
    index: u32,
    codec: &str,
    sample_rate: u32,
    is_default: bool,
) -> beam_domain::models::CreateMediaStream {
    use beam_domain::models::stream::{AudioStreamMetadata, StreamMetadata, StreamType};
    beam_domain::models::CreateMediaStream {
        file_id,
        index,
        stream_type: StreamType::Audio,
        codec: codec.to_string(),
        metadata: StreamMetadata::Audio(AudioStreamMetadata {
            language: Some("eng".to_string()),
            title: None,
            channels: 8,
            sample_rate,
            channel_layout: Some("7.1".to_string()),
            bit_rate: None,
            is_default,
            is_forced: false,
        }),
    }
}

fn subtitle_stream(
    file_id: Uuid,
    index: u32,
    codec: &str,
    is_hearing_impaired: bool,
) -> beam_domain::models::CreateMediaStream {
    use beam_domain::models::stream::{StreamMetadata, StreamType, SubtitleStreamMetadata};
    beam_domain::models::CreateMediaStream {
        file_id,
        index,
        stream_type: StreamType::Subtitle,
        codec: codec.to_string(),
        metadata: StreamMetadata::Subtitle(SubtitleStreamMetadata {
            language: Some("eng".to_string()),
            title: None,
            is_default: false,
            is_forced: false,
            is_hearing_impaired,
        }),
    }
}

/// A movie entry of `edition` for `movie_id`.
fn entry(movie_repo: &InMemoryMovieRepository, movie_id: Uuid, edition: Option<&str>) -> Uuid {
    let entry = MovieEntry {
        id: Uuid::new_v4(),
        library_id: Uuid::new_v4(),
        movie_id,
        edition: edition.map(str::to_string),
        created_at: chrono::Utc::now(),
    };
    let id = entry.id;
    movie_repo.entries.lock().unwrap().insert(entry.id, entry);
    id
}

/// A file of `content` of `size_bytes` lasting `duration_secs`.
fn file_of(
    file_repo: &InMemoryFileRepository,
    content: MediaFileContent,
    size_bytes: u64,
    duration_secs: u64,
) -> Uuid {
    let mut file = make_media_file(Uuid::new_v4(), content);
    file.size_bytes = size_bytes;
    file.duration = Some(Duration::from_secs(duration_secs));
    let id = file.id;
    file_repo.files.lock().unwrap().insert(file.id, file);
    id
}

/// The primary is chosen from what the files are, not the order they
/// were found: the default edition over a named one however good, then
/// the taller picture over a larger file. The detail route's `file_id`
/// and duration are the primary's, and `/sources` lists it first.
#[tokio::test]
async fn the_tallest_default_edition_file_is_primary_and_listed_first() {
    use crate::models::MediaMetadata;
    use beam_domain::repositories::MediaStreamRepository;

    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
    let movie = make_movie("Heat", Some(1995));
    let movie_id = movie.id;
    movie_repo.movies.lock().unwrap().insert(movie.id, movie);

    let theatrical = entry(&movie_repo, movie_id, None);
    let directors_cut = entry(&movie_repo, movie_id, Some("Director's Cut"));
    let content = MediaFileContent::movie;
    // The 720p file is the largest; the Director's Cut is the tallest
    // and the highest bit rate.
    let hd = file_of(&file_repo, content(theatrical), 9_000, 100);
    let uhd = file_of(&file_repo, content(theatrical), 5_000, 200);
    let cut = file_of(&file_repo, content(directors_cut), 9_999, 300);
    stream_repo
        .insert_streams(vec![
            video_stream(hd, 0, "h264", 720, Some(4_000_000), Some(23.976)),
            video_stream(uhd, 0, "hevc", 2160, Some(20_000_000), Some(23.976)),
            video_stream(cut, 0, "hevc", 2160, Some(60_000_000), Some(23.976)),
        ])
        .await
        .unwrap();

    let service = service(
        movie_repo,
        Arc::new(InMemoryShowRepository::default()),
        file_repo,
        stream_repo,
    );

    let sources = service.get_media_sources(movie_id).await.unwrap();
    let order: Vec<(Uuid, bool, Option<&str>)> = sources
        .iter()
        .map(|s| (s.file_id, s.is_primary, s.edition.as_deref()))
        .collect();
    assert_eq!(
        order,
        vec![
            (uhd, true, None),
            (hd, false, None),
            (cut, false, Some("Director's Cut")),
        ]
    );

    let Some(MediaMetadata::Movie(detail)) = service.get_media_metadata(movie_id).await.unwrap()
    else {
        panic!("the movie resolves");
    };
    assert_eq!(detail.file_id, Some(uhd));
    assert_eq!(detail.duration_secs, Some(200.0));
    assert_eq!(detail.source_count, Some(3));
}

/// The parts of a multi-part movie (issue #233) are one source that plays
/// them in part order, whatever order they were found in: ranked as one
/// file by its first part's picture, sized and timed as all of them, and
/// each part with its own URLs and subtitle files. A part in another
/// folder is another copy's, so it is not stacked with them.
#[tokio::test]
async fn the_parts_of_a_movie_are_one_source_played_in_order() {
    use crate::models::MediaMetadata;
    use beam_domain::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
    use beam_domain::repositories::{MediaStreamRepository, SidecarSubtitleRepository};

    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
    let sidecar_repo = Arc::new(InMemorySidecarSubtitleRepository::default());
    let movie = make_movie("Movie", Some(2019));
    let movie_id = movie.id;
    movie_repo.movies.lock().unwrap().insert(movie.id, movie);
    let theatrical = entry(&movie_repo, movie_id, None);
    let file = |path: &str, part_number: Option<u32>, size_bytes: u64, secs: u64| {
        let id = file_of(
            &file_repo,
            MediaFileContent::Movie {
                movie_entry_id: theatrical,
                part_number,
            },
            size_bytes,
            secs,
        );
        file_repo.files.lock().unwrap().get_mut(&id).unwrap().path = path.into();
        id
    };
    // Found part 2 first.
    let cd2 = file("/m/Movie (2019)/Movie (2019) - CD2.avi", Some(2), 600, 3000);
    let cd1 = file("/m/Movie (2019)/Movie (2019) - CD1.avi", Some(1), 700, 2800);
    let whole = file("/m/Movie (2019)/Movie (2019).mkv", None, 9_000, 5800);
    let elsewhere = file("/m/Other/Movie (2019) - CD1.avi", Some(1), 1, 2800);
    stream_repo
        .insert_streams(vec![
            video_stream(cd1, 0, "hevc", 2160, None, None),
            video_stream(cd2, 0, "hevc", 2160, None, None),
            video_stream(whole, 0, "h264", 1080, None, None),
        ])
        .await
        .unwrap();
    let sidecar = sidecar_repo
        .upsert_by_path(UpsertSidecarSubtitle {
            file_id: cd2,
            library_id: Uuid::new_v4(),
            path: "/m/Movie (2019)/Movie (2019) - CD2.srt".into(),
            info: SidecarInfo {
                format: SubtitleFormat::Srt,
                language: None,
                title: None,
                is_forced: false,
                is_sdh: false,
                is_default: false,
            },
            size_bytes: 10,
            mtime: None,
        })
        .await
        .unwrap();

    let service = service_with_sidecars(
        movie_repo,
        Arc::new(InMemoryShowRepository::default()),
        file_repo,
        stream_repo,
        sidecar_repo,
        Arc::default(),
    );
    let sources = service.get_media_sources(movie_id).await.unwrap();

    /// A source's first file, whether it is primary, and its parts.
    type Shape = (Uuid, bool, Vec<(Uuid, Option<u32>)>);
    let shape: Vec<Shape> = sources
        .iter()
        .map(|s| {
            (
                s.file_id,
                s.is_primary,
                s.parts.iter().map(|p| (p.file_id, p.part_number)).collect(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (cd1, true, vec![(cd1, Some(1)), (cd2, Some(2))]),
            (whole, false, vec![(whole, None)]),
            (elsewhere, false, vec![(elsewhere, Some(1))]),
        ]
    );
    let stack = &sources[0];
    assert_eq!(
        (stack.size_bytes, stack.duration_secs),
        (1300, Some(5800.0)),
        "all the parts, together"
    );
    // The stack plays from its first part; each part plays its own file.
    assert_eq!(
        (&stack.stream_url, &stack.download_url),
        (&stack.parts[0].stream_url, &stack.parts[0].download_url)
    );
    for part in &stack.parts {
        assert_urls_name(&part.stream_url, &part.download_url, part.file_id);
    }
    assert_eq!(
        stack
            .parts
            .iter()
            .map(|p| (p.file_id, p.duration_secs))
            .collect::<Vec<_>>(),
        vec![(cd1, Some(2800.0)), (cd2, Some(3000.0))]
    );
    assert!(stack.parts[0].subtitle_tracks.is_empty());
    assert_eq!(
        stack.parts[1]
            .subtitle_tracks
            .iter()
            .map(|t| t.sidecar_id)
            .collect::<Vec<_>>(),
        vec![Some(sidecar.id)],
        "a part's subtitle file is that part's"
    );

    let Some(MediaMetadata::Movie(detail)) = service.get_media_metadata(movie_id).await.unwrap()
    else {
        panic!("the movie resolves");
    };
    assert_eq!(
        (detail.file_id, detail.duration_secs, detail.source_count),
        (Some(cd1), Some(5800.0), Some(3))
    );
}

/// A DVD or Blu-ray folder rip is one source of its title (issue #234):
/// the main title's stream files are its parts, in order, and the source
/// names the disc it was copied from. A file that is no disc's names
/// none.
#[tokio::test]
async fn a_disc_rips_main_title_is_one_source_naming_its_disc() {
    use crate::models::DiscStructure;

    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let movie = make_movie("Heat", Some(1995));
    let movie_id = movie.id;
    movie_repo.movies.lock().unwrap().insert(movie.id, movie);
    let theatrical = entry(&movie_repo, movie_id, None);
    let file = |path: &str, part_number: Option<u32>, size_bytes: u64| {
        let id = file_of(
            &file_repo,
            MediaFileContent::Movie {
                movie_entry_id: theatrical,
                part_number,
            },
            size_bytes,
            3000,
        );
        file_repo.files.lock().unwrap().get_mut(&id).unwrap().path = path.into();
        id
    };
    let vob2 = file("/m/Heat (1995)/VIDEO_TS/VTS_02_2.VOB", Some(2), 9_000);
    let vob1 = file("/m/Heat (1995)/VIDEO_TS/VTS_02_1.VOB", Some(1), 9_000);
    let clip = file("/m/Heat (1995) BD/BDMV/STREAM/00800.m2ts", None, 30_000);
    let mkv = file("/m/Heat (1995)/Heat (1995).mkv", None, 1_000);

    let service = service_with_sidecars(
        movie_repo,
        Arc::new(InMemoryShowRepository::default()),
        file_repo,
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(InMemorySidecarSubtitleRepository::default()),
        Arc::default(),
    );
    let mut sources: Vec<(Uuid, Option<DiscStructure>, Vec<Uuid>)> = service
        .get_media_sources(movie_id)
        .await
        .unwrap()
        .into_iter()
        .map(|s| {
            (
                s.file_id,
                s.disc_structure,
                s.parts.iter().map(|p| p.file_id).collect(),
            )
        })
        .collect();
    sources.sort_by_key(|(file_id, _, _)| *file_id);
    let mut expected = vec![
        (vob1, Some(DiscStructure::Dvd), vec![vob1, vob2]),
        (clip, Some(DiscStructure::BluRay), vec![clip]),
        (mkv, None, vec![mkv]),
    ];
    expected.sort_by_key(|(file_id, _, _)| *file_id);
    assert_eq!(sources, expected);
}

/// The discs of a set of one film are one source (decision D234-7),
/// playing disc 1's main title and then disc 2's as the indexer numbers
/// them; a set missing a disc is a source per disc.
#[tokio::test]
async fn a_multi_disc_set_is_one_source_in_disc_order() {
    use crate::models::DiscStructure;

    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let movie = make_movie("Heat", Some(1995));
    let movie_id = movie.id;
    movie_repo.movies.lock().unwrap().insert(movie.id, movie);
    let theatrical = entry(&movie_repo, movie_id, None);
    let file = |path: &str, part_number: Option<u32>| {
        let id = file_of(
            &file_repo,
            MediaFileContent::Movie {
                movie_entry_id: theatrical,
                part_number,
            },
            9_000,
            3000,
        );
        file_repo.files.lock().unwrap().get_mut(&id).unwrap().path = path.into();
        id
    };
    // Disc 2 is the larger, so were the discs two sources it could rank
    // first and the play button would start the film's second half.
    let second = file("/m/Heat (1995)/Disc 2/VIDEO_TS/VTS_01_1.VOB", Some(3));
    let first_b = file("/m/Heat (1995)/Disc 1/VIDEO_TS/VTS_01_2.VOB", Some(2));
    let first_a = file("/m/Heat (1995)/Disc 1/VIDEO_TS/VTS_01_1.VOB", Some(1));
    // Another release, missing its disc 2: each disc its own.
    let lone_1 = file("/m/Heat.1995.PAL - CD1/VIDEO_TS/VTS_01_1.VOB", None);
    let lone_3 = file("/m/Heat.1995.PAL - CD3/VIDEO_TS/VTS_01_1.VOB", None);

    let service = service_with_sidecars(
        movie_repo,
        Arc::new(InMemoryShowRepository::default()),
        file_repo,
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(InMemorySidecarSubtitleRepository::default()),
        Arc::default(),
    );
    let sources = service.get_media_sources(movie_id).await.unwrap();
    let mut found: Vec<(Option<DiscStructure>, Vec<Uuid>)> = sources
        .iter()
        .map(|s| {
            (
                s.disc_structure,
                s.parts.iter().map(|p| p.file_id).collect(),
            )
        })
        .collect();
    found.sort_by(|a, b| a.1.cmp(&b.1));
    let mut expected = vec![
        (Some(DiscStructure::Dvd), vec![first_a, first_b, second]),
        (Some(DiscStructure::Dvd), vec![lone_1]),
        (Some(DiscStructure::Dvd), vec![lone_3]),
    ];
    expected.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(found, expected);
    assert_eq!(
        sources[0]
            .parts
            .iter()
            .map(|p| p.file_id)
            .collect::<Vec<_>>(),
        [first_a, first_b, second],
        "the whole set ranks first and starts at disc 1"
    );
}

/// A set whose disc 1 is a DVD and disc 2 a Blu-ray is still one source
/// (decision D234-7), but no one disc kind describes it: it names none
/// rather than the lead part's `dvd` while carrying a BDAV clip
/// (decision D234-12).
#[tokio::test]
async fn a_set_of_a_dvd_and_a_blu_ray_names_no_one_disc_kind() {
    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let movie = make_movie("Heat", Some(1995));
    let movie_id = movie.id;
    movie_repo.movies.lock().unwrap().insert(movie.id, movie);
    let theatrical = entry(&movie_repo, movie_id, None);
    let file = |path: &str, part_number: Option<u32>| {
        let id = file_of(
            &file_repo,
            MediaFileContent::Movie {
                movie_entry_id: theatrical,
                part_number,
            },
            9_000,
            3000,
        );
        file_repo.files.lock().unwrap().get_mut(&id).unwrap().path = path.into();
        id
    };
    let vob_a = file("/m/Heat (1995)/Disc 1/VIDEO_TS/VTS_01_1.VOB", Some(1));
    let vob_b = file("/m/Heat (1995)/Disc 1/VIDEO_TS/VTS_01_2.VOB", Some(2));
    let clip = file("/m/Heat (1995)/Disc 2/BDMV/STREAM/00001.m2ts", Some(3));

    let service = service_with_sidecars(
        movie_repo,
        Arc::new(InMemoryShowRepository::default()),
        file_repo,
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(InMemorySidecarSubtitleRepository::default()),
        Arc::default(),
    );
    let sources = service.get_media_sources(movie_id).await.unwrap();
    let found: Vec<_> = sources
        .iter()
        .map(|s| {
            (
                s.disc_structure,
                s.parts.iter().map(|p| p.file_id).collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(found, [(None, vec![vob_a, vob_b, clip])]);
}

/// A folder's parts are one source only as the whole run 1..n (C2 of
/// the #233 review): parts 2 and 3 with no part 1, or 1 and 3 with no
/// part 2, are missing a part -- or are two films whose names end alike
/// -- so each file is a source of its own, never one that starts past
/// the beginning or skips a part.
#[tokio::test]
async fn parts_that_do_not_run_from_one_are_not_one_source() {
    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let movie = make_movie("Movie", Some(2019));
    let movie_id = movie.id;
    movie_repo.movies.lock().unwrap().insert(movie.id, movie);
    let theatrical = entry(&movie_repo, movie_id, None);
    let file = |path: &str, part_number: u32, size_bytes: u64| {
        let id = file_of(
            &file_repo,
            MediaFileContent::Movie {
                movie_entry_id: theatrical,
                part_number: Some(part_number),
            },
            size_bytes,
            1000,
        );
        file_repo.files.lock().unwrap().get_mut(&id).unwrap().path = path.into();
        id
    };
    // Distinct sizes, so the rank order is known: the largest first.
    let late_two = file("/m/Late/Movie (2019) - CD2.avi", 2, 400);
    let late_three = file("/m/Late/Movie (2019) - CD3.avi", 3, 300);
    let gap_one = file("/m/Gap/Movie (2019) - CD1.avi", 1, 200);
    let gap_three = file("/m/Gap/Movie (2019) - CD3.avi", 3, 100);

    let service = service(
        movie_repo,
        Arc::new(InMemoryShowRepository::default()),
        file_repo,
        Arc::new(InMemoryMediaStreamRepository::default()),
    );
    let sources = service.get_media_sources(movie_id).await.unwrap();

    /// A source's first file and its parts.
    type Shape = (Uuid, Vec<(Uuid, Option<u32>)>);
    let shape: Vec<Shape> = sources
        .iter()
        .map(|s| {
            (
                s.file_id,
                s.parts.iter().map(|p| (p.file_id, p.part_number)).collect(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (late_two, vec![(late_two, Some(2))]),
            (late_three, vec![(late_three, Some(3))]),
            (gap_one, vec![(gap_one, Some(1))]),
            (gap_three, vec![(gap_three, Some(3))]),
        ]
    );
}

/// Every track is addressed by its stream index and names its codec as
/// FFmpeg does, whatever the codec: E-AC-3 and TrueHD are no longer
/// `Unknown`, a PGS subtitle is not `WebVTT`. A value the file does not
/// state is absent, never a made-up default.
#[tokio::test]
async fn tracks_carry_their_index_and_real_codec() {
    use crate::models::SubtitleOrigin;
    use beam_domain::repositories::MediaStreamRepository;

    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
    let movie = make_movie("Dune", Some(2021));
    let movie_id = movie.id;
    movie_repo.movies.lock().unwrap().insert(movie.id, movie);
    let file = file_of(
        &file_repo,
        MediaFileContent::movie(entry(&movie_repo, movie_id, None)),
        1,
        1,
    );
    stream_repo
        .insert_streams(vec![
            subtitle_stream(file, 4, "subrip", true),
            audio_stream(file, 2, "truehd", 48_000, true),
            video_stream(file, 0, "hevc", 2160, Some(0), None),
            audio_stream(file, 1, "eac3", 0, false),
            subtitle_stream(file, 3, "hdmv_pgs_subtitle", false),
        ])
        .await
        .unwrap();

    let service = service(
        movie_repo,
        Arc::new(InMemoryShowRepository::default()),
        file_repo,
        stream_repo,
    );
    let sources = service.get_media_sources(movie_id).await.unwrap();
    let source = &sources[0];

    let video = &source.video_tracks[0];
    assert_eq!((video.index, video.codec.as_str()), (0, "hevc"));
    assert_eq!(video.frame_rate, None, "no 29.97 is invented");
    assert_eq!(video.bit_rate, None, "a zero bit rate is an unknown one");

    let audio: Vec<(u32, &str, Option<u32>, bool)> = source
        .audio_tracks
        .iter()
        .map(|a| (a.index, a.codec.as_str(), a.sample_rate, a.is_default))
        .collect();
    assert_eq!(
        audio,
        vec![(1, "eac3", None, false), (2, "truehd", Some(48_000), true)]
    );

    let subtitles: Vec<_> = source
        .subtitle_tracks
        .iter()
        .map(|s| {
            (
                s.origin,
                s.index,
                s.codec.as_str(),
                s.is_text,
                s.is_hearing_impaired,
                s.url.is_some(),
            )
        })
        .collect();
    assert_eq!(
        subtitles,
        vec![
            (
                SubtitleOrigin::Embedded,
                Some(3),
                "hdmv_pgs_subtitle",
                false,
                false,
                false
            ),
            (
                SubtitleOrigin::Embedded,
                Some(4),
                "subrip",
                true,
                true,
                false
            ),
        ],
        "an embedded track is never served on its own"
    );
}

/// The subtitle files beside a video are its tracks too: after the
/// embedded ones, each with where to fetch it, and a WebVTT rendition
/// exactly for a SubRip or WebVTT file small enough to convert. Another
/// file's subtitles are not this one's.
#[tokio::test]
async fn sidecar_subtitles_follow_the_embedded_tracks_with_their_urls() {
    use crate::models::SubtitleOrigin;
    use crate::services::sources::SUBTITLE_CONVERT_MAX_BYTES;
    use beam_domain::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
    use beam_domain::repositories::{MediaStreamRepository, SidecarSubtitleRepository};

    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let stream_repo = Arc::new(InMemoryMediaStreamRepository::default());
    let sidecar_repo = Arc::new(InMemorySidecarSubtitleRepository::default());
    let movie = make_movie("Alien", Some(1979));
    let movie_id = movie.id;
    movie_repo.movies.lock().unwrap().insert(movie.id, movie);
    let content = MediaFileContent::movie(entry(&movie_repo, movie_id, None));
    // The other file is the smaller, so this one is primary and first.
    let file = file_of(&file_repo, content.clone(), 2, 1);
    let other = file_of(&file_repo, content, 1, 1);
    stream_repo
        .insert_streams(vec![subtitle_stream(file, 2, "subrip", false)])
        .await
        .unwrap();

    let sidecar =
        |file_id, name: &str, format, language: Option<&str>, forced, size| UpsertSidecarSubtitle {
            file_id,
            library_id: Uuid::new_v4(),
            path: std::path::PathBuf::from(format!("/videos/{name}")),
            info: SidecarInfo {
                format,
                language: language.map(str::to_string),
                title: None,
                is_forced: forced,
                is_sdh: name.contains("sdh"),
                is_default: false,
            },
            size_bytes: size,
            mtime: None,
        };
    let mut ids = std::collections::HashMap::new();
    for upsert in [
        sidecar(file, "Alien.srt", SubtitleFormat::Srt, None, false, 10),
        sidecar(
            file,
            "Alien.fre.ass",
            SubtitleFormat::Ass,
            Some("fre"),
            false,
            10,
        ),
        sidecar(
            file,
            "Alien.eng.forced.srt",
            SubtitleFormat::Srt,
            Some("eng"),
            true,
            10,
        ),
        sidecar(
            file,
            "Alien.eng.sdh.vtt",
            SubtitleFormat::Vtt,
            Some("eng"),
            false,
            10,
        ),
        sidecar(
            file,
            "Alien.spa.srt",
            SubtitleFormat::Srt,
            Some("spa"),
            false,
            SUBTITLE_CONVERT_MAX_BYTES + 1,
        ),
        sidecar(
            other,
            "Other.eng.srt",
            SubtitleFormat::Srt,
            Some("eng"),
            false,
            10,
        ),
    ] {
        let name = upsert
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let row = sidecar_repo.upsert_by_path(upsert).await.unwrap();
        ids.insert(name, row.id);
    }

    let service = service_with_sidecars(
        movie_repo,
        Arc::new(InMemoryShowRepository::default()),
        file_repo,
        stream_repo,
        sidecar_repo,
        Arc::default(),
    );
    let sources = service.get_media_sources(movie_id).await.unwrap();
    assert_eq!(sources[0].file_id, file);
    let tracks = &sources[0].subtitle_tracks;

    assert_eq!(tracks[0].origin, SubtitleOrigin::Embedded);
    let sidecars: Vec<(&str, &str, bool, bool, bool)> = tracks[1..]
        .iter()
        .map(|track| {
            let id = track.sidecar_id.expect("a sidecar has an id");
            let name = ids
                .iter()
                .find_map(|(name, row)| (*row == id).then_some(name.as_str()))
                .expect("a sidecar of this file");
            assert_eq!(track.origin, SubtitleOrigin::Sidecar);
            assert_eq!(track.index, None);
            assert!(track.is_text);
            assert_eq!(
                track.url.as_deref(),
                Some(format!("/v1/files/{file}/subtitles/{id}").as_str())
            );
            if let Some(webvtt) = &track.webvtt_url {
                assert_eq!(webvtt, &format!("/v1/files/{file}/subtitles/{id}/webvtt"));
            }
            (
                name,
                track.codec.as_str(),
                track.is_forced,
                track.is_hearing_impaired,
                track.webvtt_url.is_some(),
            )
        })
        .collect();
    assert_eq!(
        sidecars,
        vec![
            ("Alien.eng.sdh.vtt", "webvtt", false, true, true),
            ("Alien.eng.forced.srt", "subrip", true, false, true),
            ("Alien.fre.ass", "ass", false, false, false),
            ("Alien.spa.srt", "subrip", false, false, false),
            ("Alien.srt", "subrip", false, false, true),
        ],
        "by language, untagged last, full before forced; ASS and oversized files as stored only"
    );
}

/// A file holding a run of episodes says so on its source, and its
/// duration -- the whole run's -- is not given as the episode's.
#[tokio::test]
async fn an_episode_file_holding_a_run_says_so_and_lends_no_duration() {
    use crate::models::MediaMetadata;

    let show_repo = Arc::new(InMemoryShowRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let episode_id = seed_episode(&show_repo);
    let show_id = *show_repo.shows.lock().unwrap().keys().next().unwrap();
    file_of(
        &file_repo,
        MediaFileContent::Episode {
            episode_id,
            last_episode_number: Some(3),
        },
        1,
        3 * 45 * 60,
    );

    let service = service(
        Arc::new(InMemoryMovieRepository::default()),
        show_repo,
        file_repo,
        Arc::new(InMemoryMediaStreamRepository::default()),
    );

    let sources = service.get_media_sources(episode_id).await.unwrap();
    let span = sources[0].episode_span.expect("the file spans episodes");
    assert_eq!(
        (span.first_episode_number, span.last_episode_number),
        (1, 3)
    );
    assert_eq!(sources[0].duration_secs, Some(8100.0));

    let Some(MediaMetadata::Show(show)) = service.get_media_metadata(show_id).await.unwrap() else {
        panic!("the show resolves");
    };
    let episode = &show.seasons[0].episodes[0];
    assert_eq!(episode.file_id, Some(sources[0].file_id));
    assert_eq!(episode.duration_secs, None);
    assert_eq!(episode.source_count, 1);
}

/// A single-episode file spans nothing and lends the episode its
/// duration.
#[tokio::test]
async fn a_single_episode_file_lends_its_duration() {
    use crate::models::MediaMetadata;

    let show_repo = Arc::new(InMemoryShowRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let episode_id = seed_episode(&show_repo);
    let show_id = *show_repo.shows.lock().unwrap().keys().next().unwrap();
    file_of(
        &file_repo,
        MediaFileContent::episode(episode_id),
        1,
        45 * 60,
    );

    let service = service(
        Arc::new(InMemoryMovieRepository::default()),
        show_repo,
        file_repo,
        Arc::new(InMemoryMediaStreamRepository::default()),
    );

    let sources = service.get_media_sources(episode_id).await.unwrap();
    assert!(sources[0].episode_span.is_none());
    let Some(MediaMetadata::Show(show)) = service.get_media_metadata(show_id).await.unwrap() else {
        panic!("the show resolves");
    };
    assert_eq!(show.seasons[0].episodes[0].duration_secs, Some(2700.0));
}
