use async_trait::async_trait;
use sea_orm::*;
use uuid::Uuid;

use crate::utils::models::{CreateUser, User};

/// Repository for managing user data.
#[async_trait]
pub trait UserRepository: Send + Sync + std::fmt::Debug {
    /// Finds a user by their unique identifier.
    async fn find_by_id(&self, id: Uuid) -> Result<Option<User>, DbErr>;

    /// Finds a user by their username.
    async fn find_by_username(&self, username: &str) -> Result<Option<User>, DbErr>;

    /// Finds a user by their email address.
    async fn find_by_email(&self, email: &str) -> Result<Option<User>, DbErr>;

    /// Creates a new user in the database.
    async fn create(&self, user: CreateUser) -> Result<User, DbErr>;
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

    async fn find_by_username(&self, username: &str) -> Result<Option<User>, DbErr> {
        use beam_entity::user;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = user::Entity::find()
            .filter(user::Column::Username.eq(username))
            .one(&self.db)
            .await?;
        Ok(model.map(User::from))
    }

    async fn find_by_email(&self, email: &str) -> Result<Option<User>, DbErr> {
        use beam_entity::user;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = user::Entity::find()
            .filter(user::Column::Email.eq(email))
            .one(&self.db)
            .await?;
        Ok(model.map(User::from))
    }

    async fn create(&self, create: CreateUser) -> Result<User, DbErr> {
        use beam_entity::user;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let CreateUser {
            username,
            email,
            password_hash,
            is_admin,
        } = create;

        let now = Utc::now();
        let new_user = user::ActiveModel {
            id: Set(Uuid::new_v4()),
            username: Set(username),
            email: Set(email),
            password_hash: Set(password_hash),
            is_admin: Set(is_admin),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
        };

        let result = new_user.insert(&self.db).await?;
        Ok(User::from(result))
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

        async fn find_by_username(&self, username: &str) -> Result<Option<User>, DbErr> {
            let users = self.users.lock().unwrap();
            Ok(users.values().find(|u| u.username == username).cloned())
        }

        async fn find_by_email(&self, email: &str) -> Result<Option<User>, DbErr> {
            let users = self.users.lock().unwrap();
            Ok(users.values().find(|u| u.email == email).cloned())
        }

        async fn create(&self, user: CreateUser) -> Result<User, DbErr> {
            let now = Utc::now();
            let new_user = User {
                id: Uuid::new_v4(),
                username: user.username,
                email: user.email,
                password_hash: user.password_hash,
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
    }
}
