use sea_orm_migration::prelude::{extension::postgres::Type, *};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create ENUMs
        manager
            .create_type(
                Type::create()
                    .as_enum(StreamType::Table)
                    .values([StreamType::Video, StreamType::Audio, StreamType::Subtitle])
                    .to_owned(),
            )
            .await?;

        manager
            .create_type(
                Type::create()
                    .as_enum(FileStatus::Table)
                    .values([FileStatus::Known, FileStatus::Changed, FileStatus::Unknown])
                    .to_owned(),
            )
            .await?;

        // Create libraries table
        manager
            .create_table(
                Table::create()
                    .table(Libraries::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Libraries::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Libraries::Name).text().not_null())
                    .col(ColumnDef::new(Libraries::Description).text())
                    .col(
                        ColumnDef::new(Libraries::RootPath)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Libraries::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Libraries::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Libraries::LastScanStartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(Libraries::LastScanFinishedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(Libraries::LastScanFileCount).integer())
                    .to_owned(),
            )
            .await?;

        // Create movies table
        manager
            .create_table(
                Table::create()
                    .table(Movies::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Movies::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Movies::Title).text().not_null())
                    .col(ColumnDef::new(Movies::TitleLocalized).text())
                    .col(ColumnDef::new(Movies::Description).text())
                    .col(ColumnDef::new(Movies::Year).integer())
                    .col(ColumnDef::new(Movies::ReleaseDate).date())
                    .col(ColumnDef::new(Movies::RuntimeMins).integer())
                    .col(ColumnDef::new(Movies::PosterUrl).text())
                    .col(ColumnDef::new(Movies::BackdropUrl).text())
                    .col(ColumnDef::new(Movies::TmdbId).integer().unique_key())
                    .col(ColumnDef::new(Movies::ImdbId).text().unique_key())
                    .col(ColumnDef::new(Movies::TvdbId).integer().unique_key())
                    .col(ColumnDef::new(Movies::RatingTmdb).float())
                    .col(ColumnDef::new(Movies::RatingImdb).float())
                    .col(
                        ColumnDef::new(Movies::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Movies::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_movies_tmdb_id")
                    .table(Movies::Table)
                    .col(Movies::TmdbId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_movies_imdb_id")
                    .table(Movies::Table)
                    .col(Movies::ImdbId)
                    .to_owned(),
            )
            .await?;

        // Create shows table
        manager
            .create_table(
                Table::create()
                    .table(Shows::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Shows::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Shows::Title).text().not_null())
                    .col(ColumnDef::new(Shows::TitleLocalized).text())
                    .col(ColumnDef::new(Shows::Description).text())
                    .col(ColumnDef::new(Shows::Year).integer())
                    .col(ColumnDef::new(Shows::PosterUrl).text())
                    .col(ColumnDef::new(Shows::BackdropUrl).text())
                    .col(ColumnDef::new(Shows::TmdbId).integer().unique_key())
                    .col(ColumnDef::new(Shows::ImdbId).text().unique_key())
                    .col(ColumnDef::new(Shows::TvdbId).integer().unique_key())
                    .col(
                        ColumnDef::new(Shows::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Shows::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_shows_tmdb_id")
                    .table(Shows::Table)
                    .col(Shows::TmdbId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_shows_imdb_id")
                    .table(Shows::Table)
                    .col(Shows::ImdbId)
                    .to_owned(),
            )
            .await?;

        // Create library_movies junction
        manager
            .create_table(
                Table::create()
                    .table(LibraryMovies::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(LibraryMovies::LibraryId).uuid().not_null())
                    .col(ColumnDef::new(LibraryMovies::MovieId).uuid().not_null())
                    .primary_key(
                        Index::create()
                            .col(LibraryMovies::LibraryId)
                            .col(LibraryMovies::MovieId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(LibraryMovies::Table, LibraryMovies::LibraryId)
                            .to(Libraries::Table, Libraries::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(LibraryMovies::Table, LibraryMovies::MovieId)
                            .to(Movies::Table, Movies::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_library_movies_movie_id")
                    .table(LibraryMovies::Table)
                    .col(LibraryMovies::MovieId)
                    .to_owned(),
            )
            .await?;

        // Create library_shows junction
        manager
            .create_table(
                Table::create()
                    .table(LibraryShows::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(LibraryShows::LibraryId).uuid().not_null())
                    .col(ColumnDef::new(LibraryShows::ShowId).uuid().not_null())
                    .primary_key(
                        Index::create()
                            .col(LibraryShows::LibraryId)
                            .col(LibraryShows::ShowId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(LibraryShows::Table, LibraryShows::LibraryId)
                            .to(Libraries::Table, Libraries::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(LibraryShows::Table, LibraryShows::ShowId)
                            .to(Shows::Table, Shows::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_library_shows_show_id")
                    .table(LibraryShows::Table)
                    .col(LibraryShows::ShowId)
                    .to_owned(),
            )
            .await?;

        // Create seasons table
        manager
            .create_table(
                Table::create()
                    .table(Seasons::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Seasons::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Seasons::ShowId).uuid().not_null())
                    .col(ColumnDef::new(Seasons::SeasonNumber).integer().not_null())
                    .col(ColumnDef::new(Seasons::PosterUrl).text())
                    .col(ColumnDef::new(Seasons::FirstAired).date())
                    .col(ColumnDef::new(Seasons::LastAired).date())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Seasons::Table, Seasons::ShowId)
                            .to(Shows::Table, Shows::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_seasons_show_id")
                    .table(Seasons::Table)
                    .col(Seasons::ShowId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_seasons_unique")
                    .table(Seasons::Table)
                    .col(Seasons::ShowId)
                    .col(Seasons::SeasonNumber)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Create movie_entries table
        manager
            .create_table(
                Table::create()
                    .table(MovieEntries::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MovieEntries::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(MovieEntries::LibraryId).uuid().not_null())
                    .col(ColumnDef::new(MovieEntries::MovieId).uuid().not_null())
                    .col(ColumnDef::new(MovieEntries::Edition).text())
                    .col(
                        ColumnDef::new(MovieEntries::IsPrimary)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(MovieEntries::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MovieEntries::Table, MovieEntries::LibraryId)
                            .to(Libraries::Table, Libraries::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MovieEntries::Table, MovieEntries::MovieId)
                            .to(Movies::Table, Movies::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_movie_entries_library_id")
                    .table(MovieEntries::Table)
                    .col(MovieEntries::LibraryId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_movie_entries_movie_id")
                    .table(MovieEntries::Table)
                    .col(MovieEntries::MovieId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_movie_entries_unique")
                    .table(MovieEntries::Table)
                    .col(MovieEntries::LibraryId)
                    .col(MovieEntries::MovieId)
                    .col(MovieEntries::Edition)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Create episodes table
        manager
            .create_table(
                Table::create()
                    .table(Episodes::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Episodes::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Episodes::SeasonId).uuid().not_null())
                    .col(ColumnDef::new(Episodes::EpisodeNumber).integer().not_null())
                    .col(ColumnDef::new(Episodes::Title).text().not_null())
                    .col(ColumnDef::new(Episodes::Description).text())
                    .col(ColumnDef::new(Episodes::AirDate).date())
                    .col(ColumnDef::new(Episodes::RuntimeMins).integer())
                    .col(ColumnDef::new(Episodes::ThumbnailUrl).text())
                    .col(
                        ColumnDef::new(Episodes::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Episodes::Table, Episodes::SeasonId)
                            .to(Seasons::Table, Seasons::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_episodes_season_id")
                    .table(Episodes::Table)
                    .col(Episodes::SeasonId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_episodes_unique")
                    .table(Episodes::Table)
                    .col(Episodes::SeasonId)
                    .col(Episodes::EpisodeNumber)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Create files table
        manager
            .create_table(
                Table::create()
                    .table(Files::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Files::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Files::MovieEntryId).uuid())
                    .col(ColumnDef::new(Files::EpisodeId).uuid())
                    .col(ColumnDef::new(Files::LibraryId).uuid().not_null())
                    .col(ColumnDef::new(Files::FilePath).text().not_null())
                    .col(ColumnDef::new(Files::FileSize).big_integer().not_null())
                    .col(ColumnDef::new(Files::MimeType).text())
                    .col(ColumnDef::new(Files::HashXxh3).big_integer().not_null())
                    .col(ColumnDef::new(Files::DurationSecs).double())
                    .col(ColumnDef::new(Files::ContainerFormat).text())
                    .col(ColumnDef::new(Files::Language).text())
                    .col(ColumnDef::new(Files::Quality).text())
                    .col(ColumnDef::new(Files::ReleaseGroup).text())
                    .col(
                        ColumnDef::new(Files::IsPrimary)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Files::ScannedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Files::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Files::FileStatus)
                            .custom(FileStatus::Table)
                            .not_null()
                            .default("known"),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Files::Table, Files::MovieEntryId)
                            .to(MovieEntries::Table, MovieEntries::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Files::Table, Files::EpisodeId)
                            .to(Episodes::Table, Episodes::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Files::Table, Files::LibraryId)
                            .to(Libraries::Table, Libraries::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::cust(
                        "(file_status = 'unknown' AND movie_entry_id IS NULL AND episode_id IS NULL) OR \
                         (movie_entry_id IS NOT NULL AND episode_id IS NULL) OR \
                         (movie_entry_id IS NULL AND episode_id IS NOT NULL)",
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_files_movie_entry_id")
                    .table(Files::Table)
                    .col(Files::MovieEntryId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_files_episode_id")
                    .table(Files::Table)
                    .col(Files::EpisodeId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_files_library_id")
                    .table(Files::Table)
                    .col(Files::LibraryId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_files_hash")
                    .table(Files::Table)
                    .col(Files::HashXxh3)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_files_unique")
                    .table(Files::Table)
                    .col(Files::HashXxh3)
                    .col(Files::FilePath)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Create media_streams table
        manager
            .create_table(
                Table::create()
                    .table(MediaStreams::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MediaStreams::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(MediaStreams::FileId).uuid().not_null())
                    .col(
                        ColumnDef::new(MediaStreams::StreamIndex)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MediaStreams::StreamType)
                            .custom(StreamType::Table)
                            .not_null(),
                    )
                    .col(ColumnDef::new(MediaStreams::Codec).text().not_null())
                    .col(ColumnDef::new(MediaStreams::Language).text())
                    .col(ColumnDef::new(MediaStreams::Title).text())
                    .col(
                        ColumnDef::new(MediaStreams::IsDefault)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(MediaStreams::IsForced)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(MediaStreams::Width).integer())
                    .col(ColumnDef::new(MediaStreams::Height).integer())
                    .col(ColumnDef::new(MediaStreams::FrameRate).double())
                    .col(ColumnDef::new(MediaStreams::BitRate).big_integer())
                    .col(ColumnDef::new(MediaStreams::ColorSpace).text())
                    .col(ColumnDef::new(MediaStreams::ColorRange).text())
                    .col(ColumnDef::new(MediaStreams::HdrFormat).text())
                    .col(ColumnDef::new(MediaStreams::Channels).integer())
                    .col(ColumnDef::new(MediaStreams::SampleRate).integer())
                    .col(ColumnDef::new(MediaStreams::ChannelLayout).text())
                    .foreign_key(
                        ForeignKey::create()
                            .from(MediaStreams::Table, MediaStreams::FileId)
                            .to(Files::Table, Files::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_media_streams_file_id")
                    .table(MediaStreams::Table)
                    .col(MediaStreams::FileId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_media_streams_type")
                    .table(MediaStreams::Table)
                    .col(MediaStreams::StreamType)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_media_streams_language")
                    .table(MediaStreams::Table)
                    .col(MediaStreams::Language)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_media_streams_unique")
                    .table(MediaStreams::Table)
                    .col(MediaStreams::FileId)
                    .col(MediaStreams::StreamIndex)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Create genres table
        manager
            .create_table(
                Table::create()
                    .table(Genres::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Genres::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Genres::Name).text().not_null().unique_key())
                    .col(ColumnDef::new(Genres::Slug).text().not_null().unique_key())
                    .to_owned(),
            )
            .await?;

        // Create movie_genres junction
        manager
            .create_table(
                Table::create()
                    .table(MovieGenres::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(MovieGenres::MovieId).uuid().not_null())
                    .col(ColumnDef::new(MovieGenres::GenreId).uuid().not_null())
                    .primary_key(
                        Index::create()
                            .col(MovieGenres::MovieId)
                            .col(MovieGenres::GenreId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MovieGenres::Table, MovieGenres::MovieId)
                            .to(Movies::Table, Movies::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MovieGenres::Table, MovieGenres::GenreId)
                            .to(Genres::Table, Genres::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_movie_genres_genre_id")
                    .table(MovieGenres::Table)
                    .col(MovieGenres::GenreId)
                    .to_owned(),
            )
            .await?;

        // Create show_genres junction
        manager
            .create_table(
                Table::create()
                    .table(ShowGenres::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(ShowGenres::ShowId).uuid().not_null())
                    .col(ColumnDef::new(ShowGenres::GenreId).uuid().not_null())
                    .primary_key(
                        Index::create()
                            .col(ShowGenres::ShowId)
                            .col(ShowGenres::GenreId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ShowGenres::Table, ShowGenres::ShowId)
                            .to(Shows::Table, Shows::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ShowGenres::Table, ShowGenres::GenreId)
                            .to(Genres::Table, Genres::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_show_genres_genre_id")
                    .table(ShowGenres::Table)
                    .col(ShowGenres::GenreId)
                    .to_owned(),
            )
            .await?;

        // Create stream_cache table
        manager
            .create_table(
                Table::create()
                    .table(StreamCache::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(StreamCache::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(StreamCache::FileId).uuid().not_null())
                    .col(ColumnDef::new(StreamCache::TargetCodec).text().not_null())
                    .col(
                        ColumnDef::new(StreamCache::TargetContainer)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(StreamCache::TargetResolution).text())
                    .col(ColumnDef::new(StreamCache::TargetBitrate).big_integer())
                    .col(ColumnDef::new(StreamCache::HlsPlaylistPath).text())
                    .col(ColumnDef::new(StreamCache::CachePath).text().not_null())
                    .col(
                        ColumnDef::new(StreamCache::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(StreamCache::Table, StreamCache::FileId)
                            .to(Files::Table, Files::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_stream_cache_file_id")
                    .table(StreamCache::Table)
                    .col(StreamCache::FileId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop tables in reverse order
        manager
            .drop_table(Table::drop().table(StreamCache::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(ShowGenres::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(MovieGenres::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Genres::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(MediaStreams::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Files::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Episodes::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(MovieEntries::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Seasons::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(LibraryShows::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(LibraryMovies::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Shows::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Movies::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(Libraries::Table).to_owned())
            .await?;

        manager
            .drop_type(Type::drop().name(StreamType::Table).to_owned())
            .await?;

        manager
            .drop_type(Type::drop().name(FileStatus::Table).to_owned())
            .await?;

        Ok(())
    }
}

// Table identifiers
#[derive(DeriveIden)]
enum Libraries {
    Table,
    Id,
    Name,
    Description,
    RootPath,
    CreatedAt,
    UpdatedAt,
    LastScanStartedAt,
    LastScanFinishedAt,
    LastScanFileCount,
}

#[derive(DeriveIden)]
enum Movies {
    Table,
    Id,
    Title,
    TitleLocalized,
    Description,
    Year,
    ReleaseDate,
    RuntimeMins,
    PosterUrl,
    BackdropUrl,
    TmdbId,
    ImdbId,
    TvdbId,
    RatingTmdb,
    RatingImdb,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Shows {
    Table,
    Id,
    Title,
    TitleLocalized,
    Description,
    Year,
    PosterUrl,
    BackdropUrl,
    TmdbId,
    ImdbId,
    TvdbId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum LibraryMovies {
    Table,
    LibraryId,
    MovieId,
}

#[derive(DeriveIden)]
enum LibraryShows {
    Table,
    LibraryId,
    ShowId,
}

#[derive(DeriveIden)]
enum Seasons {
    Table,
    Id,
    ShowId,
    SeasonNumber,
    PosterUrl,
    FirstAired,
    LastAired,
}

#[derive(DeriveIden)]
enum MovieEntries {
    Table,
    Id,
    LibraryId,
    MovieId,
    Edition,
    IsPrimary,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Episodes {
    Table,
    Id,
    SeasonId,
    EpisodeNumber,
    Title,
    Description,
    AirDate,
    RuntimeMins,
    ThumbnailUrl,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Files {
    Table,
    Id,
    MovieEntryId,
    EpisodeId,
    LibraryId,
    FilePath,
    FileSize,
    MimeType,
    HashXxh3,
    DurationSecs,
    ContainerFormat,
    Language,
    Quality,
    ReleaseGroup,
    IsPrimary,
    ScannedAt,
    UpdatedAt,
    FileStatus,
}

#[derive(DeriveIden)]
enum MediaStreams {
    Table,
    Id,
    FileId,
    StreamIndex,
    StreamType,
    Codec,
    Language,
    Title,
    IsDefault,
    IsForced,
    Width,
    Height,
    FrameRate,
    BitRate,
    ColorSpace,
    ColorRange,
    HdrFormat,
    Channels,
    SampleRate,
    ChannelLayout,
}

#[derive(DeriveIden)]
enum StreamType {
    Table,
    #[sea_orm(iden = "video")]
    Video,
    #[sea_orm(iden = "audio")]
    Audio,
    #[sea_orm(iden = "subtitle")]
    Subtitle,
}

#[derive(DeriveIden)]
enum FileStatus {
    Table,
    #[sea_orm(iden = "known")]
    Known,
    #[sea_orm(iden = "changed")]
    Changed,
    #[sea_orm(iden = "unknown")]
    Unknown,
}

#[derive(DeriveIden)]
enum Genres {
    Table,
    Id,
    Name,
    Slug,
}

#[derive(DeriveIden)]
enum MovieGenres {
    Table,
    MovieId,
    GenreId,
}

#[derive(DeriveIden)]
enum ShowGenres {
    Table,
    ShowId,
    GenreId,
}

#[derive(DeriveIden)]
enum StreamCache {
    Table,
    Id,
    FileId,
    TargetCodec,
    TargetContainer,
    TargetResolution,
    TargetBitrate,
    HlsPlaylistPath,
    CachePath,
    CreatedAt,
}
