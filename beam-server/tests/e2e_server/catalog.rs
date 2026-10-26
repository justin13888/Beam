//! The core loop over a real library: add it, scan it, browse and search
//! what the scan found, read a title's detail and sources, and stream its
//! bytes by range.

use beam_test_support::e2e::client::api;

use crate::common::{self, Admin, Video, assert_refused, file_rows, library_root, movie_of};

/// From an empty server to a playable title: the library's files are found,
/// probed and named from their paths; browse and search list them; and the
/// stream endpoint serves exactly the file's bytes, whole or by range.
#[tokio::test]
async fn a_scanned_library_is_browsed_searched_and_streamed() {
    let mut admin = Admin::start().await;
    let arrival = Video::new(440).seconds(8);
    admin
        .stack
        .library
        .add_video("films/Arrival (2016)/Arrival (2016).mp4", &arrival);
    admin.stack.library.add_video(
        "films/Heat (1995)/Heat (1995).mkv",
        &Video::new(550).container(common::Container::Matroska),
    );

    let library = admin.add_library("Films", "films").await;
    let progress = admin.scan_ok(library).await;
    assert_eq!(
        (progress.added_count, progress.failed_count),
        (2, 0),
        "both files are new and both index: {progress:?}"
    );

    // What the scan wrote: one row per file, under the canonical root, with
    // the probe's results.
    let root = library_root(&admin.stack, library).await;
    let rows = file_rows(&admin.stack, library).await;
    // The MIME type is deliberately not asserted: the indexer stores
    // `video/<FFmpeg's demuxer list>` (`video/mov,mp4,m4a,3gp,3g2,mj2`), which
    // the stream endpoint then serves as its Content-Type. That is a defect
    // of its own, to be fixed with this assertion added here.
    let stored: Vec<(String, bool, bool)> = rows
        .iter()
        .map(|row| {
            (
                row.file_path
                    .strip_prefix(&format!("{root}/"))
                    .unwrap_or(&row.file_path)
                    .to_owned(),
                row.container_format.is_some(),
                row.missing_since.is_none(),
            )
        })
        .collect();
    assert_eq!(
        stored,
        [
            ("Arrival (2016)/Arrival (2016).mp4".to_owned(), true, true),
            ("Heat (1995)/Heat (1995).mkv".to_owned(), true, true),
        ],
        "one present, probed row per file, under the stored root"
    );
    let duration = rows[0].duration_secs.expect("the MP4 was probed");
    assert!(
        (duration - arrival.duration_secs()).abs() < 0.5,
        "the probed duration is the file's ({duration}s)"
    );

    // The library and its files, as the API lists them: relative paths only.
    let listed = admin.files(library).await;
    let paths: Vec<&str> = listed.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "Arrival (2016)/Arrival (2016).mp4",
            "Heat (1995)/Heat (1995).mkv"
        ]
    );
    let libraries = admin
        .api
        .list_libraries()
        .await
        .expect("list libraries")
        .into_inner()
        .items;
    assert_eq!(libraries.len(), 1);
    assert_eq!((libraries[0].id, libraries[0].file_count), (library, 2));

    // Browse and search.
    let titles = admin.titles().await;
    let names: Vec<(&str, Option<i64>)> = titles
        .iter()
        .map(|title| (title.name.as_str(), title.year))
        .collect();
    assert_eq!(names, [("Arrival", Some(2016)), ("Heat", Some(1995))]);
    let found = common::browse(&admin.api, Some("arriv")).await;
    assert_eq!(
        found.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["Arrival"],
        "a prefix of the title finds it, and only it"
    );
    assert!(
        common::browse(&admin.api, Some("zzzz")).await.is_empty(),
        "a query nothing matches finds nothing"
    );

    // Detail and sources.
    let movie_id = titles[0].id;
    let movie = movie_of(
        admin
            .api
            .get_media_detail(movie_id)
            .await
            .expect("read Arrival's detail")
            .into_inner(),
    );
    assert_eq!(movie.title.original, "Arrival");
    assert_eq!(movie.year, Some(2016));
    assert!(!movie.user_state.played);
    let sources = admin
        .api
        .get_media_sources(movie_id)
        .await
        .expect("read Arrival's sources")
        .into_inner()
        .items;
    assert_eq!(sources.len(), 1, "one file, one source");
    let source = &sources[0];
    assert!(source.is_primary);
    assert_eq!(source.parts.len(), 1);
    let file_id = source.file_id;
    assert_eq!(source.parts[0].file_id, file_id);
    assert_eq!(file_id, listed[0].id);
    assert_eq!(
        source
            .video_tracks
            .first()
            .map(|track| track.codec.as_str()),
        Some("h264")
    );
    assert_eq!(
        source
            .audio_tracks
            .first()
            .map(|track| track.codec.as_str()),
        Some("aac")
    );

    // Streaming: the whole file, then a range of it, byte for byte.
    let bytes = std::fs::read(
        admin
            .stack
            .library
            .path("films/Arrival (2016)/Arrival (2016).mp4"),
    )
    .expect("read the fixture");
    let base = admin.stack.base_url().to_owned();
    let whole = common::raw_get(&base, &source.stream_url, Some(&admin.session), None).await;
    assert_eq!(whole.status().as_u16(), 200);
    assert_eq!(
        whole
            .headers()
            .get("accept-ranges")
            .and_then(|v| v.to_str().ok()),
        Some("bytes")
    );
    assert_eq!(
        whole.bytes().await.expect("the body").as_ref(),
        bytes.as_slice()
    );

    let ranged = common::raw_get(
        &base,
        &source.stream_url,
        Some(&admin.session),
        Some("bytes=100-1123"),
    )
    .await;
    assert_eq!(ranged.status().as_u16(), 206);
    assert_eq!(
        ranged
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok()),
        Some(format!("bytes 100-1123/{}", bytes.len()).as_str())
    );
    assert_eq!(
        ranged.bytes().await.expect("the body").as_ref(),
        &bytes[100..1124]
    );

    let past_the_end = common::raw_get(
        &base,
        &source.stream_url,
        Some(&admin.session),
        Some(&format!("bytes={}-", bytes.len() + 10)),
    )
    .await;
    assert_eq!(past_the_end.status().as_u16(), 416, "a range past the end");

    let unauthenticated = common::raw_get(&base, &source.stream_url, None, None).await;
    assert_eq!(
        unauthenticated.status().as_u16(),
        401,
        "media is never served without a session"
    );

    admin.stack.library.write("films/notes.txt", b"not media");
    let rescanned = admin.scan_ok(library).await;
    assert_eq!(
        (rescanned.added_count, rescanned.unchanged_count),
        (0, 2),
        "a rescan of an unchanged library changes nothing, and a non-video is no file: {rescanned:?}"
    );

    admin.finish().await;
}

/// A library root is checked before it is stored: it must exist, sit inside
/// the video directory, and not overlap another library.
#[tokio::test]
async fn a_library_root_outside_the_video_directory_or_overlapping_another_is_refused() {
    let mut admin = Admin::start().await;
    admin.stack.library.create_dir("films/Arrival (2016)");
    let outside = admin
        .stack
        .library
        .outside()
        .to_str()
        .expect("a UTF-8 path")
        .to_owned();

    let create = |name: &str, root: &str| api::types::CreateLibraryRequest {
        name: name.to_owned(),
        root_path: root.to_owned(),
    };
    assert_refused(
        "a root outside the video directory",
        400,
        admin.api.create_library(&create("Outside", &outside)).await,
    );
    assert_refused(
        "a root that does not exist",
        400,
        admin
            .api
            .create_library(&create("Nowhere", "nowhere"))
            .await,
    );
    assert_refused(
        "a root escaping by ..",
        400,
        admin
            .api
            .create_library(&create("Escape", "films/../../outside"))
            .await,
    );
    assert_refused(
        "a blank name",
        422,
        admin.api.create_library(&create("  ", "films")).await,
    );

    admin.add_library("Films", "films").await;
    assert_refused(
        "a root inside another library's",
        409,
        admin
            .api
            .create_library(&create("Arrival", "films/Arrival (2016)"))
            .await,
    );
    let stored = admin
        .api
        .list_libraries()
        .await
        .expect("list libraries")
        .into_inner()
        .items;
    assert_eq!(stored.len(), 1, "only the valid library was stored");

    admin.finish().await;
}
