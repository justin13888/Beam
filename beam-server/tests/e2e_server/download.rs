//! Download (`GET /v1/files/{id}/download`): the source file's own bytes as
//! an attachment, whole or by range, resumable against its validator.

use beam_test_support::e2e::client::api;

use crate::common::{Admin, VIEWER, Video, first_source, header, raw_get_with};

/// A download is the file itself, saved under its own name: whole, from an
/// offset, or its last bytes; resumed only while its validator still holds;
/// never without a session, and never of a library the caller may not see.
#[tokio::test]
async fn a_download_is_the_files_bytes_by_range_and_resumes_on_its_validator() {
    let mut admin = Admin::start().await;
    admin.stack.library.add_video(
        "films/Arrival (2016)/Arrival (2016).mp4",
        &Video::new(440).seconds(8),
    );
    admin.stack.library.add_video(
        "kids/Paddington (2014)/Paddington (2014).mp4",
        &Video::new(330),
    );
    let films = admin.add_library("Films", "films").await;
    let kids = admin.add_library("Kids", "kids").await;
    admin.scan_ok(films).await;
    admin.scan_ok(kids).await;
    let arrival = admin.title("Arrival").await.id;
    let source = first_source(&admin.api, arrival).await;
    let url = source.download_url.clone();
    assert_ne!(
        url, source.stream_url,
        "a download is its own resource, not the inline stream"
    );
    let bytes = std::fs::read(
        admin
            .stack
            .library
            .path("films/Arrival (2016)/Arrival (2016).mp4"),
    )
    .expect("read the fixture");
    let length = bytes.len();
    let base = admin.stack.base_url().to_owned();
    let session = admin.session.clone();

    // Whole: an attachment named for the file, carrying a strong validator.
    let whole = raw_get_with(&base, &url, Some(&session), &[]).await;
    assert_eq!(whole.status().as_u16(), 200);
    let disposition = header(&whole, "content-disposition")
        .expect("a download is an attachment")
        .to_owned();
    assert!(
        disposition.starts_with("attachment") && disposition.contains("Arrival (2016).mp4"),
        "saved under the file's own name: {disposition}"
    );
    assert_eq!(header(&whole, "accept-ranges"), Some("bytes"));
    let etag = header(&whole, "etag")
        .expect("a download carries a validator")
        .to_owned();
    assert!(!etag.starts_with("W/"), "a strong validator: {etag}");
    assert_eq!(
        whole.bytes().await.expect("the body").as_ref(),
        bytes.as_slice()
    );

    // From an offset to the end: where an interrupted download resumes.
    let resumed = raw_get_with(
        &base,
        &url,
        Some(&session),
        &[("range", "bytes=1000-"), ("if-range", &etag)],
    )
    .await;
    assert_eq!(
        resumed.status().as_u16(),
        206,
        "a resume against the current validator"
    );
    assert_eq!(
        header(&resumed, "content-range"),
        Some(format!("bytes 1000-{}/{length}", length - 1).as_str())
    );
    assert!(
        header(&resumed, "content-disposition").is_some_and(|d| d.starts_with("attachment")),
        "a part of a download is still a download"
    );
    assert_eq!(
        resumed.bytes().await.expect("the body").as_ref(),
        &bytes[1000..]
    );

    // The last bytes: a suffix range.
    let tail = raw_get_with(&base, &url, Some(&session), &[("range", "bytes=-512")]).await;
    assert_eq!(tail.status().as_u16(), 206);
    assert_eq!(
        header(&tail, "content-range"),
        Some(format!("bytes {}-{}/{length}", length - 512, length - 1).as_str())
    );
    assert_eq!(
        tail.bytes().await.expect("the body").as_ref(),
        &bytes[length - 512..]
    );

    // A resume against a validator the file no longer has is the whole file
    // again, never bytes spliced from two versions of it.
    let stale = raw_get_with(
        &base,
        &url,
        Some(&session),
        &[("range", "bytes=1000-"), ("if-range", "\"not-this-file\"")],
    )
    .await;
    assert_eq!(stale.status().as_u16(), 200, "a stale resume starts over");
    assert_eq!(
        stale.bytes().await.expect("the body").as_ref(),
        bytes.as_slice()
    );

    let past_the_end = raw_get_with(
        &base,
        &url,
        Some(&session),
        &[("range", &format!("bytes={}-", length + 10))],
    )
    .await;
    assert_eq!(past_the_end.status().as_u16(), 416, "a range past the end");

    let anonymous = raw_get_with(&base, &url, None, &[]).await;
    assert_eq!(
        anonymous.status().as_u16(),
        401,
        "media is never downloaded without a session"
    );

    // A viewer restricted to the children's library cannot download a film:
    // to them the file does not exist.
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
    let hidden = raw_get_with(&base, &url, Some(&viewer_session), &[]).await;
    assert_eq!(hidden.status().as_u16(), 404);
    let paddington = admin.title("Paddington").await.id;
    let own = first_source(&viewer, paddington).await;
    let allowed = raw_get_with(
        &base,
        &own.download_url,
        Some(&viewer_session),
        &[("range", "bytes=0-99")],
    )
    .await;
    assert_eq!(allowed.status().as_u16(), 206, "their own library's file");
    assert_eq!(allowed.bytes().await.expect("the body").len(), 100);

    admin.finish().await;
}
