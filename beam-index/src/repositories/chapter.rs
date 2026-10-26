//! SQL implementation of [`ChapterRepository`] (issue #265).
//!
//! A file's chapters are replaced in one transaction -- its rows deleted,
//! the new set inserted -- so a reader sees the old set or the new one.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    ColumnTrait, DatabaseConnection, DbErr, EntityTrait, Order, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use uuid::Uuid;

use beam_domain::models::chapter::Chapter;
use beam_domain::repositories::ChapterRepository;
use beam_entity::file_chapter;

/// SQL-based implementation of the ChapterRepository trait.
#[derive(Debug, Clone)]
pub struct SqlChapterRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlChapterRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

/// A time as the table holds it. A chapter past `i64::MAX` milliseconds --
/// some 292 million years -- is not one a container can describe.
fn stored_ms(ms: u64) -> Result<i64, DbErr> {
    i64::try_from(ms).map_err(|_| DbErr::Custom(format!("a chapter time of {ms} ms")))
}

fn row_of(file_id: Uuid, chapter: Chapter) -> Result<file_chapter::ActiveModel, DbErr> {
    let Chapter {
        index,
        start_ms,
        end_ms,
        title,
    } = chapter;
    Ok(file_chapter::ActiveModel {
        file_id: Set(file_id),
        chapter_index: Set(
            i32::try_from(index).map_err(|_| DbErr::Custom(format!("chapter index {index}")))?
        ),
        start_ms: Set(stored_ms(start_ms)?),
        end_ms: Set(stored_ms(end_ms)?),
        title: Set(title),
    })
}

/// The chapter a row holds. A negative value is one the table's `CHECK`s
/// refuse, so it is an error rather than a chapter read some other way.
fn chapter_of(row: file_chapter::Model) -> Result<Chapter, DbErr> {
    let file_chapter::Model {
        file_id,
        chapter_index,
        start_ms,
        end_ms,
        title,
    } = row;
    let invalid = || DbErr::Custom(format!("file {file_id} has an invalid chapter"));
    Ok(Chapter {
        index: u32::try_from(chapter_index).map_err(|_| invalid())?,
        start_ms: u64::try_from(start_ms).map_err(|_| invalid())?,
        end_ms: u64::try_from(end_ms).map_err(|_| invalid())?,
        title,
    })
}

#[async_trait]
impl ChapterRepository for SqlChapterRepository {
    async fn replace_for_file(&self, file_id: Uuid, chapters: Vec<Chapter>) -> Result<(), DbErr> {
        let rows = chapters
            .into_iter()
            .map(|chapter| row_of(file_id, chapter))
            .collect::<Result<Vec<_>, _>>()?;

        let txn = self.db.begin().await?;
        file_chapter::Entity::delete_many()
            .filter(file_chapter::Column::FileId.eq(file_id))
            .exec(&txn)
            .await?;
        if !rows.is_empty() {
            file_chapter::Entity::insert_many(rows)
                .exec_without_returning(&txn)
                .await?;
        }
        txn.commit().await
    }

    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<Chapter>, DbErr> {
        file_chapter::Entity::find()
            .filter(file_chapter::Column::FileId.eq(file_id))
            .order_by(file_chapter::Column::ChapterIndex, Order::Asc)
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(chapter_of)
            .collect()
    }
}
