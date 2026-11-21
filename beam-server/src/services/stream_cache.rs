//! A short-lived memo of what `/stream` and `/download` decide per request
//! (issue #462).
//!
//! A player seeking through one file issues many Range requests a minute, and
//! a native player opens several at once. Uncached, each of them reads the
//! caller's access policy, asks whether that policy hides the file, and looks
//! the file up -- several database round trips repeated for an answer that
//! has not changed since the last request a second ago. This holds that
//! answer -- the file a user was granted, where it is -- for a few seconds,
//! keyed by `(user, file)`.
//!
//! What it does not hold is the session. Every request still resolves its
//! `beam_session` cookie against the store and slides its idle expiry, so a
//! revoked session is refused on its next request whatever is cached here,
//! and sign-out needs nothing from this cache. What can go stale is the
//! decision itself, and only for as long as [`STREAM_GRANT_TTL`]: a policy
//! change forgets the user's grants at once ([`StreamGrantCache::forget_user`],
//! called by [`AccessService::replace`](crate::services::access::AccessService::replace)),
//! and anything else -- a title relinked or re-rated, a file re-indexed or its
//! library removed -- is seen once the entry expires.
//!
//! Only a grant is held. A refusal (hidden or missing) is resolved afresh each
//! time: it is cheap to repeat being wrong about, and a file that appears in
//! the next scan should be served on the next request.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use uuid::Uuid;

use beam_domain::services::Clock;

use crate::services::library::LocatedFile;

/// How long a grant is served from memory before it is resolved again.
///
/// Seconds rather than minutes: long enough to absorb a seek's burst of
/// Range requests, short enough that a change no invalidation reaches -- a
/// title moved to a library the user is not granted -- takes effect within a
/// player's next buffering window.
pub const STREAM_GRANT_TTL: Duration = Duration::from_secs(5);

/// The most grants held at once. Each is two ids, two paths and a MIME type,
/// so the bound is a few megabytes at worst; past it, the grant nearest
/// expiry is dropped to make room.
pub const STREAM_GRANT_CAPACITY: NonZeroUsize = NonZeroUsize::new(4096).expect("non-zero");

/// Proof that a lookup missed, carrying the cache's generation at that moment.
///
/// A grant resolved after a miss is remembered only if nothing was forgotten
/// in between: otherwise a request that read a user's policy just before an
/// administrator replaced it could store the old decision after the
/// replacement forgot the user's grants, and serve it for a whole TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(u64);

/// The seam `/stream` and `/download` resolve a file through before reading
/// the database.
pub trait StreamGrantCache: Send + Sync + std::fmt::Debug {
    /// The file `user_id` was granted as `file_id`, while the grant is fresh;
    /// otherwise a [`Ticket`] to remember the grant about to be resolved by.
    fn lookup(&self, user_id: Uuid, file_id: Uuid) -> Result<LocatedFile, Ticket>;

    /// Remember that `user_id` was granted `file_id` as `file`, unless any
    /// grant was forgotten since `ticket` was issued.
    fn remember(&self, ticket: Ticket, user_id: Uuid, file_id: Uuid, file: LocatedFile);

    /// Forget every grant held for `user_id`, and refuse every grant resolved
    /// on a ticket issued before now.
    fn forget_user(&self, user_id: Uuid);
}

/// The production [`StreamGrantCache`]: at most `capacity` grants, each
/// expiring `ttl` after it was remembered by the injected clock's monotonic
/// time.
#[derive(Debug)]
pub struct BoundedStreamGrantCache {
    clock: Arc<dyn Clock>,
    ttl: Duration,
    capacity: NonZeroUsize,
    state: Mutex<GrantState>,
}

#[derive(Debug, Default)]
struct GrantState {
    /// Bumped by every [`StreamGrantCache::forget_user`].
    generation: u64,
    entries: HashMap<(Uuid, Uuid), Grant>,
}

#[derive(Debug)]
struct Grant {
    expires_at: Instant,
    file: LocatedFile,
}

impl BoundedStreamGrantCache {
    /// A cache holding [`STREAM_GRANT_CAPACITY`] grants for
    /// [`STREAM_GRANT_TTL`] each.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_limits(clock, STREAM_GRANT_TTL, STREAM_GRANT_CAPACITY)
    }

    /// A cache with its own bounds.
    pub fn with_limits(clock: Arc<dyn Clock>, ttl: Duration, capacity: NonZeroUsize) -> Self {
        Self {
            clock,
            ttl,
            capacity,
            state: Mutex::new(GrantState::default()),
        }
    }

    /// The state, even after a panic elsewhere held the lock: every update
    /// below leaves it consistent at each step, so a poisoned lock holds
    /// nothing half-written.
    fn state(&self) -> std::sync::MutexGuard<'_, GrantState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl StreamGrantCache for BoundedStreamGrantCache {
    fn lookup(&self, user_id: Uuid, file_id: Uuid) -> Result<LocatedFile, Ticket> {
        let now = self.clock.monotonic();
        let mut state = self.state();
        let key = (user_id, file_id);
        match state.entries.get(&key) {
            Some(grant) if now < grant.expires_at => Ok(grant.file.clone()),
            Some(_) => {
                state.entries.remove(&key);
                Err(Ticket(state.generation))
            }
            None => Err(Ticket(state.generation)),
        }
    }

    fn remember(&self, ticket: Ticket, user_id: Uuid, file_id: Uuid, file: LocatedFile) {
        let now = self.clock.monotonic();
        let mut state = self.state();
        if ticket != Ticket(state.generation) {
            return;
        }
        let key = (user_id, file_id);
        if !state.entries.contains_key(&key) && state.entries.len() >= self.capacity.get() {
            // The grant nearest expiry -- an expired one first -- is the one
            // least worth keeping. A scan, but only on a miss at capacity.
            let nearest = state
                .entries
                .iter()
                .min_by_key(|(_, grant)| grant.expires_at)
                .map(|(key, _)| *key);
            if let Some(nearest) = nearest {
                state.entries.remove(&nearest);
            }
        }
        state.entries.insert(
            key,
            Grant {
                expires_at: now + self.ttl,
                file,
            },
        );
    }

    fn forget_user(&self, user_id: Uuid) {
        let mut state = self.state();
        state.generation = state.generation.wrapping_add(1);
        state.entries.retain(|(user, _), _| *user != user_id);
    }
}

#[cfg(test)]
#[path = "stream_cache_tests.rs"]
mod stream_cache_tests;
