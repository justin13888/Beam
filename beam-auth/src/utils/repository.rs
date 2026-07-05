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
    /// admin-email allowlist on every login rather than trusting a stored
    /// value indefinitely.
    async fn set_admin(&self, id: Uuid, is_admin: bool) -> Result<(), DbErr>;

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
