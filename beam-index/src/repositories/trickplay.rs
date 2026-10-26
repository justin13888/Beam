//! SQL implementation of [`TrickplayRepository`] (issue #265).
//!
//! A file's record is one `file_trickplay` row, written by one
//! `INSERT ... ON CONFLICT (file_id) DO UPDATE`.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use beam_domain::models::trickplay::{TrickplayLayout, TrickplayRecord};
use beam_domain::repositories::TrickplayRepository;
use beam_entity::file_trickplay;

/// SQL-based implementation of the TrickplayRepository trait.
#[derive(Debug, Clone)]
pub struct SqlTrickplayRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlTrickplayRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

/// A layout dimension as the table holds it.
fn stored(value: u32) -> Result<i32, DbErr> {
    i32::try_from(value).map_err(|_| DbErr::Custom(format!("a trickplay dimension of {value}")))
}

fn row_of(record: TrickplayRecord) -> Result<file_trickplay::ActiveModel, DbErr> {
    let TrickplayRecord {
        file_id,
        file_hash,
        layout,
        generated_at,
    } = record;
    let column = |value: Option<u32>| value.map(stored).transpose();
    Ok(file_trickplay::ActiveModel {
        file_id: Set(file_id),
        // The hash's bits, as `files.hash_xxh3` holds them.
        file_hash: Set(file_hash as i64),
        interval_ms: Set(column(layout.map(|l| l.interval_ms))?),
        tile_width: Set(column(layout.map(|l| l.tile_width))?),
        tile_height: Set(column(layout.map(|l| l.tile_height))?),
        columns: Set(column(layout.map(|l| l.columns))?),
        rows: Set(column(layout.map(|l| l.rows))?),
        tile_count: Set(column(layout.map(|l| l.tile_count))?),
        generated_at: Set(generated_at.into()),
    })
}

/// The record a row holds. Layout columns that are not all set, or not all
/// `NULL`, are what the table's `CHECK` refuses, so they are an error.
fn record_of(row: file_trickplay::Model) -> Result<TrickplayRecord, DbErr> {
    let file_trickplay::Model {
        file_id,
        file_hash,
        interval_ms,
        tile_width,
        tile_height,
        columns,
        rows,
        tile_count,
        generated_at,
    } = row;
    let invalid = || DbErr::Custom(format!("file {file_id} has an invalid trickplay layout"));
    let dimension = |value: i32| u32::try_from(value).map_err(|_| invalid());
    let layout = match (
        interval_ms,
        tile_width,
        tile_height,
        columns,
        rows,
        tile_count,
    ) {
        (None, None, None, None, None, None) => None,
        (
            Some(interval_ms),
            Some(tile_width),
            Some(tile_height),
            Some(columns),
            Some(rows),
            Some(tile_count),
        ) => Some(TrickplayLayout {
            interval_ms: dimension(interval_ms)?,
            tile_width: dimension(tile_width)?,
            tile_height: dimension(tile_height)?,
            columns: dimension(columns)?,
            rows: dimension(rows)?,
            tile_count: dimension(tile_count)?,
        }),
        _ => return Err(invalid()),
    };
    Ok(TrickplayRecord {
        file_id,
        file_hash: file_hash as u64,
        layout,
        generated_at: generated_at.into(),
    })
}

#[async_trait]
impl TrickplayRepository for SqlTrickplayRepository {
    async fn upsert(&self, record: TrickplayRecord) -> Result<(), DbErr> {
        use file_trickplay::Column;

        file_trickplay::Entity::insert(row_of(record)?)
            .on_conflict(
                OnConflict::column(Column::FileId)
                    .update_columns([
                        Column::FileHash,
                        Column::IntervalMs,
                        Column::TileWidth,
                        Column::TileHeight,
                        Column::Columns,
                        Column::Rows,
                        Column::TileCount,
                        Column::GeneratedAt,
                    ])
                    .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;
        Ok(())
    }

    async fn find(&self, file_id: Uuid) -> Result<Option<TrickplayRecord>, DbErr> {
        file_trickplay::Entity::find_by_id(file_id)
            .one(self.db.as_ref())
            .await?
            .map(record_of)
            .transpose()
    }

    async fn find_by_file_ids(&self, file_ids: &[Uuid]) -> Result<Vec<TrickplayRecord>, DbErr> {
        if file_ids.is_empty() {
            return Ok(Vec::new());
        }
        file_trickplay::Entity::find()
            .filter(file_trickplay::Column::FileId.is_in(file_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(record_of)
            .collect()
    }
}
