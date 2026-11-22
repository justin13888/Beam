use std::collections::HashMap;

use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::chapter::{MarkerDetection, MarkerKind, PlaybackMarker, StoredMarker};

/// The markers Beam keeps for each file (issue #332), backing the
/// `file_markers` and `file_marker_detections` tables: at most one marker of
/// each kind per file, set by an administrator or found by the background
/// detector, and the version of each file the detector last listened to.
///
/// An administrator's marker outranks a detected one: setting one replaces
/// the detected marker of its kind, and no detection replaces it. Both go
/// with their file when its row is purged.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait MarkerRepository: Send + Sync + std::fmt::Debug {
    /// The markers kept for the file `file_id`, in the order they start;
    /// none for a file with none, and for an id no file has.
    async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<StoredMarker>, DbErr>;

    /// Keep `marker` as the administrator's marker of its kind for the file
    /// `file_id`, replacing whatever marker of that kind it had.
    async fn set_admin(&self, file_id: Uuid, marker: PlaybackMarker) -> Result<(), DbErr>;

    /// Stop keeping the file's marker of `kind`, whoever set it; whether it
    /// had one. The file's other markers are untouched.
    async fn clear(&self, file_id: Uuid, kind: MarkerKind) -> Result<bool, DbErr>;

    /// Record what one detection of a file found, whole and atomically: the
    /// file's detected markers become `detection.markers` -- less any kind an
    /// administrator set, which is kept -- and the file is remembered as
    /// detected for `detection.file_hash`.
    async fn record_detection(&self, detection: MarkerDetection) -> Result<(), DbErr>;

    /// The content hash each of `file_ids` was last detected for; a file
    /// never detected is absent. An empty list finds nothing and issues no
    /// statement.
    async fn detected_hashes(&self, file_ids: &[Uuid]) -> Result<HashMap<Uuid, u64>, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::chapter::StoredMarkerSource;
    use parking_lot::RwLock;

    #[derive(Debug, Default)]
    struct Stored {
        markers: HashMap<Uuid, Vec<StoredMarker>>,
        detections: HashMap<Uuid, u64>,
    }

    /// The in-memory double, keyed by file as the tables are. It knows
    /// nothing of files, so purging one does not take its markers with it
    /// here; the `pg-integration` tier asserts that cascade.
    #[derive(Debug, Default)]
    pub struct InMemoryMarkerRepository {
        stored: RwLock<Stored>,
    }

    #[async_trait]
    impl MarkerRepository for InMemoryMarkerRepository {
        async fn find_by_file_id(&self, file_id: Uuid) -> Result<Vec<StoredMarker>, DbErr> {
            let mut markers = self
                .stored
                .read()
                .markers
                .get(&file_id)
                .cloned()
                .unwrap_or_default();
            markers.sort_by_key(|stored| (stored.marker.start_ms, stored.marker.end_ms));
            Ok(markers)
        }

        async fn set_admin(&self, file_id: Uuid, marker: PlaybackMarker) -> Result<(), DbErr> {
            let mut stored = self.stored.write();
            let markers = stored.markers.entry(file_id).or_default();
            markers.retain(|kept| kept.marker.kind != marker.kind);
            markers.push(StoredMarker {
                marker,
                source: StoredMarkerSource::Admin,
            });
            Ok(())
        }

        async fn clear(&self, file_id: Uuid, kind: MarkerKind) -> Result<bool, DbErr> {
            let mut stored = self.stored.write();
            let Some(markers) = stored.markers.get_mut(&file_id) else {
                return Ok(false);
            };
            let before = markers.len();
            markers.retain(|kept| kept.marker.kind != kind);
            Ok(markers.len() != before)
        }

        async fn record_detection(&self, detection: MarkerDetection) -> Result<(), DbErr> {
            let MarkerDetection {
                file_id,
                file_hash,
                markers,
                detected_at: _,
            } = detection;
            let mut stored = self.stored.write();
            let kept = stored.markers.entry(file_id).or_default();
            kept.retain(|kept| kept.source == StoredMarkerSource::Admin);
            for marker in markers {
                if kept.iter().all(|kept| kept.marker.kind != marker.kind) {
                    kept.push(StoredMarker {
                        marker,
                        source: StoredMarkerSource::Detected,
                    });
                }
            }
            stored.detections.insert(file_id, file_hash);
            Ok(())
        }

        async fn detected_hashes(&self, file_ids: &[Uuid]) -> Result<HashMap<Uuid, u64>, DbErr> {
            let stored = self.stored.read();
            Ok(file_ids
                .iter()
                .filter_map(|id| stored.detections.get(id).map(|hash| (*id, *hash)))
                .collect())
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use uuid::Uuid;

    use super::MarkerRepository;
    use super::in_memory::InMemoryMarkerRepository;
    use crate::repositories::contract::fixture::MarkerFixture;

    /// The hermetic instantiation of the marker contract: no foreign key to
    /// satisfy, so every file id is fresh.
    #[derive(Debug, Default)]
    pub struct InMemoryFixture {
        repo: InMemoryMarkerRepository,
    }

    #[async_trait::async_trait]
    impl MarkerFixture for InMemoryFixture {
        fn repo(&self) -> &dyn MarkerRepository {
            &self.repo
        }

        async fn new_file(&self) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::marker_repository_contract!(setup);
}
