//! In-process token-bucket rate limiting for abuse-prone endpoints (NFR-107,
//! [#69](https://github.com/justin13888/beam/issues/69)).
//!
//! # Scope (deliberate)
//!
//! Two limiter *classes*, each keyed per client:
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
//! # Why Beam keeps its own algorithm
//!
//! Kynos ships a sliding-window limiter, and Beam does not use it: its
//! `Quotas::check` reads `SystemTime::now()` inline, with no clock seam.
//! `beam_domain::services::Clock` is the workspace's one canonical time seam
//! (AGENTS.md), and the test that proves a bucket *refills* -- rather than
//! merely that a full one refuses -- can only be written by moving time. So
//! Beam implements `RateLimitPolicy` instead, which is the seam Kynos provides
//! for exactly this, and keeps the bucket below it unchanged.
//!
//! The clock now arrives on `AppState` rather than in the constructor, because
//! `check` receives `&AppState`. The seam is the same one; only where it is
//! read from moved.
//!
//! # Design
//!
//! A classic token bucket per client key: `capacity` = burst, refilled at
//! `per_minute / 60` tokens per second. A request costs one token; when the
//! bucket is empty the request is refused with `429`, a `Retry-After`, and the
//! `X-RateLimit-*` headers Kynos renders from the returned [`ServiceLimit`].
//!
//! The bucket map is bounded ([`BeamRateLimit::MAX_ENTRIES`]) so a flood of
//! distinct client keys cannot grow it without limit. When full, entries whose
//! bucket has fully refilled (idle clients) are evicted; if none are evictable
//! the limiter *fails open* for the new key — serving the request rather than
//! rejecting a possibly-legitimate client. Fail-open is the right trade-off for
//! a defence-in-depth limiter: availability beats a perfectly-enforced cap
//! under adversarial key churn.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kynos::http::Request;
use kynos::middleware::rate_limit::decision::{Decision, RateLimitPolicy, ServiceLimit};
use kynos::middleware::rate_limit::key::{ByClientAddress, RateLimitKey};
use kynos::router::operation::Route;

use crate::config::ServerConfig;
use crate::state::AppState;

/// One client's bucket: `tokens` available as of `last_refill`.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// Which named class a limiter enforces.
///
/// The two differ only in which config key supplies `per_minute`, so they are
/// one policy with two instances rather than two policies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// `/v1/auth/login` and `/v1/auth/callback`.
    Auth,
    /// `GET /v1/media`.
    Search,
}

impl Class {
    /// The name this class reports in `X-RateLimit-*`.
    fn name(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Search => "search",
        }
    }

    /// Sustained requests per minute, from config.
    fn per_minute(self, config: &ServerConfig) -> u32 {
        match self {
            Self::Auth => config.rate_limit_auth_per_minute,
            Self::Search => config.rate_limit_search_per_minute,
        }
        .max(1)
    }
}

/// A per-client token-bucket limiter for one [`Class`].
#[derive(Debug)]
pub struct BeamRateLimit {
    class: Class,
    buckets: Mutex<HashMap<String, Bucket>>,
}

/// What one `check` concluded, before it is rendered as a Kynos [`Decision`].
#[derive(Debug, PartialEq)]
struct Outcome {
    /// Tokens left after this request, floored.
    remaining: u64,
    /// `None` when the request may continue.
    retry_after: Option<Duration>,
    /// How long until the bucket is full again.
    reset: Duration,
}

impl BeamRateLimit {
    /// Upper bound on tracked client keys. Chosen generously — one bucket is a
    /// few bytes — while still capping worst-case memory under key churn.
    const MAX_ENTRIES: usize = 10_000;

    /// A limiter for `class`. The numbers are read from `AppState` per request,
    /// so a limiter outlives any particular configuration.
    #[must_use]
    pub fn new(class: Class) -> Self {
        Self {
            class,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Refill `bucket` to `now` and, if a token is available, consume one.
    fn take_token(bucket: &mut Bucket, now: Instant, capacity: f64, refill: f64) -> Outcome {
        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill).min(capacity);
        bucket.last_refill = now;

        let reset = Duration::from_secs_f64(((capacity - bucket.tokens) / refill).max(0.0));

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Outcome {
                remaining: bucket.tokens as u64,
                retry_after: None,
                reset,
            }
        } else {
            // Whole seconds until one full token has accrued (>= 1).
            let needed = 1.0 - bucket.tokens;
            let seconds = (needed / refill).ceil().max(1.0);
            Outcome {
                remaining: 0,
                retry_after: Some(Duration::from_secs(seconds as u64)),
                reset,
            }
        }
    }

    /// Account for one request from `key` and decide whether to allow it.
    fn spend(&self, key: &str, now: Instant, capacity: f64, refill: f64) -> Outcome {
        let mut buckets = self.buckets.lock().expect("rate-limit mutex poisoned");

        if let Some(bucket) = buckets.get_mut(key) {
            return Self::take_token(bucket, now, capacity, refill);
        }

        // New key. Enforce the bound before inserting.
        if buckets.len() >= Self::MAX_ENTRIES {
            // Evict idle clients: any bucket that has fully refilled to
            // capacity has been silent long enough that dropping it only costs
            // it a fresh (still full) bucket next time.
            buckets.retain(|_, b| {
                let elapsed = now.saturating_duration_since(b.last_refill).as_secs_f64();
                (b.tokens + elapsed * refill).min(capacity) < capacity
            });

            if buckets.len() >= Self::MAX_ENTRIES {
                // Still full of active clients: fail open (see module docs).
                return Outcome {
                    remaining: capacity as u64,
                    retry_after: None,
                    reset: Duration::ZERO,
                };
            }
        }

        // A brand-new client starts with a full bucket, then spends one token.
        let mut bucket = Bucket {
            tokens: capacity,
            last_refill: now,
        };
        let outcome = Self::take_token(&mut bucket, now, capacity, refill);
        buckets.insert(key.to_owned(), bucket);
        outcome
    }

    /// The limit this class reports, whatever the outcome.
    fn limit(&self, quota: u64, outcome: &Outcome) -> ServiceLimit {
        ServiceLimit {
            name: Cow::Borrowed(self.class.name()),
            quota,
            remaining: outcome.remaining,
            reset: outcome.reset,
        }
    }
}

impl RateLimitPolicy<AppState> for BeamRateLimit {
    async fn check(&self, request: &Request, route: Route<'_>, state: &AppState) -> Decision {
        let per_minute = self.class.per_minute(&state.config);
        let quota = u64::from(per_minute);
        let capacity = f64::from(per_minute);
        let refill = capacity / 60.0;

        // Disabled is a runtime exemption rather than an unmounted interceptor:
        // the 429 stays declared on these operations in every build, so the
        // exported description does not change with deployment configuration.
        if !state.config.rate_limit_enabled {
            return Decision::allow(ServiceLimit {
                name: Cow::Borrowed(self.class.name()),
                quota,
                remaining: quota,
                reset: Duration::ZERO,
            });
        }

        // Keyed by client only, never by route: the two operations in the auth
        // class share one budget, which is what makes the class a class. Kynos
        // resolves the client through the router's trusted-proxy policy, so an
        // untrusted `X-Forwarded-For` cannot pick its own bucket -- which the
        // old per-limiter `trust_forwarded_for` flag could not guarantee.
        let Some(key) = ByClientAddress.partition(request, route, state) else {
            return Decision::allow(self.limit(
                quota,
                &Outcome {
                    remaining: quota,
                    retry_after: None,
                    reset: Duration::ZERO,
                },
            ));
        };

        let outcome = self.spend(&key, state.clock().monotonic(), capacity, refill);
        let limit = self.limit(quota, &outcome);

        match outcome.retry_after {
            Some(retry_after) => Decision::deny(retry_after, limit),
            None => Decision::allow(limit),
        }
    }
}

#[cfg(test)]
#[path = "rate_limit_tests.rs"]
mod rate_limit_tests;
