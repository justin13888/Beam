//! Artwork (`GET /v1/artwork/{kind}/{id}/{variant}`, ADR-0015): served from
//! Beam's own cache under `BEAM_DATA_DIR`, never from beside the library's
//! files.
//!
//! Art is fetched from a metadata provider, which this tier never reaches.
//! So the poster URL is written onto the title the way enrichment writes it,
//! and the image is put in the cache as an earlier fetch would have left it
//! there; the server then finds it as it starts, the way a restart after a
//! deploy finds the cache it had.

use std::path::{Path, PathBuf};
use std::process::Command;

use beam_server::services::artwork::CacheKey;
use beam_test_support::e2e::client::api;
use sea_orm::{ActiveModelTrait, Set};
use uuid::Uuid;

use crate::common::{Admin, VIEWER, Video, header, movie_of, raw_get_with};

/// Where enrichment would have pointed Arrival's poster.
const POSTER_URL: &str = "https://image.tmdb.org/t/p/original/beam-e2e-arrival.png";

/// Render a `width` by `height` PNG of one opaque colour to `dest`.
fn render_png(dest: &Path, width: u32, height: u32) {
    let output = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("color=c=0x3366cc:size={width}x{height}"))
        .args(["-frames:v", "1", "-pix_fmt", "rgb24"])
        .arg(dest)
        .output()
        .expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg could not render {}: {}",
        dest.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Every file in the artwork cache under `data_dir`.
fn cached_files(data_dir: &Path) -> Vec<PathBuf> {
    let root = data_dir.join("artwork");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("read {}: {e}", root.display()))
        .filter_map(Result::ok)
        .filter(|shard| shard.path().is_dir())
        .flat_map(|shard| std::fs::read_dir(shard.path()).into_iter().flatten())
        .filter_map(Result::ok)
        .map(|file| file.path())
        .collect();
    files.sort();
    files
}

/// Point `movie`'s poster at `url`, as enrichment would.
async fn set_poster(admin: &Admin, movie: Uuid, url: &str) {
    beam_entity::movie::ActiveModel {
        id: Set(movie),
        poster_url: Set(Some(url.to_owned())),
        ..Default::default()
    }
    .update(admin.stack.db())
    .await
    .expect("store the poster URL");
}

/// A cached poster is served as cached, under a validator that is its cache
/// key, and revalidates to `304`; a narrower size is made once from it and
/// cached beside it under its own validator. A title with no art, and a
/// title the caller may not see, are both `404`; nothing is served without
/// a session.
#[tokio::test]
async fn artwork_is_served_from_the_cache_under_the_data_directory() {
    let mut admin = Admin::start().await;
    admin
        .stack
        .library
        .add_video("films/Arrival (2016)/Arrival (2016).mp4", &Video::new(440));
    admin
        .stack
        .library
        .add_video("films/Heat (1995)/Heat (1995).mp4", &Video::new(550));
    admin.stack.library.add_video(
        "kids/Paddington (2014)/Paddington (2014).mp4",
        &Video::new(330),
    );
    let films = admin.add_library("Films", "films").await;
    let kids = admin.add_library("Kids", "kids").await;
    admin.scan_ok(films).await;
    admin.scan_ok(kids).await;
    let arrival = admin.title("Arrival").await.id;
    let heat = admin.title("Heat").await.id;

    set_poster(&admin, arrival, POSTER_URL).await;
    let key = CacheKey::for_url(POSTER_URL);
    let data_dir = admin.stack.data_dir();
    let shard = data_dir.join("artwork").join(&key.as_str()[..2]);
    std::fs::create_dir_all(&shard).expect("create the cache shard");
    let cached = shard.join(format!("{}.png", key.as_str()));
    render_png(&cached, 600, 900);
    let original = std::fs::read(&cached).expect("read the cached poster");
    admin.stack.restart().await;

    let movie = movie_of(
        admin
            .api
            .get_media_detail(arrival)
            .await
            .expect("read Arrival")
            .into_inner(),
    );
    let poster = movie
        .poster_url
        .expect("a title with a poster names where Beam serves it");
    assert!(
        poster.starts_with("/v1/artwork/"),
        "served by Beam, never the provider's URL: {poster}"
    );

    let base = admin.stack.base_url().to_owned();
    let session = admin.session.clone();
    let served = raw_get_with(&base, &poster, Some(&session), &[]).await;
    assert_eq!(served.status().as_u16(), 200);
    assert_eq!(header(&served, "content-type"), Some("image/png"));
    let etag = header(&served, "etag").expect("a validator").to_owned();
    assert_eq!(etag, format!("\"{}\"", key.as_str()), "the cache key");
    assert!(
        header(&served, "cache-control").is_some_and(|c| c.starts_with("private")),
        "who may see it depends on who asks"
    );
    assert_eq!(
        served.bytes().await.expect("the poster").as_ref(),
        original.as_slice(),
        "the cached bytes, as cached"
    );
    let revalidated =
        raw_get_with(&base, &poster, Some(&session), &[("if-none-match", &etag)]).await;
    assert_eq!(revalidated.status().as_u16(), 304);

    // A narrower size: made from the cached original, cached beside it.
    let sized = raw_get_with(&base, &format!("{poster}?size=w185"), Some(&session), &[]).await;
    assert_eq!(sized.status().as_u16(), 200);
    assert_eq!(
        header(&sized, "content-type"),
        Some("image/jpeg"),
        "an opaque rendition is a JPEG"
    );
    let sized_etag = header(&sized, "etag").expect("a validator").to_owned();
    assert_ne!(sized_etag, etag, "each size is its own resource");
    let sized_bytes = sized.bytes().await.expect("the rendition");
    assert!(sized_bytes.starts_with(&[0xFF, 0xD8, 0xFF]));
    let in_cache = cached_files(&data_dir);
    assert_eq!(
        in_cache.len(),
        2,
        "the original and its rendition: {in_cache:?}"
    );
    let rendition = in_cache
        .iter()
        .find(|path| path.as_path() != cached.as_path())
        .expect("the rendition is cached");
    assert_eq!(
        std::fs::read(rendition).expect("read the rendition"),
        sized_bytes.as_ref(),
        "the rendition served is the one cached"
    );
    assert_eq!(
        std::fs::read(&cached).expect("read the original"),
        original,
        "the original is left as it was"
    );

    // No art, and art nobody may see without a session.
    let heat_poster = format!("/v1/artwork/movie/{heat}/poster");
    assert_eq!(
        raw_get_with(&base, &heat_poster, Some(&session), &[])
            .await
            .status()
            .as_u16(),
        404,
        "a title with no art"
    );
    assert_eq!(
        raw_get_with(&base, &poster, None, &[])
            .await
            .status()
            .as_u16(),
        401
    );

    // A viewer restricted to the children's library: Arrival's poster is the
    // poster of a title that does not exist.
    let viewer_session = admin.stack.sign_in(VIEWER).await;
    let viewer = admin.stack.client(&viewer_session);
    let viewer_id = viewer
        .get_current_user()
        .await
        .expect("the viewer reads who they are")
        .into_inner()
        .id;
    admin
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
        .expect("restrict the viewer");
    assert_eq!(
        raw_get_with(&base, &poster, Some(&viewer_session), &[])
            .await
            .status()
            .as_u16(),
        404,
        "the art of a title the viewer may not see"
    );

    admin.finish().await;
}
