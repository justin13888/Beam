//! An administrator's management of other accounts: the `disabled`
//! moderation flag (issue #85) and per-user access (issue #263).
//!
//! The rules live here rather than in the handlers: an administrator cannot
//! disable their own account, disabling an account revokes every session it
//! holds, and an access policy names only libraries that exist.

use std::sync::Arc;

use sea_orm::DbErr;
use uuid::Uuid;

use beam_auth::utils::models::User;
use beam_auth::utils::repository::UserRepository;
use beam_auth::utils::session_store::SessionStore;
use beam_domain::models::access::{AccessPolicy, ContentRating, LibraryAccess};
use beam_domain::repositories::LibraryRepository;

use crate::models::{FieldError, UserAccess};
use crate::services::access::AccessService;

/// Why an administrator's change to an account was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum AdminUsersError {
    /// No account has the id.
    #[error("user {0} not found")]
    UserNotFound(Uuid),
    /// The administrator asked to disable their own account, which would
    /// lock them out.
    #[error("cannot disable your own account")]
    CannotDisableSelf,
    /// The access policy breaks one or more rules: one entry per broken
    /// rule, each naming its field.
    #[error("the access policy breaks {} rule(s)", .0.len())]
    InvalidAccess(Vec<FieldError>),
    /// A store failed.
    #[error("{0}")]
    Internal(String),
}

impl From<DbErr> for AdminUsersError {
    fn from(err: DbErr) -> Self {
        Self::Internal(err.to_string())
    }
}

/// Every store and service [`AdminUserService`] reads or writes.
#[derive(Debug)]
pub struct AdminUserDependencies {
    pub users: Arc<dyn UserRepository>,
    /// Revoked wholesale when an account is disabled.
    pub sessions: Arc<dyn SessionStore>,
    /// Where each user's access policy is kept.
    pub access: Arc<AccessService>,
    /// Checked for every library an access policy names.
    pub libraries: Arc<dyn LibraryRepository>,
}

/// An administrator's operations on accounts.
#[derive(Debug)]
pub struct AdminUserService {
    users: Arc<dyn UserRepository>,
    sessions: Arc<dyn SessionStore>,
    access: Arc<AccessService>,
    libraries: Arc<dyn LibraryRepository>,
}

impl AdminUserService {
    pub fn new(dependencies: AdminUserDependencies) -> Self {
        let AdminUserDependencies {
            users,
            sessions,
            access,
            libraries,
        } = dependencies;
        Self {
            users,
            sessions,
            access,
            libraries,
        }
    }

    /// The account `id` names, or [`AdminUsersError::UserNotFound`].
    async fn target(&self, id: Uuid) -> Result<User, AdminUsersError> {
        self.users
            .find_by_id(id)
            .await?
            .ok_or(AdminUsersError::UserNotFound(id))
    }

    /// Sets the `disabled` flag of the account `id` names, on behalf of the
    /// administrator whose user id is `actor`. Disabling revokes every
    /// session of the account, so it takes effect immediately rather than
    /// at the next login.
    ///
    /// # Errors
    ///
    /// [`AdminUsersError::UserNotFound`] for an id naming no account,
    /// [`AdminUsersError::CannotDisableSelf`] when `actor` disables
    /// themself -- nothing is written in either case -- and
    /// [`AdminUsersError::Internal`] when a store fails.
    pub async fn set_disabled(
        &self,
        actor: &str,
        id: Uuid,
        disabled: bool,
    ) -> Result<(), AdminUsersError> {
        let target = self.target(id).await?;

        if disabled && target.id.to_string() == actor {
            return Err(AdminUsersError::CannotDisableSelf);
        }

        self.users.set_disabled(target.id, disabled).await?;

        if disabled {
            // Revoke every live session so the disable takes effect
            // immediately, not just at the next login attempt.
            self.sessions
                .delete_all_for_user(&target.id.to_string())
                .await
                .map_err(|e| AdminUsersError::Internal(e.to_string()))?;
        }

        Ok(())
    }

    /// The access policy of the account `id` names.
    ///
    /// # Errors
    ///
    /// [`AdminUsersError::UserNotFound`] for an id naming no account;
    /// [`AdminUsersError::Internal`] when a store fails.
    pub async fn access(&self, id: Uuid) -> Result<AccessPolicy, AdminUsersError> {
        let target = self.target(id).await?;
        Ok(self.access.policy(target.id).await?)
    }

    /// Replaces the access policy of the account `id` names with `access`,
    /// as one whole, and returns what is now stored.
    ///
    /// # Errors
    ///
    /// [`AdminUsersError::UserNotFound`] for an id naming no account;
    /// [`AdminUsersError::InvalidAccess`] when `library_ids` is not empty
    /// alongside `all_libraries`, or names a library that does not exist --
    /// nothing is written in either case; [`AdminUsersError::Internal`]
    /// when a store fails.
    pub async fn set_access(
        &self,
        id: Uuid,
        access: UserAccess,
    ) -> Result<AccessPolicy, AdminUsersError> {
        let target = self.target(id).await?;

        let UserAccess {
            all_libraries,
            library_ids,
            max_content_rating,
        } = access;
        let mut errors = Vec::new();
        if all_libraries && !library_ids.is_empty() {
            errors.push(FieldError {
                pointer: "/library_ids".to_owned(),
                detail: "must be empty when all_libraries is true".to_owned(),
            });
        }
        for (index, library_id) in library_ids.iter().enumerate() {
            if self.libraries.find_by_id(*library_id).await?.is_none() {
                errors.push(FieldError {
                    pointer: format!("/library_ids/{index}"),
                    detail: format!("no library has the id {library_id}"),
                });
            }
        }
        if !errors.is_empty() {
            return Err(AdminUsersError::InvalidAccess(errors));
        }

        let policy = AccessPolicy {
            libraries: if all_libraries {
                LibraryAccess::All
            } else {
                LibraryAccess::Only(library_ids.into_iter().collect())
            },
            max_rating: max_content_rating.map(ContentRating::from),
        };
        self.access.replace(target.id, &policy).await?;
        Ok(policy)
    }
}
