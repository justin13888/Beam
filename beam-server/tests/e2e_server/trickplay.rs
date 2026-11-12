//! Trickplay (#265): scrub-preview sheets the background worker makes from
//! real media, written under `BEAM_DATA_DIR` and served by Beam.

use std::time::Duration;

use beam_test_support::e2e::wait;

use crate::common::{Admin, Video, first_source, header, raw_get_with};

/// How long the worker may take to make one short file's sheets.
const SHEETS_BOUND: Duration = Duration::from_secs(120);

/// The worker makes a file's sheets once it has been indexed -- here on the
/// pass it runs as the process starts -- with a tile for every interval the
/// file starts; the source then names them, each is a JPEG served with a
/// validator, and every byte of them is under the data directory.
#[tokio::test]
async fn the_worker_makes_trickplay_sheets_from_real_media_and_they_are_served() {
    let mut admin = Admin::start().await;
    let arrival = Video::new(440).seconds(25).size(320, 180);
    admin
        .stack
        .library
        .add_video("films/Arrival (2016)/Arrival (2016).mp4", &arrival);
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;
    let title = admin.title("Arrival").await.id;
    assert!(
        first_source(&admin.api, title).await.trickplay.is_none(),
        "no sheets before the worker has passed over the file"
    );

    // The worker's first pass, as a restarted server begins it.
    admin.stack.restart().await;
    let source = wait::until("the file's trickplay sheets", SHEETS_BOUND, || {
        let request = admin.api.get_media_sources(title);
        async move {
            let mut sources = request
                .await
                .map_err(|e| format!("{e:?}"))?
                .into_inner()
                .items;
            let source = sources.remove(0);
            match source.trickplay {
                Some(_) => Ok(source),
                None => Err("no trickplay yet".to_owned()),
            }
        }
    })
    .await;
    let trickplay = source.trickplay.clone().expect("waited for");
    let duration_ms = (arrival.duration_secs() * 1000.0) as i64;
    assert_eq!(
        trickplay.tile_count,
        (duration_ms + trickplay.interval_ms - 1) / trickplay.interval_ms,
        "a tile for every interval the file starts: {trickplay:?}"
    );
    assert!(
        trickplay.sheet_urls.len() as i64 * trickplay.columns * trickplay.rows
            >= trickplay.tile_count,
        "the sheets hold every tile: {trickplay:?}"
    );
    assert_eq!(
        source.parts[0].trickplay.as_ref().map(|t| &t.sheet_urls),
        Some(&trickplay.sheet_urls),
        "a whole file's one part has the file's sheets"
    );

    let base = admin.stack.base_url().to_owned();
    for url in &trickplay.sheet_urls {
        let sheet = raw_get_with(&base, url, Some(&admin.session), &[]).await;
        assert_eq!(sheet.status().as_u16(), 200, "{url}");
        assert_eq!(header(&sheet, "content-type"), Some("image/jpeg"));
        let etag = header(&sheet, "etag")
            .expect("a sheet has a validator")
            .to_owned();
        let bytes = sheet.bytes().await.expect("the sheet");
        assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]), "a JPEG: {url}");

        let revalidated = raw_get_with(
            &base,
            url,
            Some(&admin.session),
            &[("if-none-match", &etag)],
        )
        .await;
        assert_eq!(revalidated.status().as_u16(), 304, "{url} is unchanged");

        let anonymous = raw_get_with(&base, url, None, &[]).await;
        assert_eq!(anonymous.status().as_u16(), 401, "never without a session");
    }
    let past_the_last = format!(
        "/v1/files/{}/trickplay/{}",
        source.file_id,
        trickplay.sheet_urls.len()
    );
    assert_eq!(
        raw_get_with(&base, &past_the_last, Some(&admin.session), &[])
            .await
            .status()
            .as_u16(),
        404,
        "a sheet the source does not name"
    );

    // Made under the data directory, in the file's own folder; the library
    // tree is checked byte for byte by `finish`.
    let folder = admin
        .stack
        .data_dir()
        .join("trickplay")
        .join(source.file_id.to_string());
    let sheets_on_disk = std::fs::read_dir(&folder)
        .unwrap_or_else(|e| panic!("read {}: {e}", folder.display()))
        .filter_map(Result::ok)
        .flat_map(|version| std::fs::read_dir(version.path()).into_iter().flatten())
        .filter_map(Result::ok)
        .filter(|sheet| sheet.path().extension().is_some_and(|e| e == "jpg"))
        .count();
    assert_eq!(sheets_on_disk, trickplay.sheet_urls.len());

    admin.finish().await;
}
