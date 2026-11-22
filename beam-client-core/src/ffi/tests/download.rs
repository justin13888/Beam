use super::*;
use crate::download::{DownloadRequest, DownloadState, TransferReport};

const CD1: &str = "c1c1c1c1-0000-4000-8000-000000000001";
const CD2: &str = "c2c2c2c2-0000-4000-8000-000000000002";
const SIDECAR: &str = "5d5d5d5d-0000-4000-8000-000000000002";
const LONE: &str = "f1000000-0000-4000-8000-000000000001";

/// One film across two clips, the second with a sidecar subtitle file.
const TWO_PART_SOURCES: &str = r#"{"items":[
        {"file_id":"c1c1c1c1-0000-4000-8000-000000000001","is_primary":true,
         "edition":null,"episode_span":null,
         "parts":[
            {"file_id":"c1c1c1c1-0000-4000-8000-000000000001","part_number":1,
             "size_bytes":3000,"duration_secs":3000.0,"subtitle_tracks":[],
             "stream_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/stream",
             "download_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/download"},
            {"file_id":"c2c2c2c2-0000-4000-8000-000000000002","part_number":2,
             "size_bytes":1000,"duration_secs":2800.0,
             "subtitle_tracks":[
                {"origin":"sidecar","index":null,
                 "sidecar_id":"5d5d5d5d-0000-4000-8000-000000000002",
                 "codec":"subrip","language":"spa","title":null,
                 "is_default":false,"is_forced":true,"is_hearing_impaired":false,
                 "is_text":true,
                 "url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/subtitles/5d5d5d5d-0000-4000-8000-000000000002",
                 "webvtt_url":null}],
             "stream_url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/stream",
             "download_url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/download"}],
         "size_bytes":4000,
         "mime_type":null,"container_format":"mpegts","duration_secs":5800.0,
         "video_tracks":[{"index":0,"codec":"h264","width":1920,"height":1080,
                          "frame_rate":null,"bit_rate":null,"hdr_format":null}],
         "audio_tracks":[],"subtitle_tracks":[],
         "stream_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/stream",
         "download_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/download"}
    ],
    "page_info":{"has_next_page":false,"has_previous_page":false,
                 "start_cursor":null,"end_cursor":null}}"#;

fn request(file_id: &str) -> DownloadRequest {
    DownloadRequest {
        media_id: MEDIA_ID.to_owned(),
        episode_id: None,
        file_id: file_id.to_owned(),
        title: "Arrival".to_owned(),
        subtitle: Some("Director's cut".to_owned()),
        artwork_path: Some("/files/download-artwork/ab12".to_owned()),
    }
}

/// A signed-in server answering the sources lookup, over storage the test
/// keeps.
async fn client_with_sources(
    storage: &Arc<InMemoryKeyValueStore>,
) -> (Arc<BeamClient>, String, Arc<CannedBackend>) {
    let (client, id, _) = signed_in_client_over(Arc::clone(storage)).await;
    let backend = Arc::new(CannedBackend::answering(
        200,
        "application/json",
        TWO_PART_SOURCES,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    (client, id, backend)
}

/// Queueing the second clip of a two-clip film plans both clips and the
/// sidecar, from the download route, sized from the catalog -- and the
/// record reads back on a cold start with no session and no network.
#[tokio::test]
async fn a_queued_download_plans_its_whole_source_and_renders_offline() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, backend) = client_with_sources(&storage).await;

    let record = client
        .enqueue_download(request(CD2))
        .await
        .expect("signed in, with a source to plan from");

    let recorded = backend.recorded();
    assert!(
        recorded
            .iter()
            .any(|request| request.url.path() == format!("/v1/media/{MEDIA_ID}/sources")),
        "the plan comes from the title's sources"
    );
    assert_eq!(record.state, DownloadState::Queued);
    assert_eq!(record.server_id, id);
    assert_eq!(record.file_id, CD2);
    assert_eq!(record.total_bytes, Some(4000));
    assert_eq!(record.progress, Some(0.0));
    let items: Vec<&str> = record.plan.item_ids().collect();
    assert_eq!(items, vec![CD1, CD2, SIDECAR]);
    assert_eq!(
        record.plan.parts[0].download_url,
        format!("https://beam.test/v1/files/{CD1}/download")
    );
    assert_eq!(
        record.plan.subtitles[0].url,
        format!("https://beam.test/v1/files/{CD2}/subtitles/{SIDECAR}")
    );

    // A cold start that restores nothing: no server, no session.
    let offline = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>);
    let listed = offline.downloads().await.expect("read from storage alone");
    assert_eq!(listed, vec![record]);
}

/// The engine's reports move the record, across items, and persist.
#[tokio::test]
async fn reports_move_a_download_to_completion() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, _, _) = client_with_sources(&storage).await;
    client.enqueue_download(request(CD1)).await.expect("queued");

    let report = |item: &str, state, downloaded, total| TransferReport {
        item_id: item.to_owned(),
        state,
        downloaded_bytes: downloaded,
        total_bytes: total,
        failure_message: None,
    };
    let after = client
        .report_download_transfers(vec![
            report(CD1, DownloadState::Completed, 3000, Some(3000)),
            report(CD2, DownloadState::Downloading, 500, Some(1000)),
        ])
        .await
        .expect("reported");
    assert_eq!(after[0].state, DownloadState::Downloading);
    assert_eq!(after[0].progress, Some(0.875));

    client
        .report_download_transfers(vec![
            report(CD2, DownloadState::Completed, 1000, Some(1000)),
            report(SIDECAR, DownloadState::Completed, 80, Some(80)),
        ])
        .await
        .expect("reported");
    let listed = BeamClient::new(Arc::clone(&storage) as Arc<dyn KeyValueStore>)
        .downloads()
        .await
        .expect("listed");
    assert_eq!(listed[0].state, DownloadState::Completed);
}

/// A source lookup that fails plans the file alone rather than refusing
/// the download.
#[tokio::test]
async fn a_download_whose_source_cannot_be_listed_is_the_file_alone() {
    let (client, _) = client_answering(
        500,
        r#"{"type":"https://beam.justinchung.net/reference/errors/#internal","status":500}"#,
    )
    .await;

    let record = client
        .enqueue_download(request(LONE))
        .await
        .expect("a failed lookup is not a failed download");

    assert_eq!(record.plan.parts.len(), 1);
    assert_eq!(record.plan.parts[0].file_id, LONE);
    assert_eq!(record.total_bytes, None, "no catalog, no size");
    assert_eq!(record.progress, None);
}

/// A file id that is not a UUID is refused before anything is stored: it
/// would otherwise be interpolated into a URL carrying the session.
#[tokio::test]
async fn a_malformed_file_id_is_refused_and_nothing_is_stored() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, _, _) = client_with_sources(&storage).await;

    let refused = client.enqueue_download(request("../admin")).await;

    assert!(
        matches!(refused, Err(BeamError::BadRequest { .. })),
        "{refused:?}"
    );
    assert!(client.downloads().await.expect("listed").is_empty());
}

#[tokio::test]
async fn a_download_needs_an_active_server() {
    let refused = client().enqueue_download(request(LONE)).await;
    assert!(
        matches!(refused, Err(BeamError::NoActiveServer)),
        "{refused:?}"
    );
}

/// Removing a server leaves its downloads for the platform, whose call to
/// remove them answers the records -- with every item -- so it knows which
/// bytes to delete. Another server's downloads stay.
#[tokio::test]
async fn a_servers_downloads_leave_only_through_their_own_removal() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = client_with_sources(&storage).await;
    client.enqueue_download(request(CD1)).await.expect("queued");
    crate::ports::kv::KeyValueStore::put(
        storage.as_ref(),
        "downloads/elsewhere".to_owned(),
        "[]".to_owned(),
    )
    .await
    .expect("put");

    client.remove_server(id.clone()).await.expect("removed");
    assert_eq!(client.downloads().await.expect("listed").len(), 1);

    let removed = client.remove_server_downloads(id).await.expect("removed");
    assert_eq!(removed.len(), 1);
    assert_eq!(
        removed[0].plan.item_ids().collect::<Vec<_>>(),
        vec![CD1, CD2, SIDECAR]
    );
    assert!(client.downloads().await.expect("listed").is_empty());
    assert!(storage.has_plain("downloads/elsewhere"));
}

/// Removing one download answers it; removing it again answers nothing.
#[tokio::test]
async fn removing_a_download_answers_it_once() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, _, _) = client_with_sources(&storage).await;
    client.enqueue_download(request(CD1)).await.expect("queued");

    let removed = client
        .remove_download(CD1.to_owned())
        .await
        .expect("removed");
    assert_eq!(removed.map(|record| record.file_id), Some(CD1.to_owned()));
    assert_eq!(
        client
            .remove_download(CD1.to_owned())
            .await
            .expect("removed"),
        None
    );
    assert!(
        client
            .remove_all_downloads()
            .await
            .expect("removed")
            .is_empty()
    );
}
