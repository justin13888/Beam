//! `await_device_login`: RFC 8628's client-side pacing, driven by a scripted
//! token endpoint and a test clock, so every wait is asserted and none is real.

use super::*;
use crate::clock::TestClock;
use crate::transport::CannedAnswer;

const PROBLEM: &str = "application/problem+json";
const JSON: &str = "application/json";
const PENDING_5: &str = r#"{"status":"authorization_pending","interval_secs":5}"#;
const PENDING_10: &str = r#"{"status":"authorization_pending","interval_secs":10}"#;
const SLOW_DOWN_10: &str = r#"{"status":"slow_down","interval_secs":10}"#;
const DENIED: &str = r#"{"type":"https://beam.justinchung.net/reference/errors/#device-login-denied","status":403,"detail":"refused"}"#;
const EXPIRED: &str = r#"{"type":"https://beam.justinchung.net/reference/errors/#device-login-expired","status":410,"detail":"expired"}"#;
const UNAVAILABLE: &str = r#"{"type":"https://beam.justinchung.net/reference/errors/#oidc-unavailable","status":503,"detail":"down"}"#;

fn pending_5() -> CannedAnswer {
    CannedAnswer::respond(202, JSON, PENDING_5)
}

fn approved() -> Vec<CannedAnswer> {
    vec![
        CannedAnswer::respond(200, JSON, DEVICE_APPROVED),
        CannedAnswer::respond(200, JSON, ME),
    ]
}

/// The prompt `start_device_login` would hand back: five-second interval,
/// ten-minute lifetime.
fn prompt(expires_in_secs: u64) -> DeviceLoginPrompt {
    DeviceLoginPrompt {
        device_handle: "handle-1".to_owned(),
        user_code: "BCDF-GHJK".to_owned(),
        verification_uri: "https://idp.test/device".to_owned(),
        verification_uri_complete: None,
        expires_in_secs,
        interval_secs: 5,
    }
}

/// A registered, signed-out server over `answers` and a test clock.
async fn clocked_device_client(
    answers: Vec<CannedAnswer>,
) -> (Arc<BeamClient>, String, Arc<CannedBackend>, Arc<TestClock>) {
    let clock = Arc::new(TestClock::default());
    let client = BeamClient::over(
        Arc::new(InMemoryKeyValueStore::new()),
        Arc::clone(&clock) as Arc<dyn Clock>,
    );
    let id = client
        .add_server("https://beam.test".to_owned(), None)
        .await
        .expect("added")
        .id;
    let backend = Arc::new(CannedBackend::saying(answers));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");
    (client, id, backend, clock)
}

fn signed_in_as_ada(outcome: &Result<UserSummary, BeamError>) -> bool {
    matches!(outcome, Ok(user) if user.id == "00000000-0000-0000-0000-0000000000a1")
}

fn expired(outcome: &Result<UserSummary, BeamError>) -> bool {
    matches!(
        outcome,
        Err(BeamError::Server { status: 410, retryable: false, code, .. })
            if code.ends_with("#device-login-expired")
    )
}

fn denied(outcome: &Result<UserSummary, BeamError>) -> bool {
    matches!(outcome, Err(BeamError::Forbidden { code, .. }) if code.ends_with("#device-login-denied"))
}

/// Every token-endpoint script, the waits it must produce, how many polls it
/// takes, and how the login ends.
#[tokio::test]
async fn the_poll_loop_waits_what_rfc_8628_asks_and_ends_where_the_server_says() {
    type Outcome = fn(&Result<UserSummary, BeamError>) -> bool;
    /// The case, the endpoint's answers, the waits, the polls, the ending.
    type Row = (&'static str, Vec<CannedAnswer>, Vec<u64>, usize, Outcome);
    let rows: Vec<Row> = vec![
        (
            "pending, slow_down, pending, approved: slow_down stretches every later wait",
            [
                vec![
                    pending_5(),
                    CannedAnswer::respond(202, JSON, SLOW_DOWN_10),
                    CannedAnswer::respond(202, JSON, PENDING_10),
                ],
                approved(),
            ]
            .concat(),
            vec![5, 5, 10, 10],
            4,
            signed_in_as_ada,
        ),
        (
            "a slow_down whose interval did not grow still adds five seconds",
            [
                vec![
                    CannedAnswer::respond(202, JSON, r#"{"status":"slow_down","interval_secs":5}"#),
                    pending_5(),
                ],
                approved(),
            ]
            .concat(),
            vec![5, 10, 10],
            3,
            signed_in_as_ada,
        ),
        (
            "pending, slow_down, pending, expired",
            vec![
                pending_5(),
                CannedAnswer::respond(202, JSON, SLOW_DOWN_10),
                CannedAnswer::respond(202, JSON, PENDING_10),
                CannedAnswer::respond(410, PROBLEM, EXPIRED),
            ],
            vec![5, 5, 10, 10],
            4,
            expired,
        ),
        (
            "pending, denied",
            vec![pending_5(), CannedAnswer::respond(403, PROBLEM, DENIED)],
            vec![5, 5],
            2,
            denied,
        ),
        (
            "a rate limit waits its Retry-After when that is longer",
            [
                vec![
                    CannedAnswer::respond(429, "text/plain", "slow")
                        .with_header("retry-after", "30"),
                ],
                approved(),
            ]
            .concat(),
            vec![5, 30],
            2,
            signed_in_as_ada,
        ),
        (
            "a rate limit never waits less than the interval",
            [
                vec![
                    CannedAnswer::respond(429, "text/plain", "slow")
                        .with_header("retry-after", "1"),
                ],
                approved(),
            ]
            .concat(),
            vec![5, 5],
            2,
            signed_in_as_ada,
        ),
        (
            "a retryable server error and a lost connection keep the login",
            [
                vec![
                    CannedAnswer::respond(503, PROBLEM, UNAVAILABLE),
                    CannedAnswer::Unreachable,
                ],
                approved(),
            ]
            .concat(),
            vec![5, 5, 5],
            3,
            signed_in_as_ada,
        ),
    ];

    for (case, answers, waits, polls, outcome) in rows {
        let (client, id, backend, clock) = clocked_device_client(answers).await;

        let result = client.await_device_login(id.clone(), prompt(600)).await;

        assert!(outcome(&result), "{case}: ended {result:?}");
        assert_eq!(clock.sleeps(), waits, "{case}: waits");
        let token_polls = backend
            .recorded()
            .iter()
            .filter(|request| request.url.path() == "/v1/auth/device/token")
            .count();
        assert_eq!(token_polls, polls, "{case}: polls");
        assert_eq!(
            matches!(
                client.session_state(id).expect("known"),
                SessionState::Authenticated { .. }
            ),
            result.is_ok(),
            "{case}: signed in exactly when it says so"
        );
    }
}

/// A flow the server cannot be reached to end still ends at the prompt's
/// lifetime, by this device's clock, rather than polling a dead connection
/// forever.
#[tokio::test]
async fn an_unanswered_login_ends_when_the_prompt_expires() {
    let (client, id, backend, clock) = clocked_device_client(vec![CannedAnswer::Unreachable]).await;

    let result = client.await_device_login(id, prompt(12)).await;

    assert!(expired(&result), "{result:?}");
    assert_eq!(clock.sleeps(), vec![5, 5, 5]);
    assert_eq!(
        backend.recorded().len(),
        2,
        "polled at 5 and 10 seconds, not once the 12-second code had expired"
    );
}

/// Dropping the future -- what a cancelled Kotlin coroutine does -- stops the
/// polling: no request follows. Real timer, paused time.
#[tokio::test(start_paused = true)]
async fn dropping_the_wait_stops_polling() {
    let (client, id, backend, _storage) =
        device_client(CannedBackend::answering(202, JSON, PENDING_5)).await;

    let waited = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        client.await_device_login(id, prompt(600)),
    )
    .await;
    assert!(waited.is_err(), "still pending when abandoned");
    assert_eq!(backend.recorded().len(), 2, "polled at 5 and 10 seconds");

    tokio::time::sleep(std::time::Duration::from_secs(60)).await;

    assert_eq!(backend.recorded().len(), 2, "and never again");
}
