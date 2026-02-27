use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use crate::models::domain::{CreateMediaStream, MediaStream};

/// Repository for managing media stream persistence operations.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait MediaStreamRepository: Send + Sync + std::fmt::Debug {
    async fn insert_streams(&self, streams: Vec<CreateMediaStream>) -> Result<u32, DbErr>;
    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<MediaStream>, DbErr>;
}

/// SQL-based implementation of the MediaStreamRepository trait.
#[derive(Debug, Clone)]
pub struct SqlMediaStreamRepository {
    db: DatabaseConnection,
}

impl SqlMediaStreamRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl MediaStreamRepository for SqlMediaStreamRepository {
    async fn insert_streams(&self, streams: Vec<CreateMediaStream>) -> Result<u32, DbErr> {
        use crate::models::domain::{StreamMetadata, StreamType};
        use beam_entity::media_stream;
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

/// In-memory implementation for use in tests and test-utils consumers.
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct InMemoryMediaStreamRepository {
        pub streams: Mutex<HashMap<Uuid, Vec<MediaStream>>>,
    }

    #[async_trait]
    impl MediaStreamRepository for InMemoryMediaStreamRepository {
        async fn insert_streams(&self, streams: Vec<CreateMediaStream>) -> Result<u32, DbErr> {
            let count = streams.len() as u32;
            for create in streams {
                let stream = MediaStream {
                    id: Uuid::new_v4(),
                    file_id: create.file_id,
                    index: create.index,
                    stream_type: create.stream_type,
                    codec: create.codec,
                    metadata: create.metadata,
                };
                self.streams
                    .lock()
                    .unwrap()
                    .entry(create.file_id)
                    .or_default()
                    .push(stream);
            }
            Ok(count)
        }

        async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<MediaStream>, DbErr> {
            Ok(self
                .streams
                .lock()
                .unwrap()
                .get(&file_id)
                .cloned()
                .unwrap_or_default())
        }
    }
}
