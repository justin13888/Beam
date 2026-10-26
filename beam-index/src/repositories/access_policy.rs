//! SQL implementation of [`AccessPolicyRepository`] (issue #263).
//!
//! A policy is one `user_access_policies` row and the user's
//! `user_library_access` grants. Replacing it is one transaction: the row is
//! upserted, the grants deleted and the new ones inserted, so a reader never
//! sees half of one policy and half of another, and a grant naming a library
//! that does not exist fails the whole replacement on its foreign key.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait, Value,
};
use uuid::Uuid;

use beam_domain::models::access::{AccessPolicy, ContentRating, LibraryAccess};
use beam_domain::repositories::AccessPolicyRepository;
use beam_domain::services::{Clock, RealClock};

/// SQL-based implementation of the AccessPolicyRepository trait.
#[derive(Debug, Clone)]
pub struct SqlAccessPolicyRepository {
    db: Arc<DatabaseConnection>,
    /// Source of `updated_at`.
    clock: Arc<dyn Clock>,
}

impl SqlAccessPolicyRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_clock(db, Arc::new(RealClock))
    }

    pub fn with_clock(db: Arc<DatabaseConnection>, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

fn statement(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// One row of the policy read: the policy's columns, beside one of its
/// grants, or none.
#[derive(Debug, FromQueryResult)]
struct PolicyRow {
    all_libraries: bool,
    max_content_rating: Option<String>,
    library_id: Option<Uuid>,
}

/// The policy the rows of one user's read describe: `None` for no rows, a
/// user nobody restricted.
fn policy_of(user_id: Uuid, rows: Vec<PolicyRow>) -> Result<Option<AccessPolicy>, DbErr> {
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    let max_rating = first
        .max_content_rating
        .as_deref()
        .map(|stored| {
            ContentRating::parse(stored).ok_or_else(|| {
                DbErr::Custom(format!(
                    "user {user_id} has an unknown maximum content rating {stored:?}"
                ))
            })
        })
        .transpose()?;
    let libraries = if first.all_libraries {
        LibraryAccess::All
    } else {
        LibraryAccess::Only(
            rows.iter()
                .filter_map(|row| row.library_id)
                .collect::<BTreeSet<_>>(),
        )
    };
    Ok(Some(AccessPolicy {
        libraries,
        max_rating,
    }))
}

#[async_trait]
impl AccessPolicyRepository for SqlAccessPolicyRepository {
    async fn find(&self, user_id: Uuid) -> Result<AccessPolicy, DbErr> {
        // One statement, so the row and its grants are read from one
        // snapshot however a replacement interleaves.
        let rows = PolicyRow::find_by_statement(statement(
            "SELECT p.all_libraries, p.max_content_rating, g.library_id \
             FROM user_access_policies p \
             LEFT JOIN user_library_access g ON g.user_id = p.user_id \
             WHERE p.user_id = $1",
            vec![user_id.into()],
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(policy_of(user_id, rows)?.unwrap_or_default())
    }

    async fn replace(&self, user_id: Uuid, policy: &AccessPolicy) -> Result<(), DbErr> {
        let AccessPolicy {
            libraries,
            max_rating,
        } = policy;
        let txn = self.db.begin().await?;
        txn.execute_raw(statement(
            "DELETE FROM user_library_access WHERE user_id = $1",
            vec![user_id.into()],
        ))
        .await?;
        if policy.is_unrestricted() {
            txn.execute_raw(statement(
                "DELETE FROM user_access_policies WHERE user_id = $1",
                vec![user_id.into()],
            ))
            .await?;
            return txn.commit().await;
        }
        txn.execute_raw(statement(
            "INSERT INTO user_access_policies \
                 (user_id, all_libraries, max_content_rating, updated_at) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (user_id) DO UPDATE SET \
                 all_libraries = EXCLUDED.all_libraries, \
                 max_content_rating = EXCLUDED.max_content_rating, \
                 updated_at = EXCLUDED.updated_at",
            vec![
                user_id.into(),
                matches!(libraries, LibraryAccess::All).into(),
                max_rating.map(|band| band.as_str().to_string()).into(),
                self.clock.now().into(),
            ],
        ))
        .await?;
        if let LibraryAccess::Only(granted) = libraries {
            for library_id in granted {
                txn.execute_raw(statement(
                    "INSERT INTO user_library_access (user_id, library_id) VALUES ($1, $2)",
                    vec![user_id.into(), (*library_id).into()],
                ))
                .await?;
            }
        }
        txn.commit().await
    }
}
