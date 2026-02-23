use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use crate::models::domain::{CreateMediaStream, MediaStream};

/// Repository for managing media stream persistence operations.
///
/// This trait defines the data access layer for media streams (video, audio, subtitle tracks)
/// within media files. Streams represent the individual tracks contained in a media container.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait MediaStreamRepository: Send + Sync + std::fmt::Debug {
    /// Inserts multiple media streams for a file in a single operation.
    ///
    /// This is the primary method for storing stream metadata after scanning a media file.
    /// It's optimized for batch insertion of all streams from a single file.
    ///
    /// # Arguments
    ///
    /// * `streams` - A vector of stream creation parameters
    ///
    /// # Returns
    ///
    /// The number of streams successfully inserted, or a database error if insertion fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use beam_stream::repositories::MediaStreamRepository;
    /// # use beam_stream::models::domain::{CreateMediaStream, StreamType, StreamMetadata, VideoStreamMetadata};
    /// # use uuid::Uuid;
    /// # async fn example(repo: &dyn MediaStreamRepository, file_id: Uuid) -> Result<(), sea_orm::DbErr> {
    /// let streams = vec![
    ///     CreateMediaStream {
    ///         file_id,
    ///         index: 0,
    ///         stream_type: StreamType::Video,
    ///         codec: "h264".to_string(),
    ///         metadata: StreamMetadata::Video(VideoStreamMetadata {
    ///             width: 1920,
    ///             height: 1080,
    ///             frame_rate: Some(24.0),
    ///             bit_rate: Some(5_000_000),
    ///             color_space: None,
    ///             color_range: None,
    ///             hdr_format: None,
    ///         }),
    ///     },
    ///     // ... more streams
    /// ];
    /// let count = repo.insert_streams(streams).await?;
    /// # Ok(())
    /// # }
    /// ```
    async fn insert_streams(&self, streams: Vec<CreateMediaStream>) -> Result<u32, DbErr>;

    /// Retrieves all media streams for a specific file.
    ///
    /// # Arguments
    ///
    /// * `file_id` - The UUID of the file
    ///
    /// # Returns
    ///
    /// A vector of all streams belonging to the file, ordered by stream index,
    /// or a database error if the query fails.
    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<MediaStream>, DbErr>;
}

/// SQL-based implementation of the MediaStreamRepository trait.
///
/// Uses SeaORM to interact with a relational database (PostgreSQL).
#[derive(Debug, Clone)]
pub struct SqlMediaStreamRepository {
    db: DatabaseConnection,
}

impl SqlMediaStreamRepository {
    /// Creates a new SQL media stream repository.
    ///
    /// # Arguments
    ///
    /// * `db` - The database connection to use for all operations
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl MediaStreamRepository for SqlMediaStreamRepository {
    async fn insert_streams(&self, streams: Vec<CreateMediaStream>) -> Result<u32, DbErr> {
        use beam_entity::media_stream;
        use crate::models::domain::{StreamMetadata, StreamType};
        use sea_orm::{ActiveModelTrait, Set};

        let mut inserted_count = 0;

        for stream in streams {
            let db_stream_type = match stream.stream_type {
                StreamType::Video => media_stream::StreamType::Video,
                StreamType::Audio => media_stream::StreamType::Audio,
                StreamType::Subtitle => media_stream::StreamType::Subtitle,
            };

            let (
                language,
                title,
                width,
                height,
                frame_rate,
                bit_rate,
                channels,
                sample_rate,
                channel_layout,
                color_space,
                color_range,
                hdr_format,
                is_default,
                is_forced,
            ) = match stream.metadata {
                StreamMetadata::Video(v) => (
                    None,
                    None,
                    Some(v.width as i32),
                    Some(v.height as i32),
                    v.frame_rate,
                    v.bit_rate.map(|b| b as i64),
                    None,
                    None,
                    None,
                    v.color_space,
                    v.color_range,
                    v.hdr_format,
                    false,
                    false,
                ),
                StreamMetadata::Audio(a) => (
                    a.language,
                    a.title,
                    None,
                    None,
                    None,
                    a.bit_rate.map(|b| b as i64),
                    Some(a.channels as i32),
                    Some(a.sample_rate as i32),
                    a.channel_layout,
                    None,
                    None,
                    None,
                    a.is_default,
                    a.is_forced,
                ),
                StreamMetadata::Subtitle(s) => (
                    s.language,
                    s.title,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    s.is_default,
                    s.is_forced,
                ),
            };

            let new_stream = media_stream::ActiveModel {
                id: Set(Uuid::new_v4()),
                file_id: Set(stream.file_id),
                stream_index: Set(stream.index as i32),
                stream_type: Set(db_stream_type),
                codec: Set(stream.codec),
                language: Set(language),
                title: Set(title),
                is_default: Set(is_default),
                is_forced: Set(is_forced),
                width: Set(width),
                height: Set(height),
                frame_rate: Set(frame_rate),
                bit_rate: Set(bit_rate),
                color_space: Set(color_space),
                color_range: Set(color_range),
                hdr_format: Set(hdr_format),
                channels: Set(channels),
                sample_rate: Set(sample_rate),
                channel_layout: Set(channel_layout),
            };

            new_stream.insert(&self.db).await?;
            inserted_count += 1;
        }

        Ok(inserted_count)
    }

    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<MediaStream>, DbErr> {
        use beam_entity::media_stream;
        use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};

        let models = media_stream::Entity::find()
            .filter(media_stream::Column::FileId.eq(file_id))
            .order_by(media_stream::Column::StreamIndex, Order::Asc)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(MediaStream::from).collect())
    }
}
