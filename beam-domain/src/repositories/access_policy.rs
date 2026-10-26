use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::access::AccessPolicy;

/// What an administrator restricted each user to (issue #263): the libraries
/// they may see and the highest content rating.
///
/// A user nobody restricted has no stored policy and reads as
/// [`AccessPolicy::unrestricted`], so an install that never uses the feature
/// behaves as it did before it existed.
#[async_trait]
pub trait AccessPolicyRepository: Send + Sync + std::fmt::Debug {
    /// The user's policy: [`AccessPolicy::unrestricted`] for a user with none
    /// stored, and for an id no user has.
    async fn find(&self, user_id: Uuid) -> Result<AccessPolicy, DbErr>;

    /// Replace the user's policy whole, its library grants included, in one
    /// transaction. Replacing it with [`AccessPolicy::unrestricted`] removes
    /// what was stored, so it never fails for want of a user. Any other
    /// policy fails when the user, or a granted library, does not exist, and
    /// a policy failing that way leaves the previous one as it was.
    async fn replace(&self, user_id: Uuid, policy: &AccessPolicy) -> Result<(), DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use parking_lot::RwLock;
    use std::collections::{HashMap, HashSet};

    /// The in-memory double. It knows no users or libraries of its own, so
    /// it accepts any id unless told which exist: a test that wants the
    /// store's foreign keys registers them with [`Self::with_known`].
    #[derive(Debug, Default)]
    pub struct InMemoryAccessPolicyRepository {
        policies: RwLock<HashMap<Uuid, AccessPolicy>>,
        /// The users and libraries that exist, when a test said so.
        known: RwLock<Option<(HashSet<Uuid>, HashSet<Uuid>)>>,
    }

    impl InMemoryAccessPolicyRepository {
        /// A double refusing a policy for a user or a library not listed, as
        /// the store's foreign keys do.
        pub fn with_known(
            users: impl IntoIterator<Item = Uuid>,
            libraries: impl IntoIterator<Item = Uuid>,
        ) -> Self {
            Self {
                policies: RwLock::default(),
                known: RwLock::new(Some((
                    users.into_iter().collect(),
                    libraries.into_iter().collect(),
                ))),
            }
        }

        /// Register a user or a library as existing.
        pub fn know_user(&self, user_id: Uuid) {
            if let Some((users, _)) = self.known.write().as_mut() {
                users.insert(user_id);
            }
        }

        pub fn know_library(&self, library_id: Uuid) {
            if let Some((_, libraries)) = self.known.write().as_mut() {
                libraries.insert(library_id);
            }
        }
    }

    #[async_trait]
    impl AccessPolicyRepository for InMemoryAccessPolicyRepository {
        async fn find(&self, user_id: Uuid) -> Result<AccessPolicy, DbErr> {
            Ok(self
                .policies
                .read()
                .get(&user_id)
                .cloned()
                .unwrap_or_default())
        }

        async fn replace(&self, user_id: Uuid, policy: &AccessPolicy) -> Result<(), DbErr> {
            if policy.is_unrestricted() {
                self.policies.write().remove(&user_id);
                return Ok(());
            }
            if let Some((users, libraries)) = self.known.read().as_ref() {
                if !users.contains(&user_id) {
                    return Err(DbErr::Custom(format!("no user {user_id}")));
                }
                if let crate::models::access::LibraryAccess::Only(granted) = &policy.libraries
                    && let Some(unknown) = granted.iter().find(|id| !libraries.contains(id))
                {
                    return Err(DbErr::Custom(format!("no library {unknown}")));
                }
            }
            self.policies.write().insert(user_id, policy.clone());
            Ok(())
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use uuid::Uuid;

    use super::AccessPolicyRepository;
    use super::in_memory::InMemoryAccessPolicyRepository;
    use crate::repositories::contract::fixture::AccessPolicyFixture;

    /// The hermetic instantiation of the access policy contract, enforcing
    /// the foreign keys a real store holds.
    #[derive(Debug)]
    pub struct InMemoryFixture {
        repo: InMemoryAccessPolicyRepository,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            Self {
                repo: InMemoryAccessPolicyRepository::with_known([], []),
            }
        }
    }

    #[async_trait::async_trait]
    impl AccessPolicyFixture for InMemoryFixture {
        fn repo(&self) -> &dyn AccessPolicyRepository {
            &self.repo
        }

        async fn new_user(&self) -> Uuid {
            let id = Uuid::new_v4();
            self.repo.know_user(id);
            id
        }

        async fn new_library(&self) -> Uuid {
            let id = Uuid::new_v4();
            self.repo.know_library(id);
            id
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::access_policy_repository_contract!(setup);
}
