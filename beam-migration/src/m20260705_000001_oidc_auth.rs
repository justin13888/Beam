use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── users: additive OIDC identity columns ──────────────────────────
        // Kept alongside the existing password columns for now -- the OIDC
        // BFF flow (this migration) coexists with legacy password auth until
        // the auth cutover (see ADR-0003) deletes the password machinery and
        // drops the now-unneeded legacy columns.
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(Users::OidcIssuer).string())
                    .add_column(ColumnDef::new(Users::OidcSubject).string())
                    .add_column(ColumnDef::new(Users::DisplayName).string())
                    .add_column(ColumnDef::new(Users::AvatarUrl).string())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_users_oidc_identity")
                    .table(Users::Table)
                    .col(Users::OidcIssuer)
                    .col(Users::OidcSubject)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ── sessions: redesign for hash-at-rest + idle/absolute expiry ─────
        // Pre-alpha, no session data worth preserving (ADR-0005/ADR-0003):
        // drop and recreate rather than a piecemeal ALTER sequence. The
        // plaintext opaque token is now never stored -- only its SHA-256
        // hash -- and a stable internal `id` is added so sessions can be
        // listed/revoked without ever re-exposing the credential itself.
        // Serves both the legacy password-JWT-refresh flow and the new OIDC
        // BFF cookie flow.
        manager
            .drop_table(Table::drop().table(Sessions::Table).to_owned())
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .col(ColumnDef::new(Sessions::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Sessions::UserId).uuid().not_null())
                    .col(
                        ColumnDef::new(Sessions::TokenHash)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Sessions::DeviceHash).string().not_null())
                    .col(ColumnDef::new(Sessions::Ip).string().not_null())
                    .col(
                        ColumnDef::new(Sessions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Sessions::LastActive)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Sessions::IdleExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Sessions::AbsoluteExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sessions_user_id")
                            .from(Sessions::Table, Sessions::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_sessions_user_id")
                    .table(Sessions::Table)
                    .col(Sessions::UserId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_sessions_idle_expires_at")
                    .table(Sessions::Table)
                    .col(Sessions::IdleExpiresAt)
                    .to_owned(),
            )
            .await?;

        // ── pending_auths: single-use OIDC state/nonce/PKCE round-trip ─────
        manager
            .create_table(
                Table::create()
                    .table(PendingAuths::Table)
                    .col(
                        ColumnDef::new(PendingAuths::State)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PendingAuths::Nonce).string().not_null())
                    .col(
                        ColumnDef::new(PendingAuths::PkceVerifier)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PendingAuths::RedirectPath).string())
                    .col(
                        ColumnDef::new(PendingAuths::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PendingAuths::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_pending_auths_expires_at")
                    .table(PendingAuths::Table)
                    .col(PendingAuths::ExpiresAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PendingAuths::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Sessions::Table).to_owned())
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .col(
                        ColumnDef::new(Sessions::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Sessions::UserId).uuid().not_null())
                    .col(ColumnDef::new(Sessions::DeviceHash).string().not_null())
                    .col(ColumnDef::new(Sessions::Ip).string().not_null())
                    .col(
                        ColumnDef::new(Sessions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Sessions::LastActive)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("expires_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sessions_user_id")
                            .from(Sessions::Table, Sessions::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::OidcIssuer)
                    .drop_column(Users::OidcSubject)
                    .drop_column(Users::DisplayName)
                    .drop_column(Users::AvatarUrl)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    OidcIssuer,
    OidcSubject,
    DisplayName,
    AvatarUrl,
}

#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
    UserId,
    TokenHash,
    DeviceHash,
    Ip,
    CreatedAt,
    LastActive,
    IdleExpiresAt,
    AbsoluteExpiresAt,
}

#[derive(DeriveIden)]
enum PendingAuths {
    Table,
    State,
    Nonce,
    PkceVerifier,
    RedirectPath,
    CreatedAt,
    ExpiresAt,
}
