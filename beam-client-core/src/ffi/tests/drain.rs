//! `drain_progress`: every server's queue, to its own server.

use super::progress::PROGRESS_ACK;
use super::*;
use crate::progress::{ProgressDrain, ProgressOutcome};

/// Two signed-in servers over one clock and one store. `cabin`, added last,
/// is the active one.
struct TwoServers {
    client: Arc<BeamClient>,
    home: String,
    cabin: String,
    storage: Arc<InMemoryKeyValueStore>,
    clock: Arc<crate::clock::TestClock>,
}

async fn two_servers() -> TwoServers {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let clock = Arc::new(crate::clock::TestClock::new(1_000_000));
    let client = BeamClient::over(
        Arc::clone(&storage) as Arc<dyn KeyValueStore>,
        Arc::clone(&clock) as Arc<dyn Clock>,
    );
    let home = sign_in_to(&client, "https://home.beam.test", "home-cookie").await;
    let cabin = sign_in_to(&client, "https://cabin.beam.test", "cabin-cookie").await;
    TwoServers {
        client,
        home,
        cabin,
        storage,
        clock,
    }
}

/// Queue a due sample for `file_id` with `server_id`, through the ordinary
/// report path against a server that cannot be reached.
async fn queue_offline(client: &BeamClient, server_id: &str, file_id: &str, position: f64) {
    client
        .select_server(server_id.to_owned())
        .await
        .expect("known");
    answer_with(
        client,
        server_id,
        vec![crate::transport::CannedAnswer::Unreachable],
    );
    assert!(matches!(
        client
            .report_progress(file_id.to_owned(), position, None, true)
            .await
            .expect("an outcome"),
        ProgressOutcome::Queued { .. }
    ));
}

fn acknowledging(client: &BeamClient, server_id: &str) -> Arc<CannedBackend> {
    answer_with(
        client,
        server_id,
        vec![crate::transport::CannedAnswer::respond(
            200,
            JSON,
            PROGRESS_ACK,
        )],
    )
}

/// The reason `drain_progress` exists: a position recorded offline against
/// one server reaches that server, with that server's session, after the
/// viewer has switched to another -- not the active one, and not never.
#[tokio::test]
async fn a_drain_sends_each_servers_queue_to_that_server_whichever_is_active() {
    let TwoServers {
        client,
        home,
        cabin,
        storage,
        clock: _,
    } = two_servers().await;
    queue_offline(&client, &home, FILE_A, 30.0).await;
    queue_offline(&client, &cabin, FILE_B, 60.0).await;
    let home_backend = acknowledging(&client, &home);
    let cabin_backend = acknowledging(&client, &cabin);

    let drain = client.drain_progress().await.expect("drained");

    assert_eq!(
        drain,
        ProgressDrain {
            sent: 2,
            pending: 0,
            retry_in_secs: None,
            expired_server_ids: Vec::new(),
        }
    );
    let to_home = home_backend.recorded();
    assert_eq!(to_home.len(), 1);
    assert_eq!(to_home[0].url.host_str(), Some("home.beam.test"));
    assert_eq!(
        to_home[0].url.path(),
        format!("/v1/files/{FILE_A}/progress")
    );
    assert_eq!(cookies_of(&to_home[0]), vec!["beam_session=home-cookie"]);
    assert_eq!(json_body(&to_home[0])["position_secs"], 30.0);
    let to_cabin = cabin_backend.recorded();
    assert_eq!(to_cabin.len(), 1);
    assert_eq!(
        to_cabin[0].url.path(),
        format!("/v1/files/{FILE_B}/progress")
    );
    assert_eq!(cookies_of(&to_cabin[0]), vec!["beam_session=cabin-cookie"]);
    assert!(queued(&storage, &home).await.is_empty());
    assert!(queued(&storage, &cabin).await.is_empty());
}

/// One server down does not hold back the other, and the answer names when
/// the down one's failed entry falls due -- the core's back-off, which the
/// platform waits out instead of keeping a schedule of its own.
#[tokio::test]
async fn a_down_server_backs_off_while_the_other_drains_and_names_the_retry() {
    let TwoServers {
        client,
        home,
        cabin,
        storage,
        clock,
    } = two_servers().await;
    queue_offline(&client, &home, FILE_A, 30.0).await;
    queue_offline(&client, &home, FILE_B, 40.0).await;
    queue_offline(&client, &cabin, FILE_B, 60.0).await;
    let home_backend = answer_with(
        &client,
        &home,
        vec![crate::transport::CannedAnswer::Unreachable],
    );
    acknowledging(&client, &cabin);

    let drain = client
        .drain_progress()
        .await
        .expect("an outage is no error");

    assert_eq!(drain.sent, 1, "cabin took its sample");
    assert_eq!(drain.pending, 2, "both of home's wait");
    assert_eq!(
        drain.retry_in_secs,
        Some(2),
        "the failed entry's first back-off step, though the entry behind it is still due"
    );
    assert_eq!(
        home_backend.recorded().len(),
        1,
        "stopped at the first failure"
    );
    assert_eq!(
        queued(&storage, &home).await[0].not_before_unix,
        clock.now_unix() + 2
    );

    // A second failure doubles the wait, and the answer follows it.
    clock.advance_secs(2);
    let drain = client
        .drain_progress()
        .await
        .expect("an outage is no error");
    assert_eq!(drain.retry_in_secs, Some(4));

    clock.advance_secs(4);
    acknowledging(&client, &home);
    let drain = client.drain_progress().await.expect("drained");
    assert_eq!(
        (drain.sent, drain.pending, drain.retry_in_secs),
        (2, 0, None)
    );
}

/// A server's `Retry-After` is the wait the answer names, not the blind
/// back-off.
#[tokio::test]
async fn a_rate_limited_drain_retries_after_the_servers_interval() {
    let TwoServers {
        client,
        home,
        cabin: _,
        storage: _,
        clock: _,
    } = two_servers().await;
    queue_offline(&client, &home, FILE_A, 30.0).await;
    answer_with(
        &client,
        &home,
        vec![
            crate::transport::CannedAnswer::respond(429, PROBLEM, r#"{"type":"about:blank"}"#)
                .with_header("retry-after", "90"),
        ],
    );

    let drain = client
        .drain_progress()
        .await
        .expect("a rate limit is no error");

    assert_eq!(drain.retry_in_secs, Some(90));
    assert_eq!(drain.pending, 1);
}

/// A refused session signs that server out and is named, not thrown: the
/// other server still drains, and the refused one's samples wait for the
/// sign-in rather than for a retry.
#[tokio::test]
async fn a_refused_session_is_named_and_the_other_server_still_drains() {
    let TwoServers {
        client,
        home,
        cabin,
        storage,
        clock: _,
    } = two_servers().await;
    queue_offline(&client, &home, FILE_A, 30.0).await;
    queue_offline(&client, &cabin, FILE_B, 60.0).await;
    answer_with(
        &client,
        &home,
        vec![crate::transport::CannedAnswer::respond(
            401,
            PROBLEM,
            r#"{"type":"about:blank","status":401}"#,
        )],
    );
    let cabin_backend = acknowledging(&client, &cabin);

    let drain = client
        .drain_progress()
        .await
        .expect("an expiry is named, not thrown");

    assert_eq!(drain.expired_server_ids, vec![home.clone()]);
    assert_eq!(drain.sent, 1);
    assert_eq!(drain.pending, 1);
    assert_eq!(
        drain.retry_in_secs, None,
        "nothing is retried towards a refused session"
    );
    assert_eq!(cabin_backend.recorded().len(), 1);
    assert!(matches!(
        client.session_state(home.clone()).expect("known"),
        SessionState::Expired
    ));
    assert!(
        !storage.has_secret(&format!("session/{home}")),
        "the drain really did sign that server out"
    );
    assert_eq!(
        queued(&storage, &home).await.len(),
        1,
        "the resume point waits"
    );

    // Signed out, it is left alone: no request, and still no retry.
    let home_backend = acknowledging(&client, &home);
    let drain = client.drain_progress().await.expect("drained");
    assert!(home_backend.recorded().is_empty());
    assert_eq!(
        (drain.sent, drain.pending, drain.retry_in_secs),
        (0, 1, None)
    );
}

/// A queue that cannot be read fails the drain outright, and nothing was
/// backed off -- so the platform waits on the core's own schedule.
#[tokio::test]
async fn a_drain_over_an_unreadable_queue_fails_and_the_retry_is_the_queues_schedule() {
    let TwoServers {
        client,
        home,
        cabin: _,
        storage,
        clock: _,
    } = two_servers().await;
    queue_offline(&client, &home, FILE_A, 30.0).await;
    storage.set_failure(FailureMode::FailReads);

    assert!(matches!(
        client.drain_progress().await,
        Err(BeamError::Storage { .. })
    ));
    let delays: Vec<u64> = (0..9)
        .map(crate::progress::progress_retry_delay_secs)
        .collect();
    assert_eq!(delays, vec![2, 2, 4, 8, 16, 32, 64, 64, 64]);
}

mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// Draining is idempotent: whatever was queued and however the
        /// servers answered, a second drain at the same instant sends
        /// nothing, asks nothing more of a server that answered, and leaves
        /// as much queued. (A server that did not answer is asked again for
        /// the next entry behind the one that failed, exactly as a flush
        /// does -- which is why the answer names a wait.)
        #[test]
        fn draining_twice_sends_nothing_the_second_time(
            samples in proptest::collection::vec((any::<bool>(), 0_u8..4), 0..8),
            home_up in any::<bool>(),
            cabin_up in any::<bool>(),
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime");
            runtime.block_on(async {
                let TwoServers { client, home, cabin, storage: _, clock: _ } = two_servers().await;
                for (to_home, file) in &samples {
                    let server = if *to_home { &home } else { &cabin };
                    let file_id = format!("{file:08x}-0000-4000-8000-000000000000");
                    queue_offline(&client, server, &file_id, 10.0).await;
                }
                let answer = |up: bool| if up {
                    crate::transport::CannedAnswer::respond(200, JSON, PROGRESS_ACK)
                } else {
                    crate::transport::CannedAnswer::Unreachable
                };
                let home_backend = answer_with(&client, &home, vec![answer(home_up)]);
                let cabin_backend = answer_with(&client, &cabin, vec![answer(cabin_up)]);

                let first = client.drain_progress().await.expect("drained");
                let answered_wire = |up: bool, backend: &CannedBackend| {
                    if up { backend.recorded().len() } else { 0 }
                };
                let on_the_wire = answered_wire(home_up, &home_backend)
                    + answered_wire(cabin_up, &cabin_backend);
                let second = client.drain_progress().await.expect("drained");

                prop_assert_eq!(second.sent, 0);
                prop_assert_eq!(
                    answered_wire(home_up, &home_backend) + answered_wire(cabin_up, &cabin_backend),
                    on_the_wire
                );
                prop_assert_eq!(second.pending, first.pending);
                prop_assert_eq!(first.retry_in_secs.is_none(), first.pending == 0);
                prop_assert_eq!(second.retry_in_secs.is_none(), second.pending == 0);
                Ok(())
            })?;
        }
    }
}
