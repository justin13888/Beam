use sea_orm_migration::prelude::*;

/// Adds `device_auths` -- the in-flight RFC 8628 device authorization grants
/// a native client with no browser signs in through (issue #151, ADR-0017).
///
/// One row per started device login, keyed by the SHA-256 of the opaque
/// handle the client polls with; the handle itself is never stored. The row
/// holds the IdP's device code, which never leaves the server, and the
/// per-flow poll pacing (`interval_secs`, `next_poll_at`). It is deleted when
/// the flow ends, and rows that outlive `expires_at` are swept whenever a new
/// flow starts -- the `expires_at` index serves that sweep. Follows the
/// `pending_auths` precedent (ADR-0005): short-lived auth state lives in
/// Postgres beside the sessions it turns into.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(DeviceAuths::Table)
                    .col(
                        ColumnDef::new(DeviceAuths::HandleHash)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(DeviceAuths::DeviceCode).text().not_null())
                    .col(ColumnDef::new(DeviceAuths::UserCode).text().not_null())
                    .col(
                        ColumnDef::new(DeviceAuths::VerificationUri)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DeviceAuths::VerificationUriComplete).text())
                    .col(
                        ColumnDef::new(DeviceAuths::IntervalSecs)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceAuths::NextPollAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceAuths::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceAuths::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_device_auths_expires_at")
                    .table(DeviceAuths::Table)
                    .col(DeviceAuths::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DeviceAuths::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum DeviceAuths {
    Table,
    HandleHash,
    DeviceCode,
    UserCode,
    VerificationUri,
    VerificationUriComplete,
    IntervalSecs,
    NextPollAt,
    CreatedAt,
    ExpiresAt,
}
