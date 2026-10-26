use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::language_preferences::LanguagePreferences;

/// Each user's audio and subtitle language preferences (issue #264).
///
/// A user who never set any reads as [`LanguagePreferences::default`].
#[async_trait]
pub trait LanguagePreferenceRepository: Send + Sync + std::fmt::Debug {
    /// The user's preferences: the default for a user with none stored, and
    /// for an id no user has.
    async fn find(&self, user_id: Uuid) -> Result<LanguagePreferences, DbErr>;

    /// Replace the user's preferences whole. Fails for a user that does not
    /// exist.
    async fn replace(&self, user_id: Uuid, preferences: &LanguagePreferences) -> Result<(), DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use parking_lot::RwLock;
    use std::collections::{HashMap, HashSet};

    /// The in-memory double. It accepts any user unless told which exist:
    /// a test that wants the store's foreign key registers them with
    /// [`Self::know_user`] on a double built by [`Self::enforcing_users`].
    #[derive(Debug, Default)]
    pub struct InMemoryLanguagePreferenceRepository {
        preferences: RwLock<HashMap<Uuid, LanguagePreferences>>,
        /// The users that exist, when a test said so.
        known_users: RwLock<Option<HashSet<Uuid>>>,
    }

    impl InMemoryLanguagePreferenceRepository {
        /// A double refusing preferences for a user not registered, as the
        /// store's foreign key does.
        pub fn enforcing_users() -> Self {
            Self {
                preferences: RwLock::default(),
                known_users: RwLock::new(Some(HashSet::new())),
            }
        }

        /// Register a user as existing.
        pub fn know_user(&self, user_id: Uuid) {
            if let Some(users) = self.known_users.write().as_mut() {
                users.insert(user_id);
            }
        }
    }

    #[async_trait]
    impl LanguagePreferenceRepository for InMemoryLanguagePreferenceRepository {
        async fn find(&self, user_id: Uuid) -> Result<LanguagePreferences, DbErr> {
            Ok(self
                .preferences
                .read()
                .get(&user_id)
                .cloned()
                .unwrap_or_default())
        }

        async fn replace(
            &self,
            user_id: Uuid,
            preferences: &LanguagePreferences,
        ) -> Result<(), DbErr> {
            if let Some(users) = self.known_users.read().as_ref()
                && !users.contains(&user_id)
            {
                return Err(DbErr::Custom(format!("no user {user_id}")));
            }
            self.preferences
                .write()
                .insert(user_id, preferences.clone());
            Ok(())
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use uuid::Uuid;

    use super::LanguagePreferenceRepository;
    use super::in_memory::InMemoryLanguagePreferenceRepository;
    use crate::repositories::contract::fixture::LanguagePreferenceFixture;

    /// The hermetic instantiation of the language preference contract,
    /// enforcing the user foreign key a real store holds.
    #[derive(Debug)]
    pub struct InMemoryFixture {
        repo: InMemoryLanguagePreferenceRepository,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            Self {
                repo: InMemoryLanguagePreferenceRepository::enforcing_users(),
            }
        }
    }

    #[async_trait::async_trait]
    impl LanguagePreferenceFixture for InMemoryFixture {
        fn repo(&self) -> &dyn LanguagePreferenceRepository {
            &self.repo
        }

        async fn new_user(&self) -> Uuid {
            let id = Uuid::new_v4();
            self.repo.know_user(id);
            id
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::language_preference_repository_contract!(setup);
}
