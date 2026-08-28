pub mod admin_log;
pub mod enrichment;
pub mod hash;
pub mod index;
pub mod media_info;
pub mod notification;
pub mod watcher;

// The clock seam lives in `beam-domain` (one canonical `Clock` for the whole
// workspace); re-exported here so `beam_index::services::Clock` keeps working.
#[cfg(any(test, feature = "test-utils"))]
pub use beam_domain::services::TestClock;
pub use beam_domain::services::{Clock, RealClock};

#[cfg(any(test, feature = "test-utils"))]
pub use admin_log::NoOpAdminLogService;
pub use admin_log::{AdminLogService, LocalAdminLogService};
pub use enrichment::{EnrichmentPolicy, MetadataEnrichmentService};
pub use hash::{HashConfig, HashService, LocalHashService};
#[cfg(any(test, feature = "test-utils"))]
pub use index::MockIndexService;
pub use index::{IndexError, IndexService, LocalIndexService};
pub use media_info::{LocalMediaInfoService, MediaInfoService};
#[cfg(any(test, feature = "test-utils"))]
pub use notification::InMemoryNotificationService;
pub use notification::{
    AdminEvent, EventCategory, EventLevel, LocalNotificationService, NotificationService,
};
pub use watcher::{FsEvent, FsEventKind, FsWatcher, NotifyFsWatcher, PathDebouncer, WatchError};
#[cfg(any(test, feature = "test-utils"))]
pub use watcher::{InMemoryFsWatcher, MockFsWatcher};
