use super::*;
use crate::progress::ProgressOutcome;

/// The reason the queue exists: an interrupted session must not lose the
/// resume point. A 401 is not retryable *now*, and used to be dropped for
/// it -- contradicting `SessionExpired`'s own doc comment and both
/// platform reporters, which tell the viewer their place is safe.
#[tokio::test]
async fn a_progress_sample_refused_for_credentials_is_queued_not_dropped() {
    let (client, id) = client_answering(
        401,
        r#"{"type":"about:blank","status":401,"detail":"no session"}"#,
    )
    .await;

    let outcome = client
        .report_progress(uuid::Uuid::nil().to_string(), 120.0, Some(7200.0), true)
        .await
        .expect("a failed send is an outcome, not an error");

    assert_eq!(outcome, ProgressOutcome::Queued { pending: 1 });
    assert_eq!(
        client.pending_progress_count().await.expect("countable"),
        1,
        "the sample is on disk, waiting for the sign-in"
    );
    assert!(
        matches!(
            client.session_state(id).expect("known"),
            SessionState::Expired
        ),
        "a 401 with a session in place is that session expiring"
    );
}

/// A flush is a background call, and a 401 during one is a sign-out: the
/// credential comes off the client and the stored cookie is deleted. It
/// used to answer `Ok(0)` and leave the caller with no way to know.
#[tokio::test]
async fn a_flush_that_expires_the_session_reports_it_rather_than_a_count() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let (client, id, _) = signed_in_client_over(Arc::clone(&storage)).await;
    let file_id = uuid::Uuid::nil().to_string();

    // Queued by the ordinary path, against a failure that leaves the
    // session alone -- so the expiry below is the flush's own doing.
    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(
                503,
                "application/problem+json",
                r#"{"type":"about:blank","status":503,"detail":"restarting"}"#,
            )),
        )
        .expect("the server is registered");
    assert_eq!(
        client
            .report_progress(file_id, 120.0, Some(7200.0), true)
            .await
            .expect("a failed send is an outcome, not an error"),
        ProgressOutcome::Queued { pending: 1 }
    );

    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(
                401,
                "application/problem+json",
                r#"{"type":"about:blank","status":401}"#,
            )),
        )
        .expect("the server is registered");

    let error = client
        .flush_progress()
        .await
        .expect_err("a teardown is not a count");

    assert_eq!(error, BeamError::SessionExpired);
    assert!(matches!(
        client.session_state(id.clone()).expect("known"),
        SessionState::Expired
    ));
    assert!(
        !storage.has_secret(&format!("session/{id}")),
        "the flush really did sign the device out"
    );
    assert_eq!(
        client.pending_progress_count().await.expect("countable"),
        1,
        "and the resume point is still waiting for the sign-in"
    );
}

/// The other half: a request the server refuses on its shape fails
/// identically forever, and a queue slot spent on it never retires --
/// `enqueue` replaces the entry and resets `attempts` every fifteen
/// seconds while the title plays.
#[tokio::test]
async fn a_progress_sample_the_server_refuses_is_dropped() {
    let (client, _) = client_answering(
        400,
        r#"{"type":"about:blank","status":400,"detail":"position_secs must be finite"}"#,
    )
    .await;

    let outcome = client
        .report_progress(uuid::Uuid::nil().to_string(), 120.0, Some(7200.0), true)
        .await
        .expect("a refused send is an outcome, not an error");

    let ProgressOutcome::Dropped { reason } = outcome else {
        panic!("expected the sample to be dropped, got {outcome:?}");
    };
    assert!(reason.contains("position_secs must be finite"), "{reason}");
    assert_eq!(client.pending_progress_count().await.expect("countable"), 0);
}

/// What `PUT /v1/files/{id}/progress` answers: the state the server now
/// holds, which is what `ProgressOutcome::Sent` reports back.
const PROGRESS_ACK: &str = r#"{"played":false,"position_secs":45.0,"play_count":0}"#;

/// Queue one sample for each of [`FILE_A`] then [`FILE_B`], a second
/// apart, against a server that cannot be reached.
async fn queue_two_samples(client: &BeamClient, server_id: &str, clock: &crate::clock::TestClock) {
    answer_with(
        client,
        server_id,
        vec![crate::transport::CannedAnswer::Unreachable],
    );
    assert_eq!(
        client
            .report_progress(FILE_A.to_owned(), 30.0, Some(7200.0), true)
            .await
            .expect("an outcome"),
        ProgressOutcome::Queued { pending: 1 }
    );
    clock.advance_secs(1);
    assert_eq!(
        client
            .report_progress(FILE_B.to_owned(), 60.0, None, true)
            .await
            .expect("an outcome"),
        ProgressOutcome::Queued { pending: 2 }
    );
}

#[tokio::test]
async fn a_flush_sends_every_due_sample_and_empties_the_queue() {
    let (client, id, storage, clock) = clocked_client().await;
    queue_two_samples(&client, &id, &clock).await;
    let backend = answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::respond(
            200,
            JSON,
            PROGRESS_ACK,
        )],
    );

    assert_eq!(client.flush_progress().await.expect("flushed"), 2);

    assert_eq!(client.pending_progress_count().await.expect("countable"), 0);
    assert!(!storage.has_plain(&format!("progress_queue/{id}")));
    let sent = backend.recorded();
    let paths: Vec<&str> = sent.iter().map(|request| request.url.path()).collect();
    assert_eq!(
        paths,
        vec![
            format!("/v1/files/{FILE_A}/progress"),
            format!("/v1/files/{FILE_B}/progress"),
        ],
        "oldest first"
    );
    assert!(
        sent.iter()
            .all(|request| request.method == reqwest::Method::PUT)
    );
    assert_eq!(json_body(&sent[0])["position_secs"], 30.0);
    assert_eq!(json_body(&sent[0])["duration_secs"], 7200.0);
    assert_eq!(json_body(&sent[1])["position_secs"], 60.0);
}

#[tokio::test]
async fn a_flush_stops_at_the_first_failure_and_backs_that_sample_off() {
    let (client, id, storage, clock) = clocked_client().await;
    queue_two_samples(&client, &id, &clock).await;
    let backend = answer_with(
        &client,
        &id,
        vec![
            crate::transport::CannedAnswer::respond(503, PROBLEM, r#"{"type":"about:blank"}"#),
            crate::transport::CannedAnswer::respond(200, JSON, PROGRESS_ACK),
        ],
    );
    let now = clock.now_unix();

    assert_eq!(client.flush_progress().await.expect("not an error"), 0);

    assert_eq!(
        backend.recorded().len(),
        1,
        "the backlog is not sent at a server that just failed"
    );
    let queue = queued(&storage, &id).await;
    assert_eq!(queue.len(), 2, "nothing is lost");
    assert_eq!(queue[0].file_id, FILE_A);
    assert_eq!(queue[0].attempts, 1);
    assert_eq!(
        queue[0].rejections, 0,
        "a 503 is the server failing, not the sample"
    );
    assert_eq!(
        queue[0].not_before_unix,
        now + 2,
        "no Retry-After: the first exponential step"
    );
    assert_eq!(queue[1].file_id, FILE_B);
    assert_eq!(queue[1].attempts, 0, "never attempted");
}

/// An outage longer than any fixed number of retries: every flush meets a
/// server that cannot be reached, and the resume point is still queued --
/// and sent -- once it answers. Eight failures used to retire it, which on
/// the 2 s..64 s backoff is about four and a half minutes of downtime.
#[tokio::test]
async fn a_long_outage_does_not_cost_the_queued_resume_point() {
    let (client, id, storage, clock) = clocked_client().await;
    let backend = answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::Unreachable],
    );
    assert_eq!(
        client
            .report_progress(FILE_A.to_owned(), 30.0, Some(7200.0), true)
            .await
            .expect("an outcome"),
        ProgressOutcome::Queued { pending: 1 }
    );

    let outages = 3 * 8;
    for _ in 0..outages {
        assert_eq!(client.flush_progress().await.expect("not an error"), 0);
        let queue = queued(&storage, &id).await;
        assert_eq!(queue.len(), 1, "an outage never retires the sample");
        assert_eq!(queue[0].rejections, 0);
        clock.advance_secs(queue[0].not_before_unix - clock.now_unix());
    }
    assert_eq!(
        backend.recorded().len(),
        1 + outages,
        "every flush really did try the server"
    );
    assert_eq!(
        queued(&storage, &id).await[0].attempts,
        u32::try_from(outages).expect("small"),
        "each outage still backed the sample off"
    );

    let backend = answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::respond(
            200,
            JSON,
            PROGRESS_ACK,
        )],
    );
    assert_eq!(client.flush_progress().await.expect("flushed"), 1);
    let sent = backend.recorded();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url.path(), format!("/v1/files/{FILE_A}/progress"));
    assert_eq!(json_body(&sent[0])["position_secs"], 30.0);
    assert_eq!(client.pending_progress_count().await.expect("countable"), 0);
}

/// The other side of the same rule: a server that refuses the sample
/// itself counts toward retiring it, so a flush cannot spend the queue's
/// whole retention window resending a request that fails identically.
#[tokio::test]
async fn a_flush_the_server_refuses_counts_toward_retiring_the_sample() {
    let (client, id, storage, clock) = clocked_client().await;
    queue_two_samples(&client, &id, &clock).await;
    answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::respond(
            404,
            PROBLEM,
            r#"{"type":"about:blank","status":404,"detail":"no such file"}"#,
        )],
    );

    assert_eq!(client.flush_progress().await.expect("not an error"), 0);

    let queue = queued(&storage, &id).await;
    assert_eq!(queue[0].file_id, FILE_A);
    assert_eq!(queue[0].attempts, 1);
    assert_eq!(queue[0].rejections, 1);
    assert_eq!(queue[1].rejections, 0, "never attempted");
}

#[tokio::test]
async fn a_429_retry_after_reaches_the_queue() {
    let (client, id, storage, clock) = clocked_client().await;
    queue_two_samples(&client, &id, &clock).await;
    let backend = answer_with(
        &client,
        &id,
        vec![
            crate::transport::CannedAnswer::respond(429, PROBLEM, r#"{"type":"about:blank"}"#)
                .with_header("retry-after", "90"),
            crate::transport::CannedAnswer::respond(200, JSON, PROGRESS_ACK),
        ],
    );
    let now = clock.now_unix();

    assert_eq!(client.flush_progress().await.expect("not an error"), 0);
    let queue = queued(&storage, &id).await;
    assert_eq!(
        queue[0].not_before_unix,
        now + 90,
        "the server's interval, not the blind back-off"
    );

    // The other sample was never attempted, so it is still due; the
    // rate-limited one is not, until the server's interval has passed.
    assert_eq!(client.flush_progress().await.expect("flushed"), 1);
    assert_eq!(
        backend.recorded()[1].url.path(),
        format!("/v1/files/{FILE_B}/progress")
    );
    clock.advance_secs(89);
    assert_eq!(client.flush_progress().await.expect("flushed"), 0);
    assert_eq!(backend.recorded().len(), 2, "not before its interval");
    clock.advance_secs(1);
    assert_eq!(client.flush_progress().await.expect("flushed"), 1);
    assert_eq!(client.pending_progress_count().await.expect("countable"), 0);
}

/// The report path's half of the same contract: a sample that
/// `report_progress` queues because the server rate-limited it waits out the
/// server's `Retry-After`, rather than being due to the very next flush.
#[tokio::test]
async fn a_rate_limited_report_is_queued_behind_its_retry_after() {
    let (client, id, storage, clock) = clocked_client().await;
    let backend = answer_with(
        &client,
        &id,
        vec![
            crate::transport::CannedAnswer::respond(429, PROBLEM, r#"{"type":"about:blank"}"#)
                .with_header("retry-after", "90"),
            crate::transport::CannedAnswer::respond(200, JSON, PROGRESS_ACK),
        ],
    );
    let now = clock.now_unix();

    assert_eq!(
        client
            .report_progress(FILE_A.to_owned(), 30.0, Some(7200.0), true)
            .await
            .expect("a rate-limited send is an outcome, not an error"),
        ProgressOutcome::Queued { pending: 1 }
    );
    let queue = queued(&storage, &id).await;
    assert_eq!(
        queue[0].not_before_unix,
        now + 90,
        "the server's interval, not due at once"
    );
    assert_eq!(queue[0].attempts, 0);

    assert_eq!(client.flush_progress().await.expect("flushed"), 0);
    clock.advance_secs(89);
    assert_eq!(client.flush_progress().await.expect("flushed"), 0);
    assert_eq!(
        backend.recorded().len(),
        1,
        "nothing is sent inside the server's interval"
    );

    clock.advance_secs(1);
    assert_eq!(client.flush_progress().await.expect("flushed"), 1);
    let sent = backend.recorded();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].url.path(), format!("/v1/files/{FILE_A}/progress"));
    assert_eq!(json_body(&sent[1])["position_secs"], 30.0);
    assert_eq!(client.pending_progress_count().await.expect("countable"), 0);
}

/// A queued id that is not a UUID was written by an older build and can
/// never be accepted: it is failed without a request, and stops the flush
/// like any other failure. Backed off, it no longer blocks the next one.
#[tokio::test]
async fn a_queued_sample_with_a_malformed_id_fails_without_a_request() {
    let (client, id, storage, clock) = clocked_client().await;
    let now = clock.now_unix();
    let entries = vec![
        QueuedProgress {
            file_id: "not-a-uuid".to_owned(),
            position_secs: 10.0,
            duration_secs: None,
            captured_at_unix: now - 2,
            attempts: 0,
            rejections: 0,
            not_before_unix: now - 2,
        },
        QueuedProgress {
            file_id: FILE_A.to_owned(),
            position_secs: 20.0,
            duration_secs: None,
            captured_at_unix: now - 1,
            attempts: 0,
            rejections: 0,
            not_before_unix: now - 1,
        },
    ];
    storage
        .put(
            format!("progress_queue/{id}"),
            serde_json::to_string(&entries).expect("encodes"),
        )
        .await
        .expect("writable");
    let backend = answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::respond(
            200,
            JSON,
            PROGRESS_ACK,
        )],
    );

    assert_eq!(client.flush_progress().await.expect("not an error"), 0);
    assert!(backend.recorded().is_empty(), "nothing reached the wire");
    let queue = queued(&storage, &id).await;
    assert_eq!(queue[0].file_id, "not-a-uuid");
    assert_eq!(queue[0].attempts, 1);
    assert_eq!(queue[0].rejections, 1, "an id that can never be valid");
    assert_eq!(queue[1].attempts, 0);

    assert_eq!(client.flush_progress().await.expect("flushed"), 1);
    assert_eq!(
        backend.recorded()[0].url.path(),
        format!("/v1/files/{FILE_A}/progress")
    );
}

#[tokio::test]
async fn a_sent_sample_retires_its_queued_entry() {
    let (client, id, storage, _clock) = clocked_client().await;
    answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::Unreachable],
    );
    client
        .report_progress(FILE_A.to_owned(), 30.0, None, true)
        .await
        .expect("queued");
    assert_eq!(queued(&storage, &id).await.len(), 1);
    answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::respond(
            200,
            JSON,
            PROGRESS_ACK,
        )],
    );

    let outcome = client
        .report_progress(FILE_A.to_owned(), 45.0, None, true)
        .await
        .expect("sent");

    assert_eq!(
        outcome,
        ProgressOutcome::Sent {
            position_secs: 45.0
        }
    );
    assert!(
        queued(&storage, &id).await.is_empty(),
        "the newer position supersedes the queued one"
    );
}

#[tokio::test]
async fn a_held_sample_is_throttled_at_the_facade() {
    let (client, id, _storage, clock) = clocked_client().await;
    let backend = answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::respond(
            200,
            JSON,
            PROGRESS_ACK,
        )],
    );
    client
        .report_progress(FILE_A.to_owned(), 10.0, None, false)
        .await
        .expect("sent");
    clock.advance_secs(4);

    let held = client
        .report_progress(FILE_A.to_owned(), 14.0, Some(7200.0), false)
        .await
        .expect("an outcome");

    assert_eq!(
        held,
        ProgressOutcome::Throttled {
            next_eligible_in_secs: 11
        }
    );
    assert_eq!(backend.recorded().len(), 1, "a held sample sends nothing");

    // A forced report goes out at once, at its own position, with the
    // duration only the held sample knew.
    client
        .report_progress(FILE_A.to_owned(), 15.0, None, true)
        .await
        .expect("sent");
    let sent = backend.recorded();
    assert_eq!(sent.len(), 2);
    assert_eq!(json_body(&sent[1])["position_secs"], 15.0);
    assert_eq!(json_body(&sent[1])["duration_secs"], 7200.0);
}

#[tokio::test]
async fn an_unreachable_server_queues_progress_and_reports_network() {
    let (client, id, storage, _clock) = clocked_client().await;
    answer_with(
        &client,
        &id,
        vec![crate::transport::CannedAnswer::Unreachable],
    );

    assert!(matches!(
        client.genres().await,
        Err(BeamError::Network {
            retryable: true,
            ..
        })
    ));
    assert_eq!(
        client
            .report_progress(FILE_A.to_owned(), 30.0, None, true)
            .await
            .expect("an outcome, not an error"),
        ProgressOutcome::Queued { pending: 1 }
    );
    assert_eq!(queued(&storage, &id).await[0].position_secs, 30.0);
    assert!(
        matches!(
            client.session_state(id).expect("known"),
            SessionState::Authenticated { .. }
        ),
        "an outage is not a sign-out"
    );
}
