//! Subcutaneous tests for the token bucket behind [`BeamRateLimit`].
//!
//! Kynos owns the 429, the `Retry-After` and the `X-RateLimit-*` triple; Beam
//! owns the bucket and the key. So these drive a real `RateLimit` interceptor
//! over a one-route router and assert what a client actually receives, which
//! is the only place the two halves meet.
//!
//! Time moves through `AppState::clock()` -- a `TestClock` the harness holds a
//! handle to -- so the test that proves a bucket *refills* moves the clock
//! rather than sleeping (AGENTS.md's canonical time seam).
//!
//! The client is set with `TestRequest::peer`, not with `X-Forwarded-For`:
//! Kynos resolves the client through the router's trusted-proxy policy, and
//! this router trusts nobody.

use std::sync::Arc;
use std::time::Duration;

use beam_domain::services::TestClock;
use kynos::extract::body::text::Text;
use kynos::http::StatusCode;
use kynos::middleware::rate_limit::RateLimit;
use kynos::prelude::*;
use kynos::test::{TestClient, TestResponse};

use super::{BeamRateLimit, Class};
use crate::config::ServerConfig;
use crate::routes::test_support::make_app_state_with_clock;
use crate::state::AppState;

/// The socket every request arrives on unless a test says otherwise.
const CLIENT: &str = "203.0.113.7:41000";

/// A second socket, for the tests about who shares a bucket with whom.
const OTHER_CLIENT: &str = "198.51.100.9:41000";

/// A route that does nothing, so the only thing under test is the limiter.
#[kynos::get("/probe", operation_id = "rateLimitProbe")]
async fn probe() -> Text {
    Text("ok".to_owned())
}

/// A limiter plus a handle to its clock, so a test can both drive requests and
/// move time.
struct Harness {
    client: TestClient<AppState>,
    clock: Arc<TestClock>,
}

/// One limiter of `class`, over a configuration the caller writes.
fn harness(class: Class, adjust: impl FnOnce(&mut ServerConfig)) -> Harness {
    let clock = Arc::new(TestClock::new());
    let service = Router::new()
        .mount(kynos::routes![probe])
        .intercept(RateLimit::new(BeamRateLimit::new(class)))
        .build(make_app_state_with_clock(adjust, clock.clone()))
        .expect("the probe router describes itself");

    Harness {
        client: TestClient::new(service),
        clock,
    }
}

/// An auth-class limiter with `per_minute` of burst.
///
/// The search ceiling is set far apart so a limiter reading the wrong key
/// would be visible rather than coincidentally correct.
fn auth(per_minute: u32) -> Harness {
    harness(Class::Auth, |config| {
        config.rate_limit_auth_per_minute = per_minute;
        config.rate_limit_search_per_minute = 1_000;
    })
}

/// One GET from `peer`.
async fn get(client: &TestClient<AppState>, peer: &str) -> TestResponse {
    client
        .get("/probe")
        .peer(peer.parse().expect("a socket address a test wrote"))
        .send()
        .await
}

/// The value of one `X-RateLimit-*` field, as the number it is.
fn field(response: &TestResponse, name: &str) -> u64 {
    response
        .header(name)
        .unwrap_or_else(|| panic!("`{name}` must be on every rate-limited response"))
        .parse()
        .unwrap_or_else(|_| panic!("`{name}` must be a whole number"))
}

#[tokio::test]
async fn a_burst_is_allowed_and_then_refused_with_a_problem_document() {
    // Burst capacity == 3; the 4th request in the same instant is refused.
    let Harness { client, .. } = auth(3);

    for spent in 0..3 {
        let response = get(&client, CLIENT).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "request {spent} is within the burst"
        );
        assert_eq!(
            field(&response, "x-ratelimit-remaining"),
            2 - spent,
            "each allowed request must spend exactly one token"
        );
        assert_eq!(field(&response, "x-ratelimit-limit"), 3);
    }

    let response = get(&client, CLIENT).await;
    response.assert_status(StatusCode::TOO_MANY_REQUESTS);

    // Retry-After is a positive whole number of seconds, and the triple agrees
    // with it: nothing left, and the window resets when the retry says.
    let retry_after = field(&response, "retry-after");
    assert!(retry_after >= 1, "Retry-After should be at least 1s");
    assert_eq!(field(&response, "x-ratelimit-remaining"), 0);
    assert_eq!(field(&response, "x-ratelimit-limit"), 3);
    assert_eq!(field(&response, "x-ratelimit-reset"), retry_after);

    // The refusal is RFC 9457, not the `{"error": ...}` shape Salvo's handler
    // wrote. Kynos renders it, so the type is the unidentified default.
    //
    // Both halves of this pin a gap rather than a preference, and both are
    // getkono/kynos#104. Beam cannot supply a `type` here -- `RateLimitPolicy`
    // returns a `Decision`, which carries nowhere to put one -- and the 429
    // that kynos *declares* has no content at all, while the one it sends is
    // the problem document asserted below. The document therefore misdescribes
    // this response, which is the failure ADR-0010 exists to prevent, and
    // there is nothing to fix locally that would not be the hand-written
    // exception AGENTS.md rule 3 forbids.
    //
    // These assertions are expected to fail when the issue is fixed. That is
    // the point: the failure is the reminder to name this response.
    response.assert_problem_type("about:blank");
    assert_eq!(
        response.header("content-type"),
        Some("application/problem+json")
    );
    let document: serde_json::Value = response.json();
    assert_eq!(document["status"], 429);
}

#[tokio::test]
async fn distinct_clients_get_independent_budgets() {
    // Capacity 1: each distinct client gets its own single token.
    let Harness { client, .. } = auth(1);

    assert_eq!(get(&client, CLIENT).await.status(), StatusCode::OK);
    assert_eq!(
        get(&client, CLIENT).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    assert_eq!(
        get(&client, OTHER_CLIENT).await.status(),
        StatusCode::OK,
        "a second client is unaffected by the first's exhaustion"
    );
}

/// The security property the old per-limiter `trust_forwarded_for` flag could
/// not provide: the bucket is chosen by the router's trusted-proxy policy, and
/// this router trusts nobody, so a client cannot name its own bucket.
#[tokio::test]
async fn an_untrusted_forwarded_for_cannot_buy_a_fresh_budget() {
    let Harness { client, .. } = auth(1);

    let first = client
        .get("/probe")
        .peer(CLIENT.parse().unwrap())
        .header("X-Forwarded-For", "1.1.1.1")
        .send()
        .await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = client
        .get("/probe")
        .peer(CLIENT.parse().unwrap())
        .header("X-Forwarded-For", "2.2.2.2")
        .send()
        .await;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a spoofed X-Forwarded-For must not pick a bucket while no proxy is trusted"
    );
}

#[tokio::test]
async fn a_blocked_client_recovers_after_the_clock_advances() {
    // Capacity 1, refill 1/60 per second. Spend the token, get blocked, then
    // advance a full minute so exactly one token has accrued.
    let Harness { client, clock } = auth(1);

    assert_eq!(get(&client, CLIENT).await.status(), StatusCode::OK);
    assert_eq!(
        get(&client, CLIENT).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    clock.advance(Duration::from_secs(60));

    assert_eq!(
        get(&client, CLIENT).await.status(),
        StatusCode::OK,
        "a full token should have refilled after 60s"
    );
}

#[tokio::test]
async fn a_partial_wait_refills_proportionally_rather_than_wholesale() {
    // The refill is `elapsed * refill_per_sec`. The recovery test above waits a
    // full window, where the result is clamped to capacity either way -- so it
    // cannot tell that multiplication from an addition. Waiting for *part* of a
    // window is what distinguishes them: 30s at 10/minute is exactly 5 tokens,
    // not 10.
    let Harness { client, clock } = auth(10);

    for spent in 0..10 {
        assert_eq!(
            get(&client, CLIENT).await.status(),
            StatusCode::OK,
            "burst request {spent} is within capacity"
        );
    }
    assert_eq!(
        get(&client, CLIENT).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    clock.advance(Duration::from_secs(30));

    for spent in 0..5 {
        assert_eq!(
            get(&client, CLIENT).await.status(),
            StatusCode::OK,
            "half a window refills half the budget; request {spent} of 5"
        );
    }
    assert_eq!(
        get(&client, CLIENT).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the sixth request is past what 30 seconds bought"
    );
}

#[tokio::test]
async fn retry_after_reflects_the_wait_for_a_whole_token() {
    // A client that honours `Retry-After` must not come back to another 429.
    // At 1/minute an exhausted bucket needs the full 60 seconds.
    let Harness { client, clock } = auth(1);
    assert_eq!(get(&client, CLIENT).await.status(), StatusCode::OK);

    let response = get(&client, CLIENT).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = field(&response, "retry-after");
    assert_eq!(retry_after, 60);

    clock.advance(Duration::from_secs(retry_after - 1));
    assert_eq!(
        get(&client, CLIENT).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "one second early is still too early"
    );
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        get(&client, CLIENT).await.status(),
        StatusCode::OK,
        "waiting exactly as long as instructed must work"
    );
}

/// The two classes exist only to name which config key supplies `per_minute`,
/// so the thing worth asserting is that each reads its own.
#[tokio::test]
async fn each_class_enforces_its_own_configured_ceiling() {
    let auth = harness(Class::Auth, |config| {
        config.rate_limit_auth_per_minute = 1;
        config.rate_limit_search_per_minute = 1_000;
    });
    assert_eq!(get(&auth.client, CLIENT).await.status(), StatusCode::OK);
    assert_eq!(
        get(&auth.client, CLIENT).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the auth class must spend the auth budget, not the search one"
    );

    let search = harness(Class::Search, |config| {
        config.rate_limit_auth_per_minute = 1_000;
        config.rate_limit_search_per_minute = 1;
    });
    assert_eq!(get(&search.client, CLIENT).await.status(), StatusCode::OK);
    assert_eq!(
        get(&search.client, CLIENT).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the search class must spend the search budget, not the auth one"
    );
}

#[tokio::test]
async fn separate_limiter_instances_keep_separate_state() {
    // As mounted: one instance per group, so exhausting the budget on one
    // group leaves the other untouched even at the same ceiling.
    let first = auth(1);
    let second = auth(1);

    assert_eq!(get(&first.client, CLIENT).await.status(), StatusCode::OK);
    assert_eq!(
        get(&first.client, CLIENT).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    assert_eq!(get(&second.client, CLIENT).await.status(), StatusCode::OK);
}

/// `BEAM_RATE_LIMIT_ENABLED=false` is a runtime exemption rather than an
/// unmounted interceptor, so the 429 stays declared and the headers stay on
/// the wire -- reporting the full quota, every request, past the ceiling.
#[tokio::test]
async fn disabling_the_limiter_allows_past_the_ceiling_and_reports_a_full_quota() {
    let Harness { client, .. } = harness(Class::Auth, |config| {
        config.rate_limit_enabled = false;
        config.rate_limit_auth_per_minute = 1;
    });

    for attempt in 0..5 {
        let response = get(&client, CLIENT).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "request {attempt} is past a ceiling nobody is enforcing"
        );
        assert_eq!(
            field(&response, "x-ratelimit-remaining"),
            field(&response, "x-ratelimit-limit"),
            "an exempt request spends nothing, so the quota stays full"
        );
    }
}
