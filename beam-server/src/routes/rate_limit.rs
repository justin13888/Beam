//! In-process token-bucket rate limiting for abuse-prone endpoints (NFR-107,
//! [#69](https://github.com/justin13888/beam/issues/69)).
//!
//! # Scope (deliberate)
//!
//! Two limiter *classes* are installed as `.hoop()` middleware, each keyed per
//! client:
//!
//! * **auth** — `/v1/auth/login` and `/v1/auth/callback`. These start an OIDC
//!   flow and are the classic credential-stuffing / callback-replay target.
//! * **search** — `GET /v1/media` (the browse/search endpoint), the single
//!   most expensive read path (it fans out into metadata queries).
//!
//! Everything else is intentionally *not* limited:
//!
//! * Streaming and download routes (`/v1/files/{id}/stream`, `.../download`)
//!   are excluded on purpose — a video player legitimately issues a burst of
//!   HTTP range requests for one playback session, and a per-client request
//!   counter would break seeking.
//! * The rest of the API is left unguarded here; it is session-gated and not a
//!   cheap-to-abuse surface. Limiting it would add contention for no benefit.
//!
//! # Design
//!
//! A classic token bucket per client key: `capacity` = burst, refilled at
//! `per_minute / 60` tokens per second. A request costs one token; when the
//! bucket is empty the request is rejected with `429 Too Many Requests`, a
//! `Retry-After` header, and the shared [`ApiErrorBody`] JSON shape.
//!
//! The bucket map is bounded ([`RateLimiter::MAX_ENTRIES`]) so a flood of
//! distinct client keys (e.g. spoofed `X-Forwarded-For` values) cannot grow it
//! without limit. When full, entries whose bucket has fully refilled (idle
//! clients) are evicted; if none are evictable the limiter *fails open* for the
//! new key — serving the request rather than rejecting a possibly-legitimate
//! client. Fail-open is the right trade-off for a defence-in-depth limiter:
//! availability beats a perfectly-enforced cap under adversarial key churn.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use salvo::http::header::RETRY_AFTER;
use salvo::http::{HeaderValue, StatusCode};
use salvo::prelude::*;

use crate::routes::api_error::ApiErrorBody;

/// A monotonic time source, abstracted so the limiter can be driven
/// deterministically in tests without any wall-clock wait.
///
/// This is intentionally a *minimal, local* seam (not `beam-index`'s richer
/// `Clock`, which would pull that crate into `beam-server`): the limiter only
/// needs a monotonic `Instant`.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// The current monotonic instant.
    fn now(&self) -> Instant;
}

/// Production clock backed by [`Instant::now`].
#[derive(Debug, Default, Clone)]
pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// One client's bucket: `tokens` available as of `last_refill`.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// The outcome of a single [`RateLimiter::check`].
#[derive(Debug, PartialEq)]
enum Decision {
    Allowed,
    /// Rejected; the client should retry after this many whole seconds.
    Limited {
        retry_after_secs: u64,
    },
}

/// A per-client token-bucket limiter, shared across requests via the router
/// (Salvo wraps a hoop in an `Arc`, so the interior `Mutex` state persists).
#[derive(Debug)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
    /// Burst capacity, in tokens.
    capacity: f64,
    /// Refill rate, in tokens per second.
    refill_per_sec: f64,
    /// Whether to trust `X-Forwarded-For` for the client key.
    trust_forwarded_for: bool,
    clock: Arc<dyn Clock>,
}

impl RateLimiter {
    /// Upper bound on tracked client keys. Chosen generously — one bucket is a
    /// few bytes — while still capping worst-case memory under key churn.
    const MAX_ENTRIES: usize = 10_000;

    /// Build a limiter allowing `per_minute` sustained requests with a burst
    /// capacity of `per_minute`. `per_minute` must be at least 1 (enforced by
    /// config validation).
    pub fn new(per_minute: u32, trust_forwarded_for: bool, clock: Arc<dyn Clock>) -> Self {
        let per_minute = per_minute.max(1) as f64;
        Self {
            buckets: Mutex::new(HashMap::new()),
            capacity: per_minute,
            refill_per_sec: per_minute / 60.0,
            trust_forwarded_for,
            clock,
        }
    }

    /// Derive the client key for `req`.
    ///
    /// Default: the peer socket IP (port stripped). When
    /// `trust_forwarded_for` is on, the first IP of a present, non-empty
    /// `X-Forwarded-For` wins instead — only safe behind a trusted proxy that
    /// overwrites the header, since it is otherwise trivially spoofable. When
    /// no address can be derived (e.g. a Unix socket, or an in-memory test
    /// client), a single fixed `"unknown"` bucket is used.
    fn client_key(&self, req: &Request) -> String {
        if self.trust_forwarded_for
            && let Some(xff) = req
                .headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
            && let Some(first) = xff.split(',').next()
        {
            let ip = first.trim();
            if !ip.is_empty() {
                return ip.to_string();
            }
        }

        match req.remote_addr().ip() {
            Some(ip) => ip.to_string(),
            None => "unknown".to_string(),
        }
    }

    /// Refill `bucket` to `now` and, if a token is available, consume one.
    fn take_token(&self, bucket: &mut Bucket, now: Instant) -> Decision {
        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Decision::Allowed
        } else {
            // Whole seconds until one full token has accrued (>= 1).
            let needed = 1.0 - bucket.tokens;
            let retry_after_secs = (needed / self.refill_per_sec).ceil().max(1.0) as u64;
            Decision::Limited { retry_after_secs }
        }
    }

    /// Account for one request from `key` and decide whether to allow it.
    fn check(&self, key: &str) -> Decision {
        let now = self.clock.now();
        let mut buckets = self.buckets.lock().expect("rate-limit mutex poisoned");

        if let Some(bucket) = buckets.get_mut(key) {
            return self.take_token(bucket, now);
        }

        // New key. Enforce the bound before inserting.
        if buckets.len() >= Self::MAX_ENTRIES {
            // Evict idle clients: any bucket that has fully refilled to
            // capacity has been silent long enough that dropping it only costs
            // it a fresh (still full) bucket next time.
            buckets.retain(|_, b| {
                let elapsed = now.saturating_duration_since(b.last_refill).as_secs_f64();
                let refilled = (b.tokens + elapsed * self.refill_per_sec).min(self.capacity);
                refilled < self.capacity
            });

            if buckets.len() >= Self::MAX_ENTRIES {
                // Still full of active clients: fail open (see module docs).
                return Decision::Allowed;
            }
        }

        // A brand-new client starts with a full bucket, then spends one token.
        let mut bucket = Bucket {
            tokens: self.capacity,
            last_refill: now,
        };
        let decision = self.take_token(&mut bucket, now);
        buckets.insert(key.to_string(), bucket);
        decision
    }
}

#[async_trait::async_trait]
impl Handler for RateLimiter {
    async fn handle(
        &self,
        req: &mut Request,
        _depot: &mut Depot,
        res: &mut Response,
        ctrl: &mut FlowCtrl,
    ) {
        let key = self.client_key(req);
        if let Decision::Limited { retry_after_secs } = self.check(&key) {
            res.status_code(StatusCode::TOO_MANY_REQUESTS);
            res.headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(retry_after_secs));
            res.render(Json(ApiErrorBody {
                error: "Rate limit exceeded".to_string(),
            }));
            ctrl.skip_rest();
        }
    }
}

/// The two limiter instances applied by `create_router`, one per class. Bundled
/// so `rest_routes` can attach both (or neither, for the docs router) in one
/// place.
pub struct RateLimiters {
    pub auth: RateLimiter,
    pub search: RateLimiter,
}

impl RateLimiters {
    /// Build both limiters from config, using the production [`RealClock`].
    pub fn from_config(config: &crate::config::ServerConfig) -> Self {
        let clock: Arc<dyn Clock> = Arc::new(RealClock);
        Self {
            auth: RateLimiter::new(
                config.rate_limit_auth_per_minute,
                config.rate_limit_trust_forwarded_for,
                clock.clone(),
            ),
            search: RateLimiter::new(
                config.rate_limit_search_per_minute,
                config.rate_limit_trust_forwarded_for,
                clock,
            ),
        }
    }
}

#[cfg(test)]
#[path = "rate_limit_tests.rs"]
mod rate_limit_tests;
