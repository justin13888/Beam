use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PlaybackProgress::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PlaybackProgress::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PlaybackProgress::UserId).uuid().not_null())
                    .col(ColumnDef::new(PlaybackProgress::FileId).uuid().not_null())
                    .col(
                        ColumnDef::new(PlaybackProgress::PositionSecs)
                            .double()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PlaybackProgress::DurationSecs).double())
                    .col(
                        ColumnDef::new(PlaybackProgress::Completed)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(PlaybackProgress::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_playback_progress_user_id")
                            .from(PlaybackProgress::Table, PlaybackProgress::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_playback_progress_file_id")
                            .from(PlaybackProgress::Table, PlaybackProgress::FileId)
                            .to(Files::Table, Files::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_playback_progress_user_file")
                    .table(PlaybackProgress::Table)
                    .col(PlaybackProgress::UserId)
                    .col(PlaybackProgress::FileId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_playback_progress_user_updated_at")
                    .table(PlaybackProgress::Table)
                    .col(PlaybackProgress::UserId)
                    .col(PlaybackProgress::UpdatedAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PlaybackProgress::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum PlaybackProgress {
    Table,
    Id,
    UserId,
    FileId,
    PositionSecs,
    DurationSecs,
    Completed,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Files {
    Table,
    Id,
}
