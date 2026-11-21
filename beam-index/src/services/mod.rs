pub mod enrichment;
pub mod filesystem_probe;
pub mod hash;
pub mod index;
pub mod media_info;
pub mod mime;
pub mod notification;
pub mod scan;
pub mod trickplay;
pub mod watch_status;
pub mod watcher;

pub use enrichment::{EnrichmentPolicy, MetadataEnrichmentService};
pub use hash::{HashConfig, HashService, LocalHashService};
#[cfg(any(test, feature = "test-utils"))]
pub use index::MockIndexService;
pub use index::{IndexError, IndexService, LocalIndexService, ReconcileOutcome};
pub use media_info::{LocalMediaInfoService, MediaInfoService};
#[cfg(any(test, feature = "test-utils"))]
pub use notification::InMemoryNotificationService;
pub use notification::{
    AdminEvent, EventCategory, EventLevel, LocalNotificationService, NewAdminEvent,
    NotificationService,
};
pub use scan::{
    Retirement, ScanJob, ScanPhase, ScanProgress, ScanState, ScanTicket, ScanTrigger, StoppedScan,
};
pub use trickplay::{LocalTrickplayGenerator, TrickplayGenerator};
#[cfg(any(test, feature = "test-utils"))]
pub use watcher::InMemoryFsWatcher;
pub use watcher::{FsEvent, FsEventKind, FsWatcher, NotifyFsWatcher, PathDebouncer, WatchError};
