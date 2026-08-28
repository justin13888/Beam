use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::*;
use uuid::Uuid;

use beam_domain::services::{Clock, RealClock};

use crate::utils::models::{CreateUser, User};

/// Repository for managing user data. Identity is `(oidc_issuer,
/// oidc_subject)` -- there is no username/password (see ADR-0003).
#[async_trait]
pub trait UserRepository: Send + Sync + std::fmt::Debug {
    /// Finds a user by their unique identifier.
    async fn find_by_id(&self, id: Uuid) -> Result<Option<User>, DbErr>;

    /// Finds a user by their `(oidc_issuer, oidc_subject)` identity -- the
    /// JIT-provisioning lookup key.
    async fn find_by_oidc_identity(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<User>, DbErr>;

    /// Creates a new user in the database.
    async fn create(&self, user: CreateUser) -> Result<User, DbErr>;

    /// Updates a user's admin flag. OIDC login recomputes this from the
    /// IdP-asserted admin claim on every login (granting and revoking) rather
    /// than trusting a stored value indefinitely (see issue #85).
    async fn set_admin(&self, id: Uuid, is_admin: bool) -> Result<(), DbErr>;

    /// Returns a page of users ordered by `created_at` ascending (stable
    /// oldest-first ordering, so pagination doesn't shift as new users are
    /// JIT-provisioned). Backs the admin users list (issue #85).
    async fn list_page(&self, limit: u64, offset: u64) -> Result<Vec<User>, DbErr>;

    /// Total number of users, for the admin users list's `total` and the
    /// admin status endpoint's user count (issue #85).
    async fn count(&self) -> Result<u64, DbErr>;

    /// Sets a user's `disabled` moderation flag (issue #85). Disabling blocks
    /// future logins; the caller is responsible for revoking any live sessions
    /// (the admin handler does this via the session store). A no-op if the
    /// user does not exist.
    async fn set_disabled(&self, id: Uuid, disabled: bool) -> Result<(), DbErr>;

    /// Refreshes the OIDC-sourced profile fields (`display_name`,
    /// `avatar_url`) on an existing user. Called on every OIDC login so a
    /// name/picture change at the IdP shows up in beam without requiring
    /// re-provisioning.
    async fn update_oidc_profile(
        &self,
        id: Uuid,
        display_name: String,
        avatar_url: Option<String>,
    ) -> Result<(), DbErr>;
}

#[derive(Debug)]
pub struct SqlUserRepository {
    db: Arc<DatabaseConnection>,
    /// Source of the `created_at`/`updated_at` stamps. Injected rather than
    /// read from `Utc::now()` so the shared contract can assert
    /// oldest-first ordering by advancing a clock instead of racing the
    /// wall clock (equal timestamps make `list_page` order by the `id`
    /// tie-break, which is not the property under test).
    clock: Arc<dyn Clock>,
}

impl SqlUserRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_clock(db, Arc::new(RealClock))
    }

    pub fn with_clock(db: Arc<DatabaseConnection>, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

#[async_trait]
impl UserRepository for SqlUserRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<User>, DbErr> {
        use beam_entity::user;
        use sea_orm::EntityTrait;

        let model = user::Entity::find_by_id(id).one(self.db.as_ref()).await?;
        Ok(model.map(User::from))
    }

    async fn find_by_oidc_identity(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<User>, DbErr> {
        use beam_entity::user;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = user::Entity::find()
            .filter(user::Column::OidcIssuer.eq(issuer))
            .filter(user::Column::OidcSubject.eq(subject))
            .one(self.db.as_ref())
            .await?;
        Ok(model.map(User::from))
    }

    async fn create(&self, create: CreateUser) -> Result<User, DbErr> {
        use beam_entity::user;
        use sea_orm::{ActiveModelTrait, Set};

        let CreateUser {
            oidc_issuer,
            oidc_subject,
            email,
            display_name,
            avatar_url,
            is_admin,
        } = create;

        let now = self.clock.now();
        let new_user = user::ActiveModel {
            id: Set(Uuid::new_v4()),
            oidc_issuer: Set(oidc_issuer),
            oidc_subject: Set(oidc_subject),
            email: Set(email),
            display_name: Set(display_name),
            avatar_url: Set(avatar_url),
            is_admin: Set(is_admin),
            // JIT-provisioned users are always created enabled; disabling is
            // only ever done later through the admin API (issue #85).
            disabled: Set(false),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
        };

        let result = new_user.insert(self.db.as_ref()).await?;
        Ok(User::from(result))
    }

    async fn set_admin(&self, id: Uuid, is_admin: bool) -> Result<(), DbErr> {
        use beam_entity::user;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let Some(model) = user::Entity::find_by_id(id).one(self.db.as_ref()).await? else {
            return Ok(());
        };
        let mut active_model: user::ActiveModel = model.into();
        active_model.is_admin = Set(is_admin);
        active_model.update(self.db.as_ref()).await?;
        Ok(())
    }

    async fn list_page(&self, limit: u64, offset: u64) -> Result<Vec<User>, DbErr> {
        use beam_entity::user;
        use sea_orm::{EntityTrait, QueryOrder, QuerySelect};

        let models = user::Entity::find()
            .order_by_asc(user::Column::CreatedAt)
            .offset(offset)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;
        Ok(models.into_iter().map(User::from).collect())
    }

    async fn count(&self) -> Result<u64, DbErr> {
        use beam_entity::user;
        use sea_orm::{EntityTrait, PaginatorTrait};

        user::Entity::find().count(self.db.as_ref()).await
    }

    async fn set_disabled(&self, id: Uuid, disabled: bool) -> Result<(), DbErr> {
        use beam_entity::user;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let Some(model) = user::Entity::find_by_id(id).one(self.db.as_ref()).await? else {
            return Ok(());
        };
        let mut active_model: user::ActiveModel = model.into();
        active_model.disabled = Set(disabled);
        active_model.update(self.db.as_ref()).await?;
        Ok(())
    }

    async fn update_oidc_profile(
        &self,
        id: Uuid,
        display_name: String,
        avatar_url: Option<String>,
    ) -> Result<(), DbErr> {
        use beam_entity::user;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let Some(model) = user::Entity::find_by_id(id).one(self.db.as_ref()).await? else {
            return Ok(());
        };
        let mut active_model: user::ActiveModel = model.into();
        active_model.display_name = Set(display_name);
        active_model.avatar_url = Set(avatar_url);
        active_model.update(self.db.as_ref()).await?;
        Ok(())
    }
}

/// In-memory user repository for use in tests and offline scenarios.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug)]
    pub struct InMemoryUserRepository {
        users: Mutex<HashMap<Uuid, User>>,
        clock: Arc<dyn Clock>,
    }

    impl InMemoryUserRepository {
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self {
                users: Mutex::new(HashMap::new()),
                clock,
            }
        }
    }

    impl Default for InMemoryUserRepository {
        fn default() -> Self {
            Self::new(Arc::new(RealClock))
        }
    }

    #[async_trait]
    impl UserRepository for InMemoryUserRepository {
        async fn find_by_id(&self, id: Uuid) -> Result<Option<User>, DbErr> {
            Ok(self.users.lock().unwrap().get(&id).cloned())
        }

        async fn find_by_oidc_identity(
            &self,
            issuer: &str,
            subject: &str,
        ) -> Result<Option<User>, DbErr> {
            let users = self.users.lock().unwrap();
            Ok(users
                .values()
                .find(|u| u.oidc_issuer == issuer && u.oidc_subject == subject)
                .cloned())
        }

        async fn create(&self, user: CreateUser) -> Result<User, DbErr> {
            let now = self.clock.now();
            let new_user = User {
                id: Uuid::new_v4(),
                oidc_issuer: user.oidc_issuer,
                oidc_subject: user.oidc_subject,
                email: user.email,
                display_name: user.display_name,
                avatar_url: user.avatar_url,
                is_admin: user.is_admin,
                // JIT-provisioned users are always created enabled (issue #85).
                disabled: false,
                created_at: now,
                updated_at: now,
            };
            self.users
                .lock()
                .unwrap()
                .insert(new_user.id, new_user.clone());
            Ok(new_user)
        }

        async fn set_admin(&self, id: Uuid, is_admin: bool) -> Result<(), DbErr> {
            if let Some(user) = self.users.lock().unwrap().get_mut(&id) {
                user.is_admin = is_admin;
            }
            Ok(())
        }

        async fn list_page(&self, limit: u64, offset: u64) -> Result<Vec<User>, DbErr> {
            let users = self.users.lock().unwrap();
            let mut all: Vec<User> = users.values().cloned().collect();
            all.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    // Deterministic tie-break so equal timestamps (common in
                    // fast test seeding) still paginate stably.
                    .then_with(|| a.id.cmp(&b.id))
            });
            Ok(all
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect())
        }

        async fn count(&self) -> Result<u64, DbErr> {
            Ok(self.users.lock().unwrap().len() as u64)
        }

        async fn set_disabled(&self, id: Uuid, disabled: bool) -> Result<(), DbErr> {
            if let Some(user) = self.users.lock().unwrap().get_mut(&id) {
                user.disabled = disabled;
            }
            Ok(())
        }

        async fn update_oidc_profile(
            &self,
            id: Uuid,
            display_name: String,
            avatar_url: Option<String>,
        ) -> Result<(), DbErr> {
            if let Some(user) = self.users.lock().unwrap().get_mut(&id) {
                user.display_name = display_name;
                user.avatar_url = avatar_url;
            }
            Ok(())
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use beam_domain::services::TestClock;

    use super::UserRepository;
    use super::in_memory::InMemoryUserRepository;
    use crate::utils::contract::fixture::UserRepositoryFixture;

    /// The hermetic instantiation of the shared `UserRepository` contract.
    pub struct InMemoryFixture {
        repo: InMemoryUserRepository,
        clock: Arc<TestClock>,
    }

    impl InMemoryFixture {
        pub fn new() -> Self {
            let clock = Arc::new(TestClock::new());
            Self {
                repo: InMemoryUserRepository::new(clock.clone()),
                clock,
            }
        }
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait::async_trait]
    impl UserRepositoryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn UserRepository {
            &self.repo
        }

        fn clock(&self) -> &TestClock {
            &self.clock
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::new()
    }

    crate::user_repository_contract!(setup);
}
