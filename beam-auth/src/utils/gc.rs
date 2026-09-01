//! Periodic collection of expired authentication rows.
//!
//! Neither table this sweeps is required for correctness: `SessionStore::get`
//! filters on both expiries, and `PendingAuthStore::consume` refuses a state
//! whose TTL has passed. Both are required for *operability*. Every sign-in
//! writes a `sessions` row and nothing removed one unless the user explicitly
//! signed out; every authorization that is begun and abandoned leaves a
//! `pending_auths` row behind, on an endpoint reachable by anyone who can reach
//! the server. Over the life of a deployment both grow without bound.

use std::sync::Arc;

use beam_domain::services::Clock;
use tracing::{info, warn};

use crate::utils::pending_auth_store::PendingAuthStore;
use crate::utils::session_store::SessionStore;

/// What one sweep collected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Sessions past their idle or absolute expiry.
    pub sessions: u64,
    /// Authorizations begun and never completed.
    pub pending_auths: u64,
}

impl GcReport {
    /// Whether this sweep collected anything at all.
    pub fn is_empty(&self) -> bool {
        self.sessions == 0 && self.pending_auths == 0
    }
}

/// Collects expired sessions and abandoned pending authorizations on a timer.
#[derive(Debug)]
pub struct AuthGcService {
    sessions: Arc<dyn SessionStore>,
    pending_auths: Arc<dyn PendingAuthStore>,
    clock: Arc<dyn Clock>,
}

impl AuthGcService {
    pub fn new(
        sessions: Arc<dyn SessionStore>,
        pending_auths: Arc<dyn PendingAuthStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            sessions,
            pending_auths,
            clock,
        }
    }

    /// Sweeps forever, waiting `interval` between passes.
    ///
    /// The wait is on the injected [`Clock`], so a test drives the cadence by
    /// advancing it rather than by sleeping.
    pub async fn run(&self, interval: std::time::Duration) -> ! {
        loop {
            let report = self.sweep_once().await;
            if !report.is_empty() {
                info!(
                    sessions = report.sessions,
                    pending_auths = report.pending_auths,
                    "collected expired authentication rows"
                );
            }
            self.clock.sleep(interval).await;
        }
    }

    /// Runs one pass.
    ///
    /// A store that fails is logged and counted as zero rather than aborting
    /// the sweep: this is housekeeping, the rows it missed are still filtered
    /// out of every read, and the next pass will try again. Giving up on the
    /// whole loop because one statement failed would turn a transient database
    /// blip into unbounded growth for the life of the process.
    pub async fn sweep_once(&self) -> GcReport {
        let sessions = match self.sessions.delete_expired().await {
            Ok(deleted) => deleted,
            Err(error) => {
                warn!(%error, "could not collect expired sessions");
                0
            }
        };

        let pending_auths = match self.pending_auths.delete_expired().await {
            Ok(deleted) => deleted,
            Err(error) => {
                warn!(%error, "could not collect abandoned pending authorizations");
                0
            }
        };

        GcReport {
            sessions,
            pending_auths,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use beam_domain::services::TestClock;

    use super::*;
    use crate::utils::pending_auth_store::in_memory::InMemoryPendingAuthStore;
    use crate::utils::pending_auth_store::{PendingAuth, PendingAuthError};
    use crate::utils::session_store::SessionData;
    use crate::utils::session_store::in_memory::InMemorySessionStore;

    const IDLE_TTL: u64 = 3600;
    const ABSOLUTE_TTL: u64 = 86_400;
    const PENDING_TTL: u64 = 600;

    fn session() -> SessionData {
        SessionData {
            user_id: uuid::Uuid::new_v4().to_string(),
            device_hash: "device".to_string(),
            ip: "203.0.113.7".to_string(),
            created_at: 0,
            last_active: 0,
        }
    }

    fn pending(state: &str) -> PendingAuth {
        PendingAuth {
            state: state.to_string(),
            nonce: "nonce".to_string(),
            pkce_verifier: "verifier".to_string(),
            redirect_path: None,
        }
    }

    /// One sweep must collect from both stores, and report each separately --
    /// the two grow for different reasons and an operator reading the log needs
    /// to see which one is doing it.
    #[tokio::test]
    async fn a_sweep_collects_expired_rows_from_both_stores() {
        let clock = Arc::new(TestClock::default());
        let sessions = Arc::new(InMemorySessionStore::new(clock.clone()));
        let pending_auths = Arc::new(InMemoryPendingAuthStore::new(clock.clone()));

        sessions
            .create(&session(), IDLE_TTL, ABSOLUTE_TTL)
            .await
            .unwrap();
        pending_auths
            .create(&pending("a"), PENDING_TTL)
            .await
            .unwrap();
        pending_auths
            .create(&pending("b"), PENDING_TTL)
            .await
            .unwrap();

        clock.advance(Duration::from_secs(IDLE_TTL + 1));

        let service = AuthGcService::new(sessions, pending_auths, clock);
        assert_eq!(
            service.sweep_once().await,
            GcReport {
                sessions: 1,
                pending_auths: 2,
            }
        );
    }

    /// The steady state: nothing has expired, so a sweep collects nothing and
    /// says so, rather than reporting phantom work every interval.
    #[tokio::test]
    async fn a_sweep_over_live_rows_collects_nothing() {
        let clock = Arc::new(TestClock::default());
        let sessions = Arc::new(InMemorySessionStore::new(clock.clone()));
        let pending_auths = Arc::new(InMemoryPendingAuthStore::new(clock.clone()));

        sessions
            .create(&session(), IDLE_TTL, ABSOLUTE_TTL)
            .await
            .unwrap();
        pending_auths
            .create(&pending("a"), PENDING_TTL)
            .await
            .unwrap();

        let service = AuthGcService::new(sessions, pending_auths, clock);
        let report = service.sweep_once().await;

        assert!(report.is_empty());
        assert_eq!(report, GcReport::default());
    }

    /// A store that always fails, to drive the error path. The sweep is
    /// housekeeping: one failing statement must not stop the other store being
    /// collected, or the loop from running again.
    #[derive(Debug)]
    struct FailingPendingAuthStore;

    #[async_trait::async_trait]
    impl PendingAuthStore for FailingPendingAuthStore {
        async fn create(
            &self,
            _auth: &PendingAuth,
            _ttl_secs: u64,
        ) -> Result<(), PendingAuthError> {
            Ok(())
        }

        async fn consume(&self, _state: &str) -> Result<Option<PendingAuth>, PendingAuthError> {
            Ok(None)
        }

        async fn delete_expired(&self) -> Result<u64, PendingAuthError> {
            Err(PendingAuthError::Db(sea_orm::DbErr::Custom(
                "connection reset".to_string(),
            )))
        }
    }

    #[tokio::test]
    async fn one_failing_store_does_not_stop_the_other_being_collected() {
        let clock = Arc::new(TestClock::default());
        let sessions = Arc::new(InMemorySessionStore::new(clock.clone()));

        sessions
            .create(&session(), IDLE_TTL, ABSOLUTE_TTL)
            .await
            .unwrap();
        clock.advance(Duration::from_secs(IDLE_TTL + 1));

        let service = AuthGcService::new(sessions, Arc::new(FailingPendingAuthStore), clock);

        assert_eq!(
            service.sweep_once().await,
            GcReport {
                sessions: 1,
                pending_auths: 0,
            },
            "the session sweep still runs and the failure counts as zero"
        );
    }
}
