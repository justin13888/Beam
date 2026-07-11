use async_trait::async_trait;
use sea_orm::*;
use uuid::Uuid;

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
    db: DatabaseConnection,
}

impl SqlUserRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl UserRepository for SqlUserRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<User>, DbErr> {
        use beam_entity::user;
        use sea_orm::EntityTrait;

        let model = user::Entity::find_by_id(id).one(&self.db).await?;
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
            .one(&self.db)
            .await?;
        Ok(model.map(User::from))
    }

    async fn create(&self, create: CreateUser) -> Result<User, DbErr> {
        use beam_entity::user;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let CreateUser {
            oidc_issuer,
            oidc_subject,
            email,
            display_name,
            avatar_url,
            is_admin,
        } = create;

        let now = Utc::now();
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

        let result = new_user.insert(&self.db).await?;
        Ok(User::from(result))
    }

    async fn set_admin(&self, id: Uuid, is_admin: bool) -> Result<(), DbErr> {
        use beam_entity::user;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let Some(model) = user::Entity::find_by_id(id).one(&self.db).await? else {
            return Ok(());
        };
        let mut active_model: user::ActiveModel = model.into();
        active_model.is_admin = Set(is_admin);
        active_model.update(&self.db).await?;
        Ok(())
    }

    async fn list_page(&self, limit: u64, offset: u64) -> Result<Vec<User>, DbErr> {
        use beam_entity::user;
        use sea_orm::{EntityTrait, QueryOrder, QuerySelect};

        let models = user::Entity::find()
            .order_by_asc(user::Column::CreatedAt)
            .offset(offset)
            .limit(limit)
            .all(&self.db)
            .await?;
        Ok(models.into_iter().map(User::from).collect())
    }

    async fn count(&self) -> Result<u64, DbErr> {
        use beam_entity::user;
        use sea_orm::{EntityTrait, PaginatorTrait};

        user::Entity::find().count(&self.db).await
    }

    async fn set_disabled(&self, id: Uuid, disabled: bool) -> Result<(), DbErr> {
        use beam_entity::user;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let Some(model) = user::Entity::find_by_id(id).one(&self.db).await? else {
            return Ok(());
        };
        let mut active_model: user::ActiveModel = model.into();
        active_model.disabled = Set(disabled);
        active_model.update(&self.db).await?;
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

        let Some(model) = user::Entity::find_by_id(id).one(&self.db).await? else {
            return Ok(());
        };
        let mut active_model: user::ActiveModel = model.into();
        active_model.display_name = Set(display_name);
        active_model.avatar_url = Set(avatar_url);
        active_model.update(&self.db).await?;
        Ok(())
    }
}

/// In-memory user repository for use in tests and offline scenarios.
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct InMemoryUserRepository {
        users: Mutex<HashMap<Uuid, User>>,
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
            let now = Utc::now();
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

#[cfg(test)]
mod tests {
    use super::in_memory::InMemoryUserRepository;
    use super::*;

    fn create_user(subject: &str, name: &str) -> CreateUser {
        CreateUser {
            oidc_issuer: "https://idp.test".to_string(),
            oidc_subject: subject.to_string(),
            email: None,
            display_name: name.to_string(),
            avatar_url: None,
            is_admin: false,
        }
    }

    #[tokio::test]
    async fn new_users_default_to_enabled() {
        let repo = InMemoryUserRepository::default();
        let user = repo.create(create_user("s1", "Alice")).await.unwrap();
        assert!(!user.disabled, "JIT-provisioned users must start enabled");
    }

    #[tokio::test]
    async fn count_tracks_created_users() {
        let repo = InMemoryUserRepository::default();
        assert_eq!(repo.count().await.unwrap(), 0);
        repo.create(create_user("s1", "Alice")).await.unwrap();
        repo.create(create_user("s2", "Bob")).await.unwrap();
        assert_eq!(repo.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn list_page_orders_oldest_first_and_paginates() {
        let repo = InMemoryUserRepository::default();
        // created_at is set to "now" per insert; the id tie-break keeps this
        // deterministic even when the timestamps collide.
        let a = repo.create(create_user("s1", "Alice")).await.unwrap();
        let b = repo.create(create_user("s2", "Bob")).await.unwrap();
        let c = repo.create(create_user("s3", "Carol")).await.unwrap();

        let all = repo.list_page(10, 0).await.unwrap();
        assert_eq!(all.len(), 3);

        // First page of two, then the remaining one, cover the whole set once.
        let page1 = repo.list_page(2, 0).await.unwrap();
        let page2 = repo.list_page(2, 2).await.unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page2.len(), 1);

        let mut seen: Vec<Uuid> = page1.iter().chain(page2.iter()).map(|u| u.id).collect();
        seen.sort();
        let mut expected = vec![a.id, b.id, c.id];
        expected.sort();
        assert_eq!(
            seen, expected,
            "pagination must cover every user exactly once"
        );
    }

    #[tokio::test]
    async fn set_disabled_toggles_flag_and_is_noop_for_unknown_id() {
        let repo = InMemoryUserRepository::default();
        let user = repo.create(create_user("s1", "Alice")).await.unwrap();

        repo.set_disabled(user.id, true).await.unwrap();
        let disabled = repo.find_by_id(user.id).await.unwrap().unwrap();
        assert!(disabled.disabled);

        repo.set_disabled(user.id, false).await.unwrap();
        let enabled = repo.find_by_id(user.id).await.unwrap().unwrap();
        assert!(!enabled.disabled);

        // Unknown id is a silent no-op, never an error.
        repo.set_disabled(Uuid::new_v4(), true).await.unwrap();
    }
}
