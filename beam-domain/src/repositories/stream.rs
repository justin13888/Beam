use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::stream::{CreateMediaStream, MediaStream};

#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait MediaStreamRepository: Send + Sync + std::fmt::Debug {
    async fn insert_streams(&self, streams: Vec<CreateMediaStream>) -> Result<u32, DbErr>;
    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<MediaStream>, DbErr>;
    /// Delete every stream belonging to a file. Used when reconciling a changed
    /// file replaces its stream set. Returns the number of rows removed.
    async fn delete_by_file_id(&self, file_id: Uuid) -> Result<u64, DbErr>;
}

#[mutants::skip]
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
            let mut streams = self
                .streams
                .lock()
                .unwrap()
                .get(&file_id)
                .cloned()
                .unwrap_or_default();
            streams.sort_by_key(|s| s.index);
            Ok(streams)
        }

        async fn delete_by_file_id(&self, file_id: Uuid) -> Result<u64, DbErr> {
            Ok(self
                .streams
                .lock()
                .unwrap()
                .remove(&file_id)
                .map(|v| v.len() as u64)
                .unwrap_or(0))
        }
    }
}
