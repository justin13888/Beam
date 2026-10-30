use super::*;
use crate::capability::QualityPolicy;
use crate::ffi::playback::PLAYBACK_PLAN_LOOKUP_TIMEOUT;

#[tokio::test]
async fn playback_needs_a_session_before_it_will_hand_over_a_url() {
    // Returning a URL with no cookie would produce a request the server
    // answers with 401, surfacing to the viewer as a broken file.
    let (client, _) = client_with_server().await;

    assert!(matches!(
        client.playback_config("file-1".to_owned()),
        Err(BeamError::Unauthenticated)
    ));
    assert!(matches!(
        client.server_http_config(),
        Err(BeamError::Unauthenticated)
    ));
}

#[tokio::test]
async fn a_playback_config_carries_the_cookie_and_the_stream_url() {
    let (_, client, _) = restarted_after_sign_in(ME, "opaque-value").await;

    let config = client
        .playback_config("file-1".to_owned())
        .expect("configured");

    assert_eq!(config.url, "https://beam.test/v1/files/file-1/stream");
    assert_eq!(
        config.headers.get("Cookie").map(String::as_str),
        Some("beam_session=opaque-value"),
        "Media3 fetches the bytes itself, so the credential has to travel with it"
    );
    assert_eq!(config.pinned_host, "beam.test");
}

/// The server's one track model (issue #189) reaches the platform as the
/// view it already knew: the picture from the first video track, every
/// audio track, the codecs as the server named them, the sources in the
/// server's order, and the URLs resolved against the server.
#[tokio::test]
async fn sources_become_views_of_their_first_video_track() {
    const SOURCES: &str = r#"{"items":[
        {"file_id":"f1f1f1f1-0000-4000-8000-000000000001","is_primary":true,
         "edition":null,"episode_span":null,
         "parts":[
            {"file_id":"f1f1f1f1-0000-4000-8000-000000000001","part_number":null,
             "size_bytes":4000,"duration_secs":6300.0,"subtitle_tracks":[],
             "stream_url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/stream",
             "download_url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/download"}],
         "size_bytes":4000,
         "mime_type":"video/x-matroska","container_format":"matroska",
         "duration_secs":6300.0,
         "video_tracks":[
            {"index":0,"codec":"hevc","width":3840,"height":2160,
             "frame_rate":null,"bit_rate":20000000,"hdr_format":"HDR10"},
            {"index":3,"codec":"mjpeg","width":600,"height":900,
             "frame_rate":null,"bit_rate":null,"hdr_format":null}],
         "audio_tracks":[
            {"index":1,"codec":"truehd","language":"eng","title":null,
             "channels":8,"channel_layout":"7.1","sample_rate":48000,
             "bit_rate":null,"is_default":true,"is_forced":false},
            {"index":2,"codec":"eac3","language":"fre","title":null,
             "channels":6,"channel_layout":null,"sample_rate":null,
             "bit_rate":null,"is_default":false,"is_forced":false}],
         "subtitle_tracks":[
            {"origin":"sidecar","index":null,
             "sidecar_id":"5c5c5c5c-0000-4000-8000-000000000001",
             "codec":"subrip","language":"eng","title":null,
             "is_default":false,"is_forced":false,"is_hearing_impaired":true,
             "is_text":true,
             "url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/subtitles/5c5c5c5c-0000-4000-8000-000000000001",
             "webvtt_url":null}],
         "stream_url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/stream",
         "download_url":"/v1/files/f1f1f1f1-0000-4000-8000-000000000001/download"},
        {"file_id":"f2f2f2f2-0000-4000-8000-000000000002","is_primary":false,
         "edition":"Director's Cut","episode_span":null,
         "parts":[
            {"file_id":"f2f2f2f2-0000-4000-8000-000000000002","part_number":null,
             "size_bytes":1000,"duration_secs":null,"subtitle_tracks":[],
             "stream_url":"/v1/files/f2f2f2f2-0000-4000-8000-000000000002/stream",
             "download_url":"/v1/files/f2f2f2f2-0000-4000-8000-000000000002/download"}],
         "size_bytes":1000,
         "mime_type":null,"container_format":null,"duration_secs":null,
         "video_tracks":[],"audio_tracks":[],"subtitle_tracks":[],
         "stream_url":"/v1/files/f2f2f2f2-0000-4000-8000-000000000002/stream",
         "download_url":"/v1/files/f2f2f2f2-0000-4000-8000-000000000002/download"}
    ],
    "page_info":{"has_next_page":false,"has_previous_page":false,
                 "start_cursor":null,"end_cursor":null}}"#;
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering(200, "application/json", SOURCES));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let views = client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect("the sources decode");

    assert_eq!(views.len(), 2);
    let primary = &views[0];
    assert_eq!(primary.file_id, "f1f1f1f1-0000-4000-8000-000000000001");
    assert_eq!(primary.video_codec.as_deref(), Some("hevc"));
    assert_eq!((primary.width, primary.height), (Some(3840), Some(2160)));
    assert_eq!(primary.bit_rate, Some(20_000_000));
    assert_eq!(primary.hdr_format.as_deref(), Some("HDR10"));
    let audio: Vec<(&str, u16, bool)> = primary
        .audio_tracks
        .iter()
        .map(|a| (a.codec.as_str(), a.channels, a.is_default))
        .collect();
    assert_eq!(audio, vec![("truehd", 8, true), ("eac3", 6, false)]);
    assert_eq!(
        primary.stream_url,
        "https://beam.test/v1/files/f1f1f1f1-0000-4000-8000-000000000001/stream"
    );

    let other = &views[1];
    assert_eq!(other.file_id, "f2f2f2f2-0000-4000-8000-000000000002");
    assert_eq!(other.video_codec, None, "no video track, no picture");
    assert_eq!((other.width, other.height), (None, None));
}

/// A Blu-ray rip of one film across two clips, the second with a forced
/// Spanish sidecar, and an embedded image subtitle in the first.
const MULTI_PART_SOURCES: &str = r#"{"items":[
        {"file_id":"c1c1c1c1-0000-4000-8000-000000000001","is_primary":true,
         "edition":null,"episode_span":null,"disc_structure":"blu_ray",
         "parts":[
            {"file_id":"c1c1c1c1-0000-4000-8000-000000000001","part_number":1,
             "size_bytes":3000,"duration_secs":3000.0,"subtitle_tracks":[],
             "stream_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/stream",
             "download_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/download"},
            {"file_id":"c2c2c2c2-0000-4000-8000-000000000002","part_number":2,
             "size_bytes":2000,"duration_secs":2800.0,
             "subtitle_tracks":[
                {"origin":"sidecar","index":null,
                 "sidecar_id":"5d5d5d5d-0000-4000-8000-000000000002",
                 "codec":"subrip","language":"spa","title":"Forced",
                 "is_default":false,"is_forced":true,"is_hearing_impaired":false,
                 "is_text":true,
                 "url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/subtitles/5d5d5d5d-0000-4000-8000-000000000002",
                 "webvtt_url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/subtitles/5d5d5d5d-0000-4000-8000-000000000002/webvtt"}],
             "stream_url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/stream",
             "download_url":"/v1/files/c2c2c2c2-0000-4000-8000-000000000002/download"}],
         "size_bytes":5000,
         "mime_type":null,"container_format":"mpegts","duration_secs":5800.0,
         "video_tracks":[{"index":0,"codec":"h264","width":1920,"height":1080,
                          "frame_rate":null,"bit_rate":null,"hdr_format":null}],
         "audio_tracks":[],
         "subtitle_tracks":[
            {"origin":"embedded","index":3,"sidecar_id":null,
             "codec":"hdmv_pgs_subtitle","language":"eng","title":null,
             "is_default":true,"is_forced":false,"is_hearing_impaired":false,
             "is_text":false,"url":null,"webvtt_url":null}],
         "stream_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/stream",
         "download_url":"/v1/files/c1c1c1c1-0000-4000-8000-000000000001/download"}
    ],
    "page_info":{"has_next_page":false,"has_previous_page":false,
                 "start_cursor":null,"end_cursor":null}}"#;

/// A multi-part source (issue #233) carries every part in the server's
/// order with its own absolute stream URL and its own sidecars, a disc rip
/// (issue #234) its structure, and a sidecar subtitle (issue #189) the
/// URLs the player fetches it from -- resolved, so the player needs no
/// base URL of its own.
#[tokio::test]
async fn a_multi_part_disc_source_carries_its_parts_structure_and_sidecars() {
    let client = client_answering_sources(MULTI_PART_SOURCES).await;

    let views = client
        .media_sources(MEDIA_ID.to_owned())
        .await
        .expect("the sources decode");
    let source = &views[0];

    assert_eq!(
        source.disc_structure,
        Some(crate::capability::select::DiscStructure::BluRay)
    );
    let parts: Vec<(&str, Option<u32>, Option<f64>, &str)> = source
        .parts
        .iter()
        .map(|part| {
            (
                part.file_id.as_str(),
                part.part_number,
                part.duration_secs,
                part.stream_url.as_str(),
            )
        })
        .collect();
    assert_eq!(
        parts,
        vec![
            (
                "c1c1c1c1-0000-4000-8000-000000000001",
                Some(1),
                Some(3000.0),
                "https://beam.test/v1/files/c1c1c1c1-0000-4000-8000-000000000001/stream"
            ),
            (
                "c2c2c2c2-0000-4000-8000-000000000002",
                Some(2),
                Some(2800.0),
                "https://beam.test/v1/files/c2c2c2c2-0000-4000-8000-000000000002/stream"
            ),
        ]
    );
    assert!(source.contains_file("c2c2c2c2-0000-4000-8000-000000000002"));

    let embedded = &source.subtitle_tracks[0];
    assert_eq!(
        embedded.origin,
        crate::capability::select::SubtitleOrigin::Embedded
    );
    assert_eq!(embedded.index, Some(3));
    assert!(!embedded.is_text);
    assert_eq!(
        (embedded.url.as_deref(), embedded.webvtt_url.as_deref()),
        (None, None)
    );

    let sidecar = &source.parts[1].subtitle_tracks[0];
    assert_eq!(
        sidecar.origin,
        crate::capability::select::SubtitleOrigin::Sidecar
    );
    assert_eq!(
        sidecar.sidecar_id.as_deref(),
        Some("5d5d5d5d-0000-4000-8000-000000000002")
    );
    assert_eq!(sidecar.language.as_deref(), Some("spa"));
    assert!(sidecar.is_forced);
    assert_eq!(
        sidecar.webvtt_url.as_deref(),
        Some(
            "https://beam.test/v1/files/c2c2c2c2-0000-4000-8000-000000000002/subtitles/5d5d5d5d-0000-4000-8000-000000000002/webvtt"
        )
    );
    assert_eq!(
        sidecar.url.as_deref(),
        Some(
            "https://beam.test/v1/files/c2c2c2c2-0000-4000-8000-000000000002/subtitles/5d5d5d5d-0000-4000-8000-000000000002"
        )
    );
}

/// Resuming a multi-part movie in its second part queues every part and
/// starts in the second, each part with only its own subtitle files.
#[tokio::test]
async fn a_plan_queues_every_part_and_starts_in_the_one_named() {
    let client = client_answering_sources(MULTI_PART_SOURCES).await;

    let plan = client
        .playback_plan(
            Some(MEDIA_ID.to_owned()),
            "c2c2c2c2-0000-4000-8000-000000000002".to_owned(),
        )
        .await
        .expect("signed in, with a source to plan from");

    let files: Vec<&str> = plan.parts.iter().map(|p| p.file_id.as_str()).collect();
    assert_eq!(
        files,
        vec![
            "c1c1c1c1-0000-4000-8000-000000000001",
            "c2c2c2c2-0000-4000-8000-000000000002"
        ]
    );
    assert_eq!(plan.start_part_index, 1);
    assert_eq!(
        plan.parts[1].url,
        "https://beam.test/v1/files/c2c2c2c2-0000-4000-8000-000000000002/stream"
    );
    assert_eq!(plan.parts[0].duration_secs, Some(3000.0));
    assert!(plan.parts[0].sidecar_subtitles.is_empty());
    assert_eq!(
        plan.parts[1].sidecar_subtitles[0].language.as_deref(),
        Some("spa")
    );
    assert_eq!(
        plan.headers.get("Cookie").map(String::as_str),
        Some("beam_session=opaque-value")
    );
    assert_eq!(plan.pinned_host, "beam.test");
    // The origin the player scopes the session to, and every part is on it.
    assert_eq!(plan.base_url, "https://beam.test");
    assert!(
        plan.parts
            .iter()
            .all(|part| part.url.starts_with("https://beam.test/"))
    );
}

/// A downloaded file played with no connection cannot list its source,
/// and still plays: alone, from the URL the player would have used.
#[tokio::test]
async fn a_plan_whose_source_cannot_be_listed_is_the_file_alone() {
    let (client, _) = client_answering(
        500,
        r#"{"type":"https://beam.justinchung.net/reference/errors/#internal","status":500}"#,
    )
    .await;

    let plan = client
        .playback_plan(Some(MEDIA_ID.to_owned()), "f-1".to_owned())
        .await
        .expect("a failed lookup is not a failed plan");

    assert_eq!(plan.parts.len(), 1);
    assert_eq!(plan.parts[0].file_id, "f-1");
    assert_eq!(plan.parts[0].url, "https://beam.test/v1/files/f-1/stream");
    assert_eq!(plan.start_part_index, 0);
}

/// Without a title there is no source to look up: the plan is the file
/// alone, and nothing is asked of the server.
#[tokio::test]
async fn a_plan_without_a_title_is_the_file_alone_and_asks_nothing() {
    let (client, backend) = one_answer(500, r#"{"type":"about:blank","status":500}"#).await;

    let plan = client
        .playback_plan(None, "f-1".to_owned())
        .await
        .expect("a plan of one file");

    assert_eq!(plan.parts.len(), 1);
    assert_eq!(plan.parts[0].url, "https://beam.test/v1/files/f-1/stream");
    assert_eq!(plan.start_part_index, 0);
    assert!(backend.recorded().is_empty(), "no title, so no lookup");
}

/// A server that never answers holds a start for the bounded lookup
/// only, then the file plays alone -- a download is not held for as long
/// as the OS takes to give up on a connection that drops every packet.
///
/// Tokio's paused clock advances only once the runtime is idle, so the
/// elapsed virtual time is exactly how long the plan waited.
#[tokio::test(start_paused = true)]
async fn a_plan_whose_source_lookup_never_answers_is_the_file_alone_once_bounded() {
    let (client, id, _) = signed_in_client().await;
    client
        .use_transport(
            &id,
            Arc::new(crate::transport::SilentBackend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    let started = tokio::time::Instant::now();

    let plan = client
        .playback_plan(Some(MEDIA_ID.to_owned()), "f-1".to_owned())
        .await
        .expect("a lookup that never answers is not a failed plan");

    assert_eq!(started.elapsed(), PLAYBACK_PLAN_LOOKUP_TIMEOUT);
    assert_eq!(plan.parts.len(), 1);
    assert_eq!(plan.parts[0].file_id, "f-1");
    assert_eq!(plan.parts[0].url, "https://beam.test/v1/files/f-1/stream");
    assert_eq!(plan.start_part_index, 0);
}

/// Two H.264 versions of one title: a 1080p file at 8 Mbps and a 720p
/// file at 3 Mbps.
const TWO_H264_SOURCES: &str = r#"{"items":[
    {"file_id":"a1a1a1a1-0000-4000-8000-000000000001","is_primary":true,
     "edition":null,"episode_span":null,"parts":[],
     "size_bytes":4000000000,"mime_type":"video/mp4","container_format":"mp4",
     "duration_secs":6300.0,
     "video_tracks":[{"index":0,"codec":"h264","width":1920,"height":1080,
                      "frame_rate":null,"bit_rate":8000000,"hdr_format":null}],
     "audio_tracks":[{"index":1,"codec":"aac","language":"eng","title":null,
                      "channels":2,"channel_layout":null,"sample_rate":null,
                      "bit_rate":null,"is_default":true,"is_forced":false}],
     "subtitle_tracks":[],
     "stream_url":"/v1/files/a1a1a1a1-0000-4000-8000-000000000001/stream",
     "download_url":"/v1/files/a1a1a1a1-0000-4000-8000-000000000001/download"},
    {"file_id":"b2b2b2b2-0000-4000-8000-000000000002","is_primary":false,
     "edition":null,"episode_span":null,"parts":[],
     "size_bytes":1500000000,"mime_type":"video/mp4","container_format":"mp4",
     "duration_secs":6300.0,
     "video_tracks":[{"index":0,"codec":"h264","width":1280,"height":720,
                      "frame_rate":null,"bit_rate":3000000,"hdr_format":null}],
     "audio_tracks":[{"index":1,"codec":"aac","language":"eng","title":null,
                      "channels":2,"channel_layout":null,"sample_rate":null,
                      "bit_rate":null,"is_default":true,"is_forced":false}],
     "subtitle_tracks":[],
     "stream_url":"/v1/files/b2b2b2b2-0000-4000-8000-000000000002/stream",
     "download_url":"/v1/files/b2b2b2b2-0000-4000-8000-000000000002/download"}
],
"page_info":{"has_next_page":false,"has_previous_page":false,
             "start_cursor":null,"end_cursor":null}}"#;

async fn client_answering_sources(body: &'static str) -> Arc<BeamClient> {
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering(200, "application/json", body));
    client
        .use_transport(&id, backend as Arc<dyn crate::api::HttpBackend>)
        .expect("the server is registered");
    client
}

#[tokio::test]
async fn a_downgrade_falls_back_from_the_server_sources_to_the_lighter_file() {
    let client = client_answering_sources(TWO_H264_SOURCES).await;
    client.set_device_profile(
        crate::capability::builders::DeviceProfileBuilder::phone_h264_hevc().build(),
    );

    let downgrade = client
        .select_downgrade_source(
            MEDIA_ID.to_owned(),
            "a1a1a1a1-0000-4000-8000-000000000001".to_owned(),
        )
        .await
        .expect("the sources decode")
        .expect("the 720p file is lighter and plays");

    assert_eq!(
        downgrade.source.file_id,
        "b2b2b2b2-0000-4000-8000-000000000002"
    );
    assert_eq!(
        downgrade.reason,
        "Switched to 720p because playback kept stalling"
    );
}

#[tokio::test]
async fn the_lightest_file_has_no_downgrade_and_that_is_not_an_error() {
    let client = client_answering_sources(TWO_H264_SOURCES).await;
    client.set_device_profile(
        crate::capability::builders::DeviceProfileBuilder::phone_h264_hevc().build(),
    );

    let downgrade = client
        .select_downgrade_source(
            MEDIA_ID.to_owned(),
            "b2b2b2b2-0000-4000-8000-000000000002".to_owned(),
        )
        .await
        .expect("nothing lighter is an answer, not a failure");

    assert_eq!(downgrade, None);
}

#[tokio::test]
async fn a_downgrade_is_refused_before_the_device_profile_is_known() {
    // Without the profile there is no way to know which smaller file
    // plays, and guessing is how a stall becomes a decoder failure.
    let client = client_answering_sources(TWO_H264_SOURCES).await;

    let refused = client
        .select_downgrade_source(
            MEDIA_ID.to_owned(),
            "a1a1a1a1-0000-4000-8000-000000000001".to_owned(),
        )
        .await;

    assert!(
        matches!(refused, Err(BeamError::BadRequest { .. })),
        "{refused:?}"
    );
}

#[test]
fn the_rebuffer_monitor_carries_the_detectors_verdict_across_calls() {
    // The platform holds one monitor per player and calls it from the
    // player's event callbacks; the verdict must accumulate across those
    // calls, and `reset` must clear it for the next file.
    let monitor = RebufferMonitor::new();
    assert!(!monitor.stall_started(0));
    assert!(!monitor.stall_ended(1_000));
    assert!(!monitor.stall_started(10_000));
    assert!(!monitor.stall_ended(11_000));
    assert!(monitor.stall_started(20_000), "the third stall in a minute");
    assert!(monitor.poll(20_500));
    assert!(
        monitor.stall_ended(21_000),
        "the verdict stands once the stall ends"
    );

    monitor.reset();
    assert!(!monitor.poll(21_000));
}

// -- characterization: choosing a source ------------------------------

/// Two files nothing on a phone decodes.
const UNPLAYABLE_SOURCES: &str = r#"{"items":[
    {"file_id":"a1a1a1a1-0000-4000-8000-000000000001","is_primary":true,
     "edition":null,"episode_span":null,"parts":[],
     "size_bytes":4000,"mime_type":"video/mp4","container_format":"mp4",
     "duration_secs":60.0,
     "video_tracks":[{"index":0,"codec":"vp6","width":640,"height":480,
                      "frame_rate":null,"bit_rate":null,"hdr_format":null}],
     "audio_tracks":[],"subtitle_tracks":[],
     "stream_url":"/v1/files/a1a1a1a1-0000-4000-8000-000000000001/stream",
     "download_url":"/v1/files/a1a1a1a1-0000-4000-8000-000000000001/download"},
    {"file_id":"b2b2b2b2-0000-4000-8000-000000000002","is_primary":false,
     "edition":null,"episode_span":null,"parts":[],
     "size_bytes":2000,"mime_type":"video/mp4","container_format":"mp4",
     "duration_secs":60.0,
     "video_tracks":[{"index":0,"codec":"theora","width":640,"height":480,
                      "frame_rate":null,"bit_rate":null,"hdr_format":null}],
     "audio_tracks":[],"subtitle_tracks":[],
     "stream_url":"/v1/files/b2b2b2b2-0000-4000-8000-000000000002/stream",
     "download_url":"/v1/files/b2b2b2b2-0000-4000-8000-000000000002/download"}
],
"page_info":{"has_next_page":false,"has_previous_page":false,
             "start_cursor":null,"end_cursor":null}}"#;

#[tokio::test]
async fn a_title_nothing_here_can_play_is_not_found_with_the_first_reason() {
    let client = client_answering_sources(UNPLAYABLE_SOURCES).await;
    client.set_device_profile(
        crate::capability::builders::DeviceProfileBuilder::phone_h264_hevc().build(),
    );

    let refused = client
        .select_playback_source(MEDIA_ID.to_owned(), QualityPolicy::Highest)
        .await;

    assert_eq!(
        refused,
        Err(BeamError::NotFound {
            detail: "No VP6 decoder on this device".to_owned(),
            code: ABOUT_BLANK.to_owned(),
        })
    );
}

#[tokio::test]
async fn the_highest_playable_source_is_selected() {
    let client = client_answering_sources(TWO_H264_SOURCES).await;
    client.set_device_profile(
        crate::capability::builders::DeviceProfileBuilder::phone_h264_hevc().build(),
    );

    let selection = client
        .select_playback_source(MEDIA_ID.to_owned(), QualityPolicy::Highest)
        .await
        .expect("both files play");

    assert_eq!(
        selection.source.file_id,
        "a1a1a1a1-0000-4000-8000-000000000001"
    );
}

#[tokio::test]
async fn selection_is_refused_before_the_profile_is_set() {
    let client = client_answering_sources(TWO_H264_SOURCES).await;

    assert!(matches!(
        client
            .select_playback_source(MEDIA_ID.to_owned(), QualityPolicy::Highest)
            .await,
        Err(BeamError::BadRequest { .. })
    ));
}
