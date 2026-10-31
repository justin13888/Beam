//! Shared behavioural contracts for this crate's stores: the
//! [`crate::utils::repository::UserRepository`], the
//! [`crate::utils::session_store::SessionStore`], the
//! [`crate::utils::pending_auth_store::PendingAuthStore`] and the
//! [`crate::utils::device_auth_store::DeviceAuthStore`], one file each.
//!
//! Each is instantiated over its in-memory double (hermetic, always run) and,
//! under the opt-in `pg-integration` feature, over its SQL implementation
//! against a real Postgres. This is what makes the doubles legitimate: the
//! same assertions constrain both, so a divergence fails rather than drifting
//! silently. Without them the tests here drove a `HashMap` and proved nothing
//! about production.
//!
//! Each macro is `#[macro_export]`ed, so it is invoked from the crate root
//! (`$crate::user_repository_contract!`) whichever file defines it.

mod device_auth_store;
mod pending_auth_store;
mod session_store;
mod user_repository;

/// Fixture the contract is written against.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod fixture {
    use beam_domain::services::TestClock;

    use crate::utils::repository::UserRepository;

    use crate::utils::device_auth_store::DeviceAuthStore;
    use crate::utils::pending_auth_store::PendingAuthStore;
    use crate::utils::session_store::SessionStore;

    #[async_trait::async_trait]
    pub trait UserRepositoryFixture: Send + Sync {
        /// The repository under contract, holding no users yet.
        fn repo(&self) -> &dyn UserRepository;

        /// The clock the repository stamps `created_at` from.
        fn clock(&self) -> &TestClock;
    }

    #[async_trait::async_trait]
    pub trait SessionStoreFixture: Send + Sync {
        /// The store under contract, holding no sessions yet.
        fn store(&self) -> &dyn SessionStore;

        /// The clock the store reads every expiry against.
        fn clock(&self) -> &TestClock;

        /// A user that exists as far as the backing store is concerned. A real
        /// Postgres enforces the `sessions.user_id` foreign key; the in-memory
        /// store does not, so the fixture owns the difference.
        async fn new_user(&self) -> uuid::Uuid;
    }

    #[async_trait::async_trait]
    pub trait PendingAuthStoreFixture: Send + Sync {
        /// The store under contract, holding no pending authorizations yet.
        fn store(&self) -> &dyn PendingAuthStore;

        /// The clock the store reads the TTL against.
        fn clock(&self) -> &TestClock;
    }

    #[async_trait::async_trait]
    pub trait DeviceAuthStoreFixture: Send + Sync {
        /// The store under contract. It may hold other tests' flows (a real
        /// database is shared), so every test keys its own by a fresh hash.
        fn store(&self) -> &dyn DeviceAuthStore;

        /// The clock the store reads pacing and expiry against.
        fn clock(&self) -> &TestClock;
    }
}
