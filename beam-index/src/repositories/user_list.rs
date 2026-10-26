//! SQL implementation of [`UserListRepository`] (issue #262).
//!
//! An addition is one `INSERT ... ON CONFLICT DO NOTHING` against the
//! per-kind unique indexes, so concurrent additions of one title cannot race
//! a read-then-insert; the entry is then read back, whichever statement wrote
//! it.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait, Value,
};
use uuid::Uuid;

use beam_domain::models::user_list::{UserList, UserListEntry, UserListPosition};
use beam_domain::models::watch_state::TitleRef;
use beam_domain::repositories::UserListRepository;
use beam_domain::repositories::user_list::carry_kind_mismatch;
use beam_domain::services::{Clock, RealClock};
use beam_entity::user_list_entry;

/// SQL-based implementation of the UserListRepository trait.
#[derive(Debug, Clone)]
pub struct SqlUserListRepository {
    db: Arc<DatabaseConnection>,
    /// Source of `added_at`. Injected so the shared contract orders entries
    /// by advancing a `TestClock` instead of sleeping.
    clock: Arc<dyn Clock>,
}

impl SqlUserListRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_clock(db, Arc::new(RealClock))
    }

    pub fn with_clock(db: Arc<DatabaseConnection>, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

/// Every column, in the order [`user_list_entry::Model`] reads them.
const COLUMNS: &str = "id, user_id, list, movie_id, show_id, added_at";

/// The column a title is keyed by, and its id.
fn title_column(title: TitleRef) -> (&'static str, Uuid) {
    match title {
        TitleRef::Movie(id) => ("movie_id", id),
        TitleRef::Show(id) => ("show_id", id),
    }
}

fn statement(sql: impl Into<String>, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

async fn entries(
    db: &impl ConnectionTrait,
    sql: String,
    values: Vec<Value>,
) -> Result<Vec<UserListEntry>, DbErr> {
    user_list_entry::Model::find_by_statement(statement(sql, values))
        .all(db)
        .await?
        .into_iter()
        .map(UserListEntry::try_from)
        .collect()
}

#[async_trait]
impl UserListRepository for SqlUserListRepository {
    async fn add(
        &self,
        user_id: Uuid,
        list: UserList,
        title: TitleRef,
    ) -> Result<UserListEntry, DbErr> {
        let (column, title_id) = title_column(title);
        let db = self.db.as_ref();
        db.execute_raw(statement(
            format!(
                "INSERT INTO user_list_entries (id, user_id, list, {column}, added_at) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING"
            ),
            vec![
                Uuid::new_v4().into(),
                user_id.into(),
                list.as_str().into(),
                title_id.into(),
                self.clock.now().into(),
            ],
        ))
        .await?;
        entries(
            db,
            format!(
                "SELECT {COLUMNS} FROM user_list_entries \
                 WHERE user_id = $1 AND list = $2 AND {column} = $3"
            ),
            vec![user_id.into(), list.as_str().into(), title_id.into()],
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "the {} entry for {title_id} was removed as it was added",
                list.as_str()
            ))
        })
    }

    async fn remove(&self, user_id: Uuid, list: UserList, title: TitleRef) -> Result<bool, DbErr> {
        let (column, title_id) = title_column(title);
        let removed = self
            .db
            .execute_raw(statement(
                format!(
                    "DELETE FROM user_list_entries \
                     WHERE user_id = $1 AND list = $2 AND {column} = $3"
                ),
                vec![user_id.into(), list.as_str().into(), title_id.into()],
            ))
            .await?;
        Ok(removed.rows_affected() > 0)
    }

    async fn lists_of(&self, user_id: Uuid, title: TitleRef) -> Result<Vec<UserList>, DbErr> {
        let (column, title_id) = title_column(title);
        let mut lists: Vec<UserList> = entries(
            self.db.as_ref(),
            format!("SELECT {COLUMNS} FROM user_list_entries WHERE user_id = $1 AND {column} = $2"),
            vec![user_id.into(), title_id.into()],
        )
        .await?
        .into_iter()
        .map(|entry| entry.list)
        .collect();
        lists.sort();
        Ok(lists)
    }

    async fn find_page(
        &self,
        user_id: Uuid,
        list: UserList,
        after: Option<UserListPosition>,
        limit: u64,
    ) -> Result<Vec<UserListEntry>, DbErr> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let (sql, values) = match after {
            None => (
                format!(
                    "SELECT {COLUMNS} FROM user_list_entries WHERE user_id = $1 AND list = $2 \
                     ORDER BY added_at DESC, id DESC LIMIT $3"
                ),
                vec![user_id.into(), list.as_str().into(), limit.into()],
            ),
            Some(UserListPosition { added_at, id }) => (
                format!(
                    "SELECT {COLUMNS} FROM user_list_entries WHERE user_id = $1 AND list = $2 \
                       AND (added_at, id) < ($3, $4) \
                     ORDER BY added_at DESC, id DESC LIMIT $5"
                ),
                vec![
                    user_id.into(),
                    list.as_str().into(),
                    added_at.into(),
                    id.into(),
                    limit.into(),
                ],
            ),
        };
        entries(self.db.as_ref(), sql, values).await
    }

    async fn count(&self, user_id: Uuid, list: UserList) -> Result<u64, DbErr> {
        #[derive(Debug, FromQueryResult)]
        struct Counted {
            count: i64,
        }

        let counted = Counted::find_by_statement(statement(
            "SELECT count(*) AS count FROM user_list_entries WHERE user_id = $1 AND list = $2",
            vec![user_id.into(), list.as_str().into()],
        ))
        .one(self.db.as_ref())
        .await?;
        Ok(counted.map_or(0, |c| u64::try_from(c.count).unwrap_or(0)))
    }

    async fn carry(&self, from: TitleRef, to: TitleRef) -> Result<(), DbErr> {
        let ((column, from_id), (to_column, to_id)) = (title_column(from), title_column(to));
        if column != to_column {
            return Err(carry_kind_mismatch());
        }
        if from_id == to_id {
            return Ok(());
        }
        let txn = self.db.begin().await?;
        // A user with both titles on one list keeps the survivor's entry,
        // added when the earlier of the two was; the retired title's goes.
        // Every other entry of the retired title moves onto the survivor.
        txn.execute_raw(statement(
            format!(
                "UPDATE user_list_entries kept SET added_at = LEAST(kept.added_at, gone.added_at) \
                 FROM user_list_entries gone \
                 WHERE kept.{column} = $2 AND gone.{column} = $1 \
                   AND kept.user_id = gone.user_id AND kept.list = gone.list"
            ),
            vec![from_id.into(), to_id.into()],
        ))
        .await?;
        txn.execute_raw(statement(
            format!(
                "DELETE FROM user_list_entries gone USING user_list_entries kept \
                 WHERE gone.{column} = $1 AND kept.{column} = $2 \
                   AND kept.user_id = gone.user_id AND kept.list = gone.list"
            ),
            vec![from_id.into(), to_id.into()],
        ))
        .await?;
        txn.execute_raw(statement(
            format!("UPDATE user_list_entries SET {column} = $2 WHERE {column} = $1"),
            vec![from_id.into(), to_id.into()],
        ))
        .await?;
        txn.commit().await
    }
}
