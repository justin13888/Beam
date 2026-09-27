use sea_orm_migration::prelude::*;

/// One `files` row per path (issue #181).
///
/// The only uniqueness `files` had was `(hash_xxh3, file_path)`, so two tasks
/// reconciling one library at once -- a scan and a watcher event, two
/// overlapping scans -- could each insert a row for the same path, with
/// different hashes if the file was still being written. The indexer now
/// serialises every scan of a library, and this constraint makes a second
/// row for a path impossible whatever writes it.
///
/// A path already stored more than once keeps one row, ranked in order:
/// a present row before a missing one, a `known` row before any other
/// status, the row with the most playback progress, the most recently
/// updated, then the lowest id. Playback progress on the rows merged away
/// moves to the kept row -- per user, the most recently updated progress
/// wins -- and the rows themselves are deleted, taking their media streams
/// with them.
///
/// `down()` restores the old `(hash, path)` index. The merge is not undone:
/// which of the duplicate rows a progress row belonged to was never
/// meaningful, and the rows merged away were copies of one file.
#[derive(DeriveMigrationName)]
pub struct Migration;

/// Every `files` row with the id of the row its path keeps. Materialised
/// once, so the ranking -- which counts playback progress -- is not
/// recomputed after progress starts moving.
const RANK_DUPLICATES: &str = "\
    CREATE TEMPORARY TABLE files_path_keep AS \
    SELECT f.id, first_value(f.id) OVER ( \
               PARTITION BY f.file_path \
               ORDER BY (f.missing_since IS NULL) DESC, \
                        (f.file_status = 'known') DESC, \
                        (SELECT count(*) FROM playback_progress p WHERE p.file_id = f.id) DESC, \
                        f.updated_at DESC, \
                        f.id) AS keep \
      FROM files f \
     WHERE f.file_path IN (SELECT file_path FROM files GROUP BY file_path HAVING count(*) > 1)";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(RANK_DUPLICATES).await?;

        // Per user, only the most recently updated progress across a path's
        // rows survives; it then moves to the kept row, so the
        // `(user_id, file_id)` unique index is never violated.
        db.execute_unprepared(
            "DELETE FROM playback_progress p \
              USING (SELECT p.id, row_number() OVER ( \
                                  PARTITION BY p.user_id, k.keep \
                                  ORDER BY p.updated_at DESC, p.id) AS newest \
                       FROM playback_progress p \
                       JOIN files_path_keep k ON k.id = p.file_id) ranked \
              WHERE p.id = ranked.id AND ranked.newest > 1",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE playback_progress p SET file_id = k.keep \
               FROM files_path_keep k \
              WHERE p.file_id = k.id AND k.id <> k.keep",
        )
        .await?;
        // `media_streams` follows its file through `ON DELETE CASCADE`.
        db.execute_unprepared(
            "DELETE FROM files f \
              USING files_path_keep k \
              WHERE f.id = k.id AND k.id <> k.keep",
        )
        .await?;
        db.execute_unprepared("DROP TABLE files_path_keep").await?;

        // `idx_files_hash` still serves lookups by hash; the `(hash, path)`
        // index is superseded by the path alone.
        db.execute_unprepared("DROP INDEX idx_files_unique").await?;
        db.execute_unprepared("CREATE UNIQUE INDEX idx_files_path_unique ON files (file_path)")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP INDEX idx_files_path_unique")
            .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX idx_files_unique ON files (hash_xxh3, file_path)",
        )
        .await?;

        Ok(())
    }
}
