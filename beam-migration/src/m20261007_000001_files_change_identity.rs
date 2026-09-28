use sea_orm_migration::prelude::*;

/// Adds `files.inode` and `files.ctime` -- what a rename or a replace always
/// changes about the file at a path (issue #228).
///
/// A scan hashes a file only when what a stat says differs from its row. Size
/// and mtime alone miss a swap or a rotation of files of one size written
/// within one timestamp tick, or copied by a tool that keeps mtimes: each
/// path still matches its own row, so the content-hash relink (FR-221) never
/// sees the move. An inode cannot survive a rename over a path, and a ctime
/// cannot be set back by any copy tool.
///
/// Both nullable with no default, and written together: a `CHECK` keeps a row
/// from holding one without the other. Existing rows read as having no
/// identity, which the indexer treats as "nothing to compare"; the next scan
/// that finds such a file at its recorded size and mtime records its identity
/// without hashing it.
///
/// `down()` drops the constraint and both columns.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE files ADD COLUMN inode BIGINT")
            .await?;
        db.execute_unprepared("ALTER TABLE files ADD COLUMN ctime TIMESTAMPTZ")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE files ADD CONSTRAINT chk_files_identity_whole \
             CHECK ((inode IS NULL) = (ctime IS NULL))",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE files DROP CONSTRAINT chk_files_identity_whole")
            .await?;
        db.execute_unprepared("ALTER TABLE files DROP COLUMN ctime")
            .await?;
        db.execute_unprepared("ALTER TABLE files DROP COLUMN inode")
            .await?;

        Ok(())
    }
}
