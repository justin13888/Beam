use super::*;
use crate::telemetry::{StartFailureCause, SwitchTrigger, TelemetryFlush};

const TELEMETRY_PATH: &str = "/v1/telemetry/playback";

/// A distinct, well-formed file id per `n`.
fn telemetry_file(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

/// A signed-in client whose telemetry requests get these answers in turn.
async fn telemetry_client(
    answers: Vec<(u16, &'static str, &'static str)>,
) -> (Arc<BeamClient>, Arc<CannedBackend>) {
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering_in_turn(answers));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    (client, backend)
}

/// Every telemetry request's body, in order.
fn telemetry_bodies(backend: &CannedBackend) -> Vec<serde_json::Value> {
    backend
        .recorded()
        .iter()
        .filter(|request| request.url.path() == TELEMETRY_PATH)
        .map(|request| {
            assert_eq!(request.method, reqwest::Method::POST);
            serde_json::from_slice(&request.body).expect("a JSON body")
        })
        .collect()
}

const NO_CONTENT: (u16, &str, &str) = (204, "text/plain", "");

#[tokio::test]
async fn telemetry_leaves_at_most_fifty_events_to_a_request() {
    let (client, backend) = telemetry_client(vec![NO_CONTENT]).await;
    client.set_playback_client_kind(TelemetryClientKind::AndroidTv);
    for n in 0..120 {
        client
            .record_playback_start(telemetry_file(n), Some(0))
            .expect("recorded");
    }

    let flush = client.flush_playback_telemetry().await.expect("flushed");

    assert_eq!(
        flush,
        TelemetryFlush {
            sent: 120,
            dropped: 0,
            pending: 0,
            disabled: false
        }
    );
    let bodies = telemetry_bodies(&backend);
    let sizes: Vec<_> = bodies
        .iter()
        .map(|body| body["starts"].as_array().expect("starts").len())
        .collect();
    assert_eq!(sizes, [50, 50, 20]);
    assert_eq!(bodies[0]["client_kind"], "android_tv");
    assert_eq!(
        bodies[0]["starts"][0]["file_id"],
        telemetry_file(0),
        "oldest first"
    );
}

#[tokio::test]
async fn a_409_stops_telemetry_for_the_rest_of_the_session() {
    let (client, backend) = telemetry_client(vec![(
        409,
        "application/problem+json",
        r#"{"type":"https://beam.justinchung.net/reference/errors/#playback-telemetry-disabled","status":409}"#,
    )])
    .await;
    for n in 0..60 {
        client
            .record_playback_rebuffer(telemetry_file(n), 2_000)
            .expect("recorded");
    }

    let flush = client.flush_playback_telemetry().await.expect("flushed");

    assert!(flush.disabled);
    assert_eq!((flush.sent, flush.pending), (0, 0));
    assert_eq!(
        telemetry_bodies(&backend).len(),
        1,
        "one refusal is the operator's answer; the rest of the queue is not tried"
    );
    assert_eq!(
        client
            .record_playback_start(telemetry_file(1), None)
            .expect("accepted"),
        0,
        "nothing is held once the server has said no"
    );
    client.flush_playback_telemetry().await.expect("flushed");
    assert_eq!(
        telemetry_bodies(&backend).len(),
        1,
        "and nothing is asked again"
    );
}

#[tokio::test]
async fn a_refused_batch_is_dropped_and_the_flush_moves_on() {
    let (client, backend) = telemetry_client(vec![
        (
            422,
            "application/problem+json",
            r#"{"type":"https://beam.justinchung.net/reference/errors/#validation-failed","status":422,"errors":[{"pointer":"/starts/0/file_id","detail":"bad"}]}"#,
        ),
        NO_CONTENT,
    ])
    .await;
    for n in 0..60 {
        client
            .record_playback_start(telemetry_file(n), None)
            .expect("recorded");
    }

    let flush = client.flush_playback_telemetry().await.expect("flushed");

    assert_eq!(
        flush,
        TelemetryFlush {
            sent: 10,
            dropped: 50,
            pending: 0,
            disabled: false
        }
    );
    assert_eq!(telemetry_bodies(&backend).len(), 2);
}

#[tokio::test]
async fn a_transient_failure_keeps_the_batch_for_the_next_flush() {
    let (client, backend) = telemetry_client(vec![
        (
            503,
            "application/problem+json",
            r#"{"type":"about:blank","status":503}"#,
        ),
        NO_CONTENT,
    ])
    .await;
    for n in 0..60 {
        client
            .record_playback_start(telemetry_file(n), None)
            .expect("recorded");
    }

    let first = client.flush_playback_telemetry().await.expect("flushed");
    assert_eq!((first.sent, first.dropped, first.pending), (0, 0, 60));
    assert_eq!(
        telemetry_bodies(&backend).len(),
        1,
        "an unavailable server is not handed the rest of the backlog"
    );

    let second = client.flush_playback_telemetry().await.expect("flushed");
    assert_eq!((second.sent, second.pending), (60, 0));
    assert_eq!(
        telemetry_bodies(&backend)[1]["starts"][0]["file_id"],
        telemetry_file(0),
        "the kept batch goes first"
    );
}

#[tokio::test]
async fn an_expired_session_ends_the_flush_and_keeps_the_events() {
    let (client, _) = telemetry_client(vec![(
        401,
        "application/problem+json",
        r#"{"type":"about:blank","status":401}"#,
    )])
    .await;
    client
        .record_playback_start(telemetry_file(1), None)
        .expect("recorded");

    assert_eq!(
        client.flush_playback_telemetry().await,
        Err(BeamError::SessionExpired)
    );
    assert_eq!(
        client
            .record_playback_start(telemetry_file(2), None)
            .expect("recorded"),
        2,
        "signing in again delivers what was waiting"
    );
}

#[tokio::test]
async fn uncounted_events_are_accepted_and_change_nothing() {
    let (client, backend) = telemetry_client(vec![NO_CONTENT]).await;

    let excluded = client
        .record_playback_start_failure(
            telemetry_file(1),
            StartFailureCause::Rejected {
                reason: crate::RejectionReason::ExcludedByPolicy,
            },
            None,
        )
        .expect("accepted");
    let reload = client
        .record_playback_source_switch(telemetry_file(1), telemetry_file(1), SwitchTrigger::Manual)
        .expect("accepted");
    let instant = client
        .record_playback_rebuffer(telemetry_file(1), 0)
        .expect("accepted");

    assert_eq!((excluded, reload, instant), (0, 0, 0));
    client.flush_playback_telemetry().await.expect("flushed");
    assert!(telemetry_bodies(&backend).is_empty(), "nothing to send");
}

#[tokio::test]
async fn a_preflight_rejection_reaches_the_wire_as_one() {
    let (client, backend) = telemetry_client(vec![NO_CONTENT]).await;
    client
        .record_playback_start_failure(
            telemetry_file(1),
            StartFailureCause::Rejected {
                reason: crate::RejectionReason::ContainerUnsupported,
            },
            Some(1),
        )
        .expect("recorded");

    client.flush_playback_telemetry().await.expect("flushed");

    assert_eq!(
        telemetry_bodies(&backend)[0]["start_failures"][0],
        serde_json::json!({
            "file_id": telemetry_file(1),
            "reason": "container",
            "stage": "preflight",
            "audio_track_index": 1,
        })
    );
}

#[tokio::test]
async fn a_malformed_file_id_is_refused_before_it_is_queued() {
    let (client, _) = telemetry_client(vec![NO_CONTENT]).await;
    assert!(matches!(
        client.record_playback_start("not-a-uuid".to_owned(), None),
        Err(BeamError::BadRequest { .. })
    ));
    assert!(matches!(
        client.record_playback_source_switch(
            telemetry_file(1),
            "nope".to_owned(),
            SwitchTrigger::Auto
        ),
        Err(BeamError::BadRequest { .. })
    ));
}

const TELEMETRY_REPORT: &str = r#"{
    "enabled": false,
    "from": "2026-10-01",
    "to": "2026-10-07",
    "retention_days": 400,
    "totals": { "started_count": 4, "failed_count": 1, "rebuffer_count": 0,
                "rebuffer_total_ms": 0, "source_switch_count": 0 },
    "starts": [{ "client_kind": "android", "container": "mp4", "video_codec": "h264",
                 "audio_codec": "aac", "height_class": "hd", "count": 4 }],
    "start_failures": [{ "client_kind": "android", "reason": "audio_codec",
                         "stage": "preflight", "container": "mp4", "video_codec": "h264",
                         "audio_codec": "dts", "height_class": "hd", "count": 1 }],
    "rebuffers": [],
    "source_switches": []
}"#;

#[tokio::test]
async fn the_admin_telemetry_view_asks_for_the_days_it_was_given() {
    let (client, backend) =
        telemetry_client(vec![(200, "application/json", TELEMETRY_REPORT)]).await;

    let summary = client
        .admin_playback_telemetry(Some("2026-10-01".to_owned()), Some("2026-10-07".to_owned()))
        .await
        .expect("answered");

    let request = backend.recorded().last().cloned().expect("a request");
    assert_eq!(request.url.path(), "/v1/admin/telemetry/playback");
    let query: HashMap<_, _> = request.url.query_pairs().into_owned().collect();
    assert_eq!(query.get("from").map(String::as_str), Some("2026-10-01"));
    assert_eq!(query.get("to").map(String::as_str), Some("2026-10-07"));
    assert!(
        !summary.enabled,
        "counts stay readable after collection is off"
    );
    assert_eq!((summary.started, summary.failed), (4, 1));
    assert_eq!(summary.by_client[0].label, "android");
}

#[tokio::test]
async fn a_day_that_does_not_parse_never_reaches_the_server() {
    let (client, backend) =
        telemetry_client(vec![(200, "application/json", TELEMETRY_REPORT)]).await;
    let sent_before = backend.recorded().len();

    assert!(matches!(
        client
            .admin_playback_telemetry(Some("last tuesday".to_owned()), None)
            .await,
        Err(BeamError::BadRequest { .. })
    ));
    assert_eq!(backend.recorded().len(), sent_before);
}
