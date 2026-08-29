use std::sync::Arc;
use std::time::Duration;

use confique::Config as _;
use salvo::http::header::RETRY_AFTER;
use salvo::prelude::*;
use salvo::test::{ResponseExt, TestClient};

use super::RateLimiter;
use crate::routes::api_error::ApiErrorBody;
use beam_domain::services::TestClock;

#[handler]
async fn ok_handler() -> &'static str {
    "ok"
}

/// A limiter plus a handle to its clock, so a test can both drive requests and
/// advance time.
struct Harness {
    service: Service,
    clock: Arc<TestClock>,
}

fn harness(per_minute: u32, trust_forwarded_for: bool) -> Harness {
    let clock = Arc::new(TestClock::new());
    let limiter = RateLimiter::new(per_minute, trust_forwarded_for, clock.clone());
    let router = Router::new().hoop(limiter).goal(ok_handler);
    Harness {
        service: Service::new(router),
        clock,
    }
}

/// Drives one GET, optionally spoofing an `X-Forwarded-For` client.
async fn get(service: &Service, forwarded_for: Option<&str>) -> Response {
    let mut req = TestClient::get("http://localhost/");
    if let Some(xff) = forwarded_for {
        req = req.add_header("X-Forwarded-For", xff, true);
    }
    req.send(service).await
}

#[tokio::test]
async fn auth_limiter_allows_burst_then_rejects_with_retry_after_and_body() {
    // Burst capacity == 3; the 4th request in the same instant is rejected.
    let Harness { service, .. } = harness(3, false);

    for i in 0..3 {
        let res = get(&service, None).await;
        assert_eq!(
            res.status_code,
            Some(StatusCode::OK),
            "request {i} should pass"
        );
    }

    let mut res = get(&service, None).await;
    assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));

    // Retry-After is present and a positive whole number of seconds.
    let retry_after = res
        .headers()
        .get(RETRY_AFTER)
        .expect("429 must carry a Retry-After header")
        .to_str()
        .unwrap()
        .parse::<u64>()
        .expect("Retry-After must be an integer number of seconds");
    assert!(retry_after >= 1, "Retry-After should be at least 1s");

    // Body is the shared ApiError JSON shape.
    let body: ApiErrorBody = res.take_json().await.unwrap();
    assert_eq!(body.error, "Rate limit exceeded");
}

#[tokio::test]
async fn distinct_forwarded_for_clients_get_independent_budgets() {
    // Trust XFF, capacity 1: each distinct client gets its own single token.
    let Harness { service, .. } = harness(1, true);

    // Client A spends its token, then is blocked.
    assert_eq!(
        get(&service, Some("1.1.1.1")).await.status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        get(&service, Some("1.1.1.1")).await.status_code,
        Some(StatusCode::TOO_MANY_REQUESTS)
    );

    // Client B is unaffected by A's exhaustion.
    assert_eq!(
        get(&service, Some("2.2.2.2")).await.status_code,
        Some(StatusCode::OK)
    );
}

#[tokio::test]
async fn blocked_client_recovers_after_clock_advances() {
    // Capacity 1, refill 1/60 per second. Spend the token, get blocked, then
    // advance a full minute so exactly one token has accrued.
    let Harness { service, clock } = harness(1, false);

    assert_eq!(get(&service, None).await.status_code, Some(StatusCode::OK));
    assert_eq!(
        get(&service, None).await.status_code,
        Some(StatusCode::TOO_MANY_REQUESTS)
    );

    clock.advance(Duration::from_secs(60));

    assert_eq!(
        get(&service, None).await.status_code,
        Some(StatusCode::OK),
        "a full token should have refilled after 60s"
    );
}

#[tokio::test]
async fn a_partial_wait_refills_proportionally_rather_than_wholesale() {
    // The refill is `elapsed * refill_per_sec`. The existing recovery test
    // waits a full window, where the result is clamped to capacity either way
    // -- so it cannot tell that multiplication from an addition. Waiting for
    // *part* of a window is what distinguishes them: 30s at 10/minute is
    // exactly 5 tokens, not 10.
    let Harness { service, clock } = harness(10, false);

    for i in 0..10 {
        assert_eq!(
            get(&service, None).await.status_code,
            Some(StatusCode::OK),
            "burst request {i} is within capacity"
        );
    }
    assert_eq!(
        get(&service, None).await.status_code,
        Some(StatusCode::TOO_MANY_REQUESTS)
    );

    clock.advance(Duration::from_secs(30));

    for i in 0..5 {
        assert_eq!(
            get(&service, None).await.status_code,
            Some(StatusCode::OK),
            "half a window refills half the budget; request {i} of 5"
        );
    }
    assert_eq!(
        get(&service, None).await.status_code,
        Some(StatusCode::TOO_MANY_REQUESTS),
        "the sixth request is past what 30 seconds bought"
    );
}

#[tokio::test]
async fn retry_after_reflects_the_wait_for_a_whole_token() {
    // A client that honours `Retry-After` must not come back to another 429.
    // At 1/minute an exhausted bucket needs the full 60 seconds.
    let Harness { service, clock } = harness(1, false);
    assert_eq!(get(&service, None).await.status_code, Some(StatusCode::OK));

    let res = get(&service, None).await;
    assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
    let retry_after: u64 = res
        .headers()
        .get(RETRY_AFTER)
        .expect("a 429 must say when to come back")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(retry_after, 60);

    clock.advance(Duration::from_secs(retry_after - 1));
    assert_eq!(
        get(&service, None).await.status_code,
        Some(StatusCode::TOO_MANY_REQUESTS),
        "one second early is still too early"
    );
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        get(&service, None).await.status_code,
        Some(StatusCode::OK),
        "waiting exactly as long as instructed must work"
    );
}

#[tokio::test]
async fn search_and_auth_limiters_are_independent() {
    // Two separate limiter instances (as installed on separate subrouters)
    // keep separate state: exhausting one leaves the other untouched.
    let auth = harness(1, false);
    let search = harness(1, false);

    // Exhaust the "auth" limiter.
    assert_eq!(
        get(&auth.service, None).await.status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        get(&auth.service, None).await.status_code,
        Some(StatusCode::TOO_MANY_REQUESTS)
    );

    // The "search" limiter still has its full budget.
    assert_eq!(
        get(&search.service, None).await.status_code,
        Some(StatusCode::OK)
    );
}

#[tokio::test]
async fn no_limiter_installed_never_rate_limits() {
    // Mirrors `BEAM_RATE_LIMIT_ENABLED=false`: no hoop is mounted, so an
    // arbitrary number of requests all pass.
    let service = Service::new(Router::new().goal(ok_handler));
    for i in 0..50 {
        assert_eq!(
            get(&service, None).await.status_code,
            Some(StatusCode::OK),
            "request {i} should pass when no limiter is installed"
        );
    }
}

#[tokio::test]
async fn forwarded_for_is_ignored_when_trust_flag_is_off() {
    // Trust off, capacity 1. Under TestClient the peer address is unknown, so
    // both "distinct" spoofed XFF clients share the single "unknown" bucket.
    let Harness { service, .. } = harness(1, false);

    assert_eq!(
        get(&service, Some("1.1.1.1")).await.status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        get(&service, Some("2.2.2.2")).await.status_code,
        Some(StatusCode::TOO_MANY_REQUESTS),
        "a different XFF must not grant a fresh budget when the trust flag is off"
    );
}

#[test]
fn build_rate_limiters_respects_enabled_flag() {
    let mut config = crate::config::ServerConfig::builder()
        .load()
        .expect("defaults-only config should load");

    assert!(
        crate::routes::build_rate_limiters(&config).is_some(),
        "enabled by default"
    );

    config.rate_limit_enabled = false;
    assert!(
        crate::routes::build_rate_limiters(&config).is_none(),
        "disabled flag must install no limiters"
    );
}
