//! SQL implementation of [`MarkerRepository`] (issue #332).
//!
//! A file's kept markers are `file_markers` rows keyed by the file and the
//! kind, so an administrator's set is one `INSERT ... ON CONFLICT DO UPDATE`.
//! A detection is one transaction: the file's detected rows deleted, the new
//! ones inserted with `ON CONFLICT DO NOTHING` -- so a kind an administrator
//! set keeps its row -- and the file's `file_marker_detections` row upserted.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, DatabaseConnection, DbErr, EntityTrait, Order, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use uuid::Uuid;

use beam_domain::models::chapter::{
    MarkerDetection, MarkerKind, PlaybackMarker, StoredMarker, StoredMarkerSource,
};
use beam_domain::repositories::MarkerRepository;
use beam_entity::file_markers;
use beam_entity::file_markers::detection;

/// SQL-based implementation of the MarkerRepository trait.
#[derive(Debug, Clone)]
pub struct SqlMarkerRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlMarkerRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

/// The `kind` column's value for `kind`.
fn kind_key(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::Recap => "recap",
        MarkerKind::Intro => "intro",
        MarkerKind::Credits => "credits",
        MarkerKind::Preview => "preview",
    }
}

/// The `source` column's value for `source`.
fn source_key(source: StoredMarkerSource) -> &'static str {
    match source {
        StoredMarkerSource::Admin => "admin",
        StoredMarkerSource::Detected => "detected",
    }
}

/// A time as the table holds it. A marker past `i64::MAX` milliseconds is
/// not one any file can hold.
fn stored_ms(ms: u64) -> Result<i64, DbErr> {
    i64::try_from(ms).map_err(|_| DbErr::Custom(format!("a marker time of {ms} ms")))
}

fn row_of(
    file_id: Uuid,
    marker: PlaybackMarker,
    source: StoredMarkerSource,
) -> Result<file_markers::ActiveModel, DbErr> {
    let PlaybackMarker {
        kind,
        start_ms,
        end_ms,
    } = marker;
    Ok(file_markers::ActiveModel {
        file_id: Set(file_id),
        kind: Set(kind_key(kind).to_owned()),
        start_ms: Set(stored_ms(start_ms)?),
        end_ms: Set(stored_ms(end_ms)?),
        source: Set(source_key(source).to_owned()),
    })
}

/// The marker a row holds. A value the table's `CHECK`s refuse is an error
/// rather than a marker read some other way.
fn marker_of(row: file_markers::Model) -> Result<StoredMarker, DbErr> {
    let file_markers::Model {
        file_id,
        kind,
        start_ms,
        end_ms,
        source,
    } = row;
    let invalid = || DbErr::Custom(format!("file {file_id} has an invalid marker"));
    let kind = MarkerKind::ALL
        .into_iter()
        .find(|candidate| kind_key(*candidate) == kind)
        .ok_or_else(invalid)?;
    let source = [StoredMarkerSource::Admin, StoredMarkerSource::Detected]
        .into_iter()
        .find(|candidate| source_key(*candidate) == source)
        .ok_or_else(invalid)?;
    Ok(StoredMarker {
        marker: PlaybackMarker {
            kind,
            start_ms: u64::try_from(start_ms).map_err(|_| invalid())?,
            end_ms: u64::try_from(end_ms).map_err(|_| invalid())?,
        },
        source,
    })
}

#[async_trait]
impl MarkerRepository for SqlMarkerRepository {
    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<StoredMarker>, DbErr> {
        file_markers::Entity::find()
            .filter(file_markers::Column::FileId.eq(file_id))
            .order_by(file_markers::Column::StartMs, Order::Asc)
            .order_by(file_markers::Column::EndMs, Order::Asc)
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(marker_of)
            .collect()
    }

    async fn set_admin(&self, file_id: Uuid, marker: PlaybackMarker) -> Result<(), DbErr> {
        use file_markers::Column;

        file_markers::Entity::insert(row_of(file_id, marker, StoredMarkerSource::Admin)?)
            .on_conflict(
                OnConflict::columns([Column::FileId, Column::Kind])
                    .update_columns([Column::StartMs, Column::EndMs, Column::Source])
                    .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;
        Ok(())
    }

    async fn clear(&self, file_id: Uuid, kind: MarkerKind) -> Result<bool, DbErr> {
        let deleted = file_markers::Entity::delete_many()
            .filter(file_markers::Column::FileId.eq(file_id))
            .filter(file_markers::Column::Kind.eq(kind_key(kind)))
            .exec(self.db.as_ref())
            .await?;
        Ok(deleted.rows_affected > 0)
    }

    async fn record_detection(&self, detection: MarkerDetection) -> Result<(), DbErr> {
        use file_markers::Column;

        let MarkerDetection {
            file_id,
            file_hash,
            markers,
            detected_at,
        } = detection;
        let rows = markers
            .into_iter()
            .map(|marker| row_of(file_id, marker, StoredMarkerSource::Detected))
            .collect::<Result<Vec<_>, _>>()?;

        let txn = self.db.begin().await?;
        file_markers::Entity::delete_many()
            .filter(Column::FileId.eq(file_id))
            .filter(Column::Source.eq(source_key(StoredMarkerSource::Detected)))
            .exec(&txn)
            .await?;
        if !rows.is_empty() {
            // A kind an administrator set keeps its row.
            file_markers::Entity::insert_many(rows)
                .on_conflict_do_nothing_on([Column::FileId, Column::Kind])
                .exec_without_returning(&txn)
                .await?;
        }
        detection::Entity::insert(detection::ActiveModel {
            file_id: Set(file_id),
            // The hash's bits, as `files.hash_xxh3` holds them.
            file_hash: Set(file_hash as i64),
            detected_at: Set(detected_at.into()),
        })
        .on_conflict(
            OnConflict::column(detection::Column::FileId)
                .update_columns([detection::Column::FileHash, detection::Column::DetectedAt])
                .to_owned(),
        )
        .exec_without_returning(&txn)
        .await?;
        txn.commit().await
    }

    async fn detected_hashes(&self, file_ids: &[Uuid]) -> Result<HashMap<Uuid, u64>, DbErr> {
        if file_ids.is_empty() {
            return Ok(HashMap::new());
        }
        Ok(detection::Entity::find()
            .filter(detection::Column::FileId.is_in(file_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|row| (row.file_id, row.file_hash as u64))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind and source reads back as the one written: the column
    /// values name each one, and no two share a value.
    #[test]
    fn every_kind_and_source_reads_back_from_its_row() {
        let file_id = Uuid::new_v4();
        for kind in MarkerKind::ALL {
            for source in [StoredMarkerSource::Admin, StoredMarkerSource::Detected] {
                let marker = PlaybackMarker {
                    kind,
                    start_ms: 1_000,
                    end_ms: 2_000,
                };
                let row = row_of(file_id, marker, source).unwrap();
                let model = file_markers::Model {
                    file_id: row.file_id.unwrap(),
                    kind: row.kind.unwrap(),
                    start_ms: row.start_ms.unwrap(),
                    end_ms: row.end_ms.unwrap(),
                    source: row.source.unwrap(),
                };
                assert_eq!(
                    marker_of(model).unwrap(),
                    StoredMarker { marker, source },
                    "{kind:?} {source:?}"
                );
            }
        }
    }

    #[test]
    fn a_row_the_checks_refuse_is_an_error_not_a_marker() {
        let row = |kind: &str, start_ms: i64, source: &str| file_markers::Model {
            file_id: Uuid::new_v4(),
            kind: kind.to_owned(),
            start_ms,
            end_ms: 2_000,
            source: source.to_owned(),
        };
        assert!(marker_of(row("opening", 0, "admin")).is_err());
        assert!(marker_of(row("intro", 0, "user")).is_err());
        assert!(marker_of(row("intro", -1, "admin")).is_err());
        assert!(marker_of(row("intro", 0, "admin")).is_ok());
    }

    /// The detection's transaction deletes only the file's detected rows,
    /// then inserts the new ones without touching a kind already kept, then
    /// remembers the version heard.
    #[tokio::test]
    async fn a_detection_replaces_only_detected_rows_and_leaves_an_admins_kind() {
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
            ])
            .into_connection();
        let db = Arc::new(db);
        let repo = SqlMarkerRepository::new(db.clone());
        let file_id = Uuid::new_v4();

        repo.record_detection(MarkerDetection {
            file_id,
            file_hash: 7,
            markers: vec![PlaybackMarker {
                kind: MarkerKind::Intro,
                start_ms: 1_000,
                end_ms: 60_000,
            }],
            detected_at: chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
        })
        .await
        .unwrap();
        drop(repo);

        let log = Arc::try_unwrap(db)
            .expect("the repository is dropped")
            .into_transaction_log();
        let statements: Vec<String> = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.clone())
            .collect();
        let delete = statements
            .iter()
            .find(|sql| sql.starts_with("DELETE"))
            .expect("the detected rows are deleted");
        assert!(delete.contains(r#""file_markers""#), "{delete}");
        assert!(delete.contains(r#""source" = "#), "{delete}");
        let insert = statements
            .iter()
            .find(|sql| sql.contains(r#"INSERT INTO "file_markers""#))
            .expect("the new rows are inserted");
        assert!(insert.contains("ON CONFLICT"), "{insert}");
        assert!(insert.contains("DO NOTHING"), "{insert}");
        let remembered = statements
            .iter()
            .find(|sql| sql.contains(r#"INSERT INTO "file_marker_detections""#))
            .expect("the version heard is remembered");
        assert!(remembered.contains("DO UPDATE"), "{remembered}");
    }
}
