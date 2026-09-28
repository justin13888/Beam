//! SQL implementation of [`WatchStateRepository`] (issue #188).
//!
//! The writes are single `INSERT ... ON CONFLICT` statements, so a report,
//! a mark and a concurrent report for the same title cannot race a
//! read-modify-write. The sticky-played rules of
//! [`beam_domain::models::watch_state::Played`] are restated here as `CASE`
//! expressions over the old row; the shared contract holds this and the
//! in-memory double to the same outcomes.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait, Value,
};
use uuid::Uuid;

use beam_domain::models::watch_state::{
    ContinueCandidate, HistoryPosition, RecordProgress, TitleRef, WatchState, WatchTarget,
};
use beam_domain::repositories::WatchStateRepository;
use beam_domain::services::{Clock, RealClock};

/// SQL-based implementation of the WatchStateRepository trait.
#[derive(Debug, Clone)]
pub struct SqlWatchStateRepository {
    db: Arc<DatabaseConnection>,
    /// Source of `last_played_at` and `dismissed_at`. Injected so the shared
    /// contract orders rows by advancing a `TestClock` instead of sleeping.
    clock: Arc<dyn Clock>,
}

impl SqlWatchStateRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_clock(db, Arc::new(RealClock))
    }

    pub fn with_clock(db: Arc<DatabaseConnection>, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

/// Every column, in the order [`beam_entity::watch_state::Model`] reads them.
const COLUMNS: &str = "id, user_id, movie_id, episode_id, show_id, last_file_id, position_secs, \
                       duration_secs, completed, play_count, last_played_at, dismissed_at";

/// The unique index a target's row is found by: `(user_id, movie_id)` or
/// `(user_id, episode_id)`.
fn key_column(target: WatchTarget) -> &'static str {
    match target {
        WatchTarget::Movie { .. } => "movie_id",
        WatchTarget::Episode { .. } => "episode_id",
    }
}

fn key(target: WatchTarget) -> Uuid {
    match target {
        WatchTarget::Movie { movie_id } => movie_id,
        WatchTarget::Episode { episode_id, .. } => episode_id,
    }
}

/// `(movie_id, episode_id, show_id)` for `target`.
fn target_columns(target: WatchTarget) -> (Option<Uuid>, Option<Uuid>, Option<Uuid>) {
    match target {
        WatchTarget::Movie { movie_id } => (Some(movie_id), None, None),
        WatchTarget::Episode {
            episode_id,
            show_id,
        } => (None, Some(episode_id), Some(show_id)),
    }
}

/// A row that sits at its end: played, not being rewatched. Reaching the end
/// again, or marking it played, changes nothing about it.
const AT_END: &str = "(watch_state.completed AND watch_state.position_secs = 0)";

fn statement(sql: impl Into<String>, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

async fn rows(
    db: &impl ConnectionTrait,
    sql: String,
    values: Vec<Value>,
) -> Result<Vec<WatchState>, DbErr> {
    beam_entity::watch_state::Model::find_by_statement(statement(sql, values))
        .all(db)
        .await?
        .into_iter()
        .map(WatchState::try_from)
        .collect()
}

/// `$first, $first+1, ...` for `count` bound values.
fn placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl SqlWatchStateRepository {
    /// Mark `targets` -- all movies, or all episodes -- played in one
    /// statement conflicting on their shared unique index.
    async fn mark_kind_played(
        &self,
        db: &impl ConnectionTrait,
        user_id: Uuid,
        targets: &[WatchTarget],
        now: DateTime<Utc>,
    ) -> Result<(), DbErr> {
        let Some(first) = targets.first() else {
            return Ok(());
        };
        let column = key_column(*first);
        let mut values: Vec<Value> = vec![user_id.into(), now.into()];
        let mut tuples = Vec::with_capacity(targets.len());
        for target in targets {
            let (movie_id, episode_id, show_id) = target_columns(*target);
            let base = values.len();
            tuples.push(format!(
                "(${}, $1, ${}, ${}, ${}, 0, true, 1, $2)",
                base + 1,
                base + 2,
                base + 3,
                base + 4
            ));
            values.extend([
                Uuid::new_v4().into(),
                movie_id.into(),
                episode_id.into(),
                show_id.into(),
            ]);
        }
        let sql = format!(
            "INSERT INTO watch_state (id, user_id, movie_id, episode_id, show_id, position_secs, \
                                      completed, play_count, last_played_at) \
             VALUES {tuples} \
             ON CONFLICT (user_id, {column}) DO UPDATE SET \
               play_count = CASE WHEN {AT_END} THEN watch_state.play_count \
                                 ELSE watch_state.play_count + 1 END, \
               last_played_at = CASE WHEN {AT_END} THEN watch_state.last_played_at \
                                     ELSE excluded.last_played_at END, \
               position_secs = 0, \
               completed = true",
            tuples = tuples.join(", "),
        );
        db.execute_raw(statement(sql, values)).await?;
        Ok(())
    }
}

/// `targets` without repeats, split into movies and episodes: one `ON
/// CONFLICT` statement can neither target two unique indexes nor touch one
/// row twice.
fn split_targets(targets: &[WatchTarget]) -> (Vec<WatchTarget>, Vec<WatchTarget>) {
    let mut movies = Vec::new();
    let mut episodes = Vec::new();
    for target in targets {
        let list = match target {
            WatchTarget::Movie { .. } => &mut movies,
            WatchTarget::Episode { .. } => &mut episodes,
        };
        if !list
            .iter()
            .any(|seen: &WatchTarget| key(*seen) == key(*target))
        {
            list.push(*target);
        }
    }
    (movies, episodes)
}

#[async_trait]
impl WatchStateRepository for SqlWatchStateRepository {
    async fn record_progress(&self, report: RecordProgress) -> Result<WatchState, DbErr> {
        let reaches_end = report.reaches_end();
        let RecordProgress {
            user_id,
            target,
            file_id,
            position_secs,
            duration_secs,
        } = report;
        let (movie_id, episode_id, show_id) = target_columns(target);
        // What a first report writes; on conflict the CASEs below derive the
        // row from the old one, as `Played::finished` and `Played::at` do.
        let position_secs = if reaches_end { 0.0 } else { position_secs };
        let sql = format!(
            "INSERT INTO watch_state (id, user_id, movie_id, episode_id, show_id, last_file_id, \
                                      position_secs, duration_secs, completed, play_count, \
                                      last_played_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, CASE WHEN $9 THEN 1 ELSE 0 END, $10) \
             ON CONFLICT (user_id, {column}) DO UPDATE SET \
               play_count = CASE WHEN excluded.completed AND NOT {AT_END} \
                                 THEN watch_state.play_count + 1 \
                                 ELSE watch_state.play_count END, \
               completed = watch_state.completed OR excluded.completed, \
               position_secs = excluded.position_secs, \
               duration_secs = COALESCE(excluded.duration_secs, watch_state.duration_secs), \
               last_file_id = excluded.last_file_id, \
               last_played_at = excluded.last_played_at \
             RETURNING {COLUMNS}",
            column = key_column(target),
        );
        let values: Vec<Value> = vec![
            Uuid::new_v4().into(),
            user_id.into(),
            movie_id.into(),
            episode_id.into(),
            show_id.into(),
            file_id.into(),
            position_secs.into(),
            duration_secs.into(),
            reaches_end.into(),
            self.clock.now().into(),
        ];
        rows(self.db.as_ref(), sql, values)
            .await?
            .pop()
            .ok_or_else(|| DbErr::RecordNotFound("the upserted watch_state row".to_string()))
    }

    async fn mark_played(&self, user_id: Uuid, targets: &[WatchTarget]) -> Result<(), DbErr> {
        let (movies, episodes) = split_targets(targets);
        if movies.is_empty() && episodes.is_empty() {
            return Ok(());
        }
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        self.mark_kind_played(&txn, user_id, &movies, now).await?;
        self.mark_kind_played(&txn, user_id, &episodes, now).await?;
        txn.commit().await
    }

    async fn mark_unplayed(&self, user_id: Uuid, targets: &[WatchTarget]) -> Result<(), DbErr> {
        let (movies, episodes) = split_targets(targets);
        for (column, list) in [("movie_id", movies), ("episode_id", episodes)] {
            if list.is_empty() {
                continue;
            }
            let mut values: Vec<Value> = vec![user_id.into()];
            values.extend(list.iter().map(|target| Value::from(key(*target))));
            let sql = format!(
                "DELETE FROM watch_state WHERE user_id = $1 AND {column} IN ({})",
                placeholders(2, list.len())
            );
            self.db.execute_raw(statement(sql, values)).await?;
        }
        Ok(())
    }

    async fn clear_progress(&self, user_id: Uuid, target: WatchTarget) -> Result<(), DbErr> {
        let column = key_column(target);
        let values = || -> Vec<Value> { vec![user_id.into(), key(target).into()] };
        let txn = self.db.begin().await?;
        txn.execute_raw(statement(
            format!(
                "DELETE FROM watch_state WHERE user_id = $1 AND {column} = $2 AND NOT completed"
            ),
            values(),
        ))
        .await?;
        txn.execute_raw(statement(
            format!(
                "UPDATE watch_state SET position_secs = 0 WHERE user_id = $1 AND {column} = $2"
            ),
            values(),
        ))
        .await?;
        txn.commit().await
    }

    async fn dismiss(&self, user_id: Uuid, title: TitleRef) -> Result<(), DbErr> {
        let (column, id) = match title {
            TitleRef::Movie(id) => ("movie_id", id),
            TitleRef::Show(id) => ("show_id", id),
        };
        self.db
            .execute_raw(statement(
                format!(
                    "UPDATE watch_state SET dismissed_at = $3 WHERE user_id = $1 AND {column} = $2"
                ),
                vec![user_id.into(), id.into(), self.clock.now().into()],
            ))
            .await?;
        Ok(())
    }

    async fn find(&self, user_id: Uuid, target: WatchTarget) -> Result<Option<WatchState>, DbErr> {
        let sql = format!(
            "SELECT {COLUMNS} FROM watch_state WHERE user_id = $1 AND {} = $2",
            key_column(target)
        );
        Ok(rows(
            self.db.as_ref(),
            sql,
            vec![user_id.into(), key(target).into()],
        )
        .await?
        .pop())
    }

    async fn find_for_movies(
        &self,
        user_id: Uuid,
        movie_ids: &[Uuid],
    ) -> Result<Vec<WatchState>, DbErr> {
        if movie_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut values: Vec<Value> = vec![user_id.into()];
        values.extend(movie_ids.iter().map(|id| Value::from(*id)));
        let sql = format!(
            "SELECT {COLUMNS} FROM watch_state WHERE user_id = $1 AND movie_id IN ({})",
            placeholders(2, movie_ids.len())
        );
        rows(self.db.as_ref(), sql, values).await
    }

    async fn find_for_show(&self, user_id: Uuid, show_id: Uuid) -> Result<Vec<WatchState>, DbErr> {
        rows(
            self.db.as_ref(),
            format!("SELECT {COLUMNS} FROM watch_state WHERE user_id = $1 AND show_id = $2"),
            vec![user_id.into(), show_id.into()],
        )
        .await
    }

    async fn find_for_shows(
        &self,
        user_id: Uuid,
        show_ids: &[Uuid],
    ) -> Result<Vec<WatchState>, DbErr> {
        if show_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut values: Vec<Value> = vec![user_id.into()];
        values.extend(show_ids.iter().map(|id| Value::from(*id)));
        let sql = format!(
            "SELECT {COLUMNS} FROM watch_state WHERE user_id = $1 AND show_id IN ({})",
            placeholders(2, show_ids.len())
        );
        rows(self.db.as_ref(), sql, values).await
    }

    async fn find_continue_candidates(
        &self,
        user_id: Uuid,
        after: Option<ContinueCandidate>,
        limit: u64,
    ) -> Result<Vec<ContinueCandidate>, DbErr> {
        use sea_orm::prelude::DateTimeWithTimeZone;

        #[derive(Debug, FromQueryResult)]
        struct Candidate {
            movie_id: Option<Uuid>,
            show_id: Option<Uuid>,
            last_played_at: DateTimeWithTimeZone,
        }

        // Movie rows group as (movie_id, NULL), a show's episodes as (NULL,
        // show_id): GROUP BY treats the NULLs as equal. A page seeks past the
        // last candidate before it rather than skipping an offset, so reading
        // on does not re-read what came before.
        let mut values: Vec<Value> = vec![
            user_id.into(),
            i64::try_from(limit).unwrap_or(i64::MAX).into(),
        ];
        let seek = match after {
            None => "",
            Some(ContinueCandidate {
                title,
                last_played_at,
            }) => {
                values.extend([Value::from(last_played_at), Value::from(title.id())]);
                "AND (max(last_played_at), coalesce(movie_id, show_id)) < ($3, $4) "
            }
        };
        let sql = format!(
            "SELECT movie_id, show_id, max(last_played_at) AS last_played_at \
               FROM watch_state WHERE user_id = $1 \
              GROUP BY movie_id, show_id \
             HAVING max(last_played_at) > coalesce(max(dismissed_at), '-infinity') \
                AND (bool_or(show_id IS NOT NULL) OR bool_or(position_secs > 0)) \
                {seek}\
              ORDER BY max(last_played_at) DESC, coalesce(movie_id, show_id) DESC \
              LIMIT $2"
        );
        let found = Candidate::find_by_statement(statement(sql, values))
            .all(self.db.as_ref())
            .await?;
        found
            .into_iter()
            .map(
                |Candidate {
                     movie_id,
                     show_id,
                     last_played_at,
                 }| {
                    let title = match (movie_id, show_id) {
                        (Some(movie_id), None) => TitleRef::Movie(movie_id),
                        (None, Some(show_id)) => TitleRef::Show(show_id),
                        _ => {
                            return Err(DbErr::Custom(
                                "a continue-watching group names neither a movie nor a show"
                                    .to_string(),
                            ));
                        }
                    };
                    Ok(ContinueCandidate {
                        title,
                        last_played_at: last_played_at.with_timezone(&Utc),
                    })
                },
            )
            .collect()
    }

    async fn find_history_page(
        &self,
        user_id: Uuid,
        after: Option<HistoryPosition>,
        limit: u64,
    ) -> Result<Vec<WatchState>, DbErr> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let (sql, values) = match after {
            None => (
                format!(
                    "SELECT {COLUMNS} FROM watch_state WHERE user_id = $1 \
                     ORDER BY last_played_at DESC, id DESC LIMIT $2"
                ),
                vec![user_id.into(), limit.into()],
            ),
            Some(HistoryPosition { last_played_at, id }) => (
                format!(
                    "SELECT {COLUMNS} FROM watch_state WHERE user_id = $1 \
                       AND (last_played_at, id) < ($2, $3) \
                     ORDER BY last_played_at DESC, id DESC LIMIT $4"
                ),
                vec![
                    user_id.into(),
                    last_played_at.into(),
                    id.into(),
                    limit.into(),
                ],
            ),
        };
        rows(self.db.as_ref(), sql, values).await
    }

    async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DbErr> {
        #[derive(Debug, FromQueryResult)]
        struct Counted {
            count: i64,
        }

        let counted = Counted::find_by_statement(statement(
            "SELECT count(*) AS count FROM watch_state WHERE user_id = $1",
            vec![user_id.into()],
        ))
        .one(self.db.as_ref())
        .await?;
        Ok(counted.map_or(0, |c| u64::try_from(c.count).unwrap_or(0)))
    }

    async fn last_played_at(
        &self,
        file_ids: Vec<Uuid>,
    ) -> Result<HashMap<Uuid, DateTime<Utc>>, DbErr> {
        use sea_orm::prelude::DateTimeWithTimeZone;

        #[derive(Debug, FromQueryResult)]
        struct LastPlayed {
            last_file_id: Uuid,
            last_played_at: DateTimeWithTimeZone,
        }

        if file_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let sql = format!(
            "SELECT last_file_id, max(last_played_at) AS last_played_at FROM watch_state \
             WHERE last_file_id IN ({}) GROUP BY last_file_id",
            placeholders(1, file_ids.len())
        );
        let found = LastPlayed::find_by_statement(statement(
            sql,
            file_ids.into_iter().map(Value::from).collect(),
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(found
            .into_iter()
            .map(|row| (row.last_file_id, row.last_played_at.with_timezone(&Utc)))
            .collect())
    }
}
