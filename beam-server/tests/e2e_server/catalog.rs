//! The core loop over a real library: add it, scan it, browse and search
//! what the scan found, read a title's detail and sources, and stream its
//! bytes by range.

use beam_test_support::e2e::client::api;

use crate::common::{
    self, Admin, VIEWER, Video, assert_refused, file_rows, library_root, movie_of,
};

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
    // of its own (#311), to be fixed with this assertion added here.
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
    // A blank name's 422 is the handler's alone and is proven without a
    // stack (`admin_tests`); what only this tier shows is the root resolved
    // against a real BEAM_VIDEO_DIR.

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

/// The committed probe fixtures -- codecs no CI FFmpeg can encode, and an
/// MP4 whose index sits after its media -- index through the real server
/// with the codecs they hold.
#[tokio::test]
async fn the_committed_probe_fixtures_index_with_their_codecs() {
    let mut admin = Admin::start().await;
    let root = common::workspace_root();
    let fixtures = [
        ("hevc.mkv", "Dune (2001)", "mkv", "hevc", None),
        ("av1.mp4", "Solaris (2002)", "mp4", "av1", None),
        ("vp9_opus.webm", "Tenet (2003)", "webm", "vp9", Some("opus")),
        (
            "h264_aac_moov_at_end.mp4",
            "Memento (2004)",
            "mp4",
            "h264",
            Some("aac"),
        ),
    ];
    for (fixture, name, extension, _, _) in fixtures {
        admin.stack.library.copy_in(
            &common::probe_fixture(&root, fixture),
            format!("films/{name}/{name}.{extension}"),
        );
    }
    let library = admin.add_library("Films", "films").await;
    let progress = admin.scan_ok(library).await;
    assert_eq!(
        (progress.added_count, progress.failed_count),
        (4, 0),
        "{progress:?}"
    );

    for (fixture, name, _, video, audio) in fixtures {
        let title = name.split(" (").next().expect("a title");
        let id = admin.title(title).await.id;
        let sources = admin
            .api
            .get_media_sources(id)
            .await
            .unwrap_or_else(|e| panic!("read {title}'s sources: {e:?}"))
            .into_inner()
            .items;
        assert_eq!(sources.len(), 1, "{fixture}");
        let codecs = (
            sources[0]
                .video_tracks
                .first()
                .map(|track| track.codec.as_str()),
            sources[0]
                .audio_tracks
                .first()
                .map(|track| track.codec.as_str()),
        );
        assert_eq!(codecs, (Some(video), audio), "{fixture}");
    }

    admin.finish().await;
}

/// What a viewer may see, set by an administrator and read back through the
/// SQL access filter: restricted to one library, a viewer browses only its
/// titles and finds the other library's not found, while the administrator
/// who restricted them still sees everything.
#[tokio::test]
async fn a_restricted_viewer_sees_only_their_libraries_over_postgres() {
    let mut admin = Admin::start().await;
    admin.stack.library.add_video(
        "kids/Paddington (2014)/Paddington (2014).mp4",
        &Video::new(330),
    );
    admin
        .stack
        .library
        .add_video("grown/Heat (1995)/Heat (1995).mp4", &Video::new(550));
    let kids = admin.add_library("Kids", "kids").await;
    let grown = admin.add_library("Grown-ups", "grown").await;
    admin.scan_ok(kids).await;
    admin.scan_ok(grown).await;
    let paddington = admin.title("Paddington").await.id;
    let heat = admin.title("Heat").await.id;

    let viewer = admin.stack.client(&admin.stack.sign_in(VIEWER).await);
    let viewer_id = viewer
        .get_current_user()
        .await
        .expect("the viewer reads who they are")
        .into_inner()
        .id;
    let set = admin
        .api
        .set_admin_user_access(
            viewer_id,
            &api::types::UserAccess {
                all_libraries: false,
                library_ids: vec![kids],
                max_content_rating: None,
            },
        )
        .await
        .expect("restrict the viewer")
        .into_inner();
    assert_eq!((set.all_libraries, set.library_ids), (false, vec![kids]));

    let seen: Vec<_> = common::browse(&viewer, None)
        .await
        .into_iter()
        .map(|title| title.id)
        .collect();
    assert_eq!(seen, vec![paddington], "the viewer browses one library");
    assert_refused(
        "a title of a library the viewer may not see",
        404,
        viewer.get_media_detail(heat).await,
    );
    viewer
        .get_media_detail(paddington)
        .await
        .expect("a title of the viewer's own library");

    let everything: Vec<_> = admin.titles().await.into_iter().map(|t| t.id).collect();
    assert_eq!(
        everything.len(),
        2,
        "the administrator who restricted the viewer is not restricted"
    );

    admin.finish().await;
}

/// The watchlist, the favourites and a collection, each written and read
/// back through its SQL store. Collections arrive from a metadata provider,
/// which this tier never reaches, so the one here is written straight into
/// the database the way enrichment would write it.
#[tokio::test]
async fn lists_and_collections_round_trip_over_postgres() {
    use sea_orm::{ActiveModelTrait, Set};

    let mut admin = Admin::start().await;
    admin
        .stack
        .library
        .add_video("films/Arrival (2016)/Arrival (2016).mp4", &Video::new(440));
    admin
        .stack
        .library
        .add_video("films/Heat (1995)/Heat (1995).mp4", &Video::new(550));
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;
    let arrival = admin.title("Arrival").await.id;
    let heat = admin.title("Heat").await.id;

    let viewer = admin.stack.client(&admin.stack.sign_in(VIEWER).await);
    viewer
        .add_to_watchlist(arrival)
        .await
        .expect("put Arrival on the watchlist");
    viewer
        .add_to_favourites(heat)
        .await
        .expect("make Heat a favourite");
    let ids = |page: api::types::UserListConnection| {
        page.items
            .iter()
            .map(|item| common::title_of(&item.media).id)
            .collect::<Vec<_>>()
    };
    let watchlist = viewer
        .get_watchlist(api::GetWatchlistParams {
            first: None,
            after: None,
        })
        .await
        .expect("read the watchlist")
        .into_inner();
    assert_eq!(watchlist.total, 1);
    assert_eq!(ids(watchlist), vec![arrival]);
    let favourites = viewer
        .get_favourites(api::GetFavouritesParams {
            first: None,
            after: None,
        })
        .await
        .expect("read the favourites")
        .into_inner();
    assert_eq!(ids(favourites), vec![heat]);
    let admins_own = admin
        .api
        .get_watchlist(api::GetWatchlistParams {
            first: None,
            after: None,
        })
        .await
        .expect("read the administrator's watchlist")
        .into_inner();
    assert_eq!(admins_own.total, 0, "a list is its owner's alone");

    viewer
        .remove_from_watchlist(arrival)
        .await
        .expect("take Arrival off the watchlist");
    let emptied = viewer
        .get_watchlist(api::GetWatchlistParams {
            first: None,
            after: None,
        })
        .await
        .expect("read the watchlist")
        .into_inner();
    assert_eq!(emptied.total, 0);

    let now = chrono::Utc::now().fixed_offset();
    let collection = beam_entity::collection::ActiveModel {
        id: Set(uuid::Uuid::new_v4()),
        external_ref: Set("tmdb:1000".to_owned()),
        name: Set("A Collection".to_owned()),
        poster_url: Set(None),
        backdrop_url: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(admin.stack.db())
    .await
    .expect("store a collection");
    for movie in [arrival, heat] {
        beam_entity::collection_movie::ActiveModel {
            movie_id: Set(movie),
            collection_id: Set(collection.id),
        }
        .insert(admin.stack.db())
        .await
        .expect("put a movie in the collection");
    }

    let listed = viewer
        .list_collections(api::ListCollectionsParams {
            first: None,
            after: None,
        })
        .await
        .expect("list the collections")
        .into_inner();
    assert_eq!(listed.total, 1);
    assert_eq!(listed.items[0].id, collection.id);
    assert_eq!(listed.items[0].movie_count, 2);
    let detail = viewer
        .get_collection(collection.id)
        .await
        .expect("read the collection")
        .into_inner();
    assert_eq!(detail.name, "A Collection");
    assert_eq!(
        detail
            .movies
            .iter()
            .map(|movie| movie.id)
            .collect::<Vec<_>>(),
        vec![heat, arrival],
        "in release order"
    );

    admin.finish().await;
}
