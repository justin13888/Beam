use std::collections::VecDeque;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use tokio::sync::broadcast;
use uuid::Uuid;

use beam_domain::models::enrichment::{EnrichmentStatus, EnrichmentTargetId};

use crate::services::scan::ScanEvent;

const BROADCAST_CAPACITY: usize = 256;
const DEFAULT_LOG_SIZE: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventCategory {
    LibraryScan,
    /// A scan job's structured progress (FR-208): started, per-item
    /// progress, completed, failed. Every such event carries
    /// [`AdminEvent::scan`]. Broadcast live and never kept in the recent
    /// event log, which a scan of a large library would otherwise flush.
    ScanProgress,
    /// What one enrichment attempt made of a title (FR-309): enriched, left
    /// unmatched, to be retried, or failed. Every such event carries
    /// [`AdminEvent::enrichment`]. Broadcast live and, like scan progress,
    /// never kept in the recent event log, which a sweep of a large library
    /// would otherwise flush; the title's standing is in the enrichment list.
    Enrichment,
    System,
}

impl EventCategory {
    /// Whether events of this category are live state rather than history:
    /// broadcast to subscribers and never kept in the recent event log.
    #[must_use]
    pub const fn is_live_only(self) -> bool {
        match self {
            EventCategory::ScanProgress | EventCategory::Enrichment => true,
            EventCategory::LibraryScan | EventCategory::System => false,
        }
    }
}

/// The title an [`EventCategory::Enrichment`] event reports on, and where
/// its enrichment now stands.
#[derive(Clone, Debug, PartialEq)]
pub struct EnrichmentEvent {
    pub target: EnrichmentTargetId,
    /// The title's display title, when the title still exists.
    pub title: Option<String>,
    pub status: EnrichmentStatus,
    /// The `"provider:id"` the title is matched to, if any.
    pub matched_ref: Option<String>,
    /// Why it was not enriched; `None` once it was.
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AdminEvent {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub level: EventLevel,
    pub category: EventCategory,
    pub message: String,
    pub library_id: Option<Uuid>,
    pub library_name: Option<String>,
    /// The scan job an [`EventCategory::ScanProgress`] event reports on.
    pub scan: Option<ScanEvent>,
    /// The title an [`EventCategory::Enrichment`] event reports on.
    pub enrichment: Option<EnrichmentEvent>,
}

impl AdminEvent {
    pub fn new(
        level: EventLevel,
        category: EventCategory,
        message: impl Into<String>,
        library_id: Option<Uuid>,
        library_name: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            level,
            category,
            message: message.into(),
            library_id,
            library_name,
            scan: None,
            enrichment: None,
        }
    }

    pub fn info(
        category: EventCategory,
        message: impl Into<String>,
        library_id: Option<Uuid>,
        library_name: Option<String>,
    ) -> Self {
        Self::new(
            EventLevel::Info,
            category,
            message,
            library_id,
            library_name,
        )
    }

    pub fn warning(
        category: EventCategory,
        message: impl Into<String>,
        library_id: Option<Uuid>,
        library_name: Option<String>,
    ) -> Self {
        Self::new(
            EventLevel::Warning,
            category,
            message,
            library_id,
            library_name,
        )
    }

    pub fn error(
        category: EventCategory,
        message: impl Into<String>,
        library_id: Option<Uuid>,
        library_name: Option<String>,
    ) -> Self {
        Self::new(
            EventLevel::Error,
            category,
            message,
            library_id,
            library_name,
        )
    }

    /// Attach the scan job this event reports on.
    pub fn with_scan(mut self, scan: ScanEvent) -> Self {
        self.scan = Some(scan);
        self
    }

    /// Attach the title this event reports on.
    pub fn with_enrichment(mut self, enrichment: EnrichmentEvent) -> Self {
        self.enrichment = Some(enrichment);
        self
    }
}

/// A position in a process's [`LibraryChangeLog`] (issue #454): every
/// `library_changed` event after it is news to whoever holds it.
///
/// `epoch` names the process that issued it. A cursor from another process
/// -- the server restarted, and its sequence started again from zero --
/// cannot be compared with this one's, so it reads as a reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LibraryChangeCursor {
    pub epoch: Uuid,
    pub sequence: u64,
}

/// What changed since a [`LibraryChangeCursor`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryChanges {
    /// Where the next read continues from.
    pub cursor: LibraryChangeCursor,
    /// What the holder of the old cursor is to refetch.
    pub changed: ChangedLibraries,
}

/// The libraries a [`LibraryChanges`] says changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangedLibraries {
    /// These libraries, each once, in no particular order. Empty when
    /// nothing changed.
    Only(Vec<Uuid>),
    /// Anything may have changed: there was no cursor, or it was issued by
    /// another process, or it is ahead of this one's sequence.
    All,
}

/// The coalesced `library_changed { library_id }` events of one process
/// (issue #454): for each library, the sequence number of the latest change
/// to it. A reader learns which libraries changed since its cursor, not how
/// often or how -- no file path, so nothing leaks past a user's access.
///
/// Bounded by the number of libraries ever changed in this process, not by
/// the number of changes.
#[derive(Debug)]
pub struct LibraryChangeLog {
    epoch: Uuid,
    state: RwLock<LibraryChangeState>,
}

#[derive(Debug, Default)]
struct LibraryChangeState {
    sequence: u64,
    latest: std::collections::HashMap<Uuid, u64>,
}

impl LibraryChangeLog {
    /// An empty log for a process named `epoch`.
    #[must_use]
    pub fn new(epoch: Uuid) -> Self {
        Self {
            epoch,
            state: RwLock::new(LibraryChangeState::default()),
        }
    }

    /// Record a change to `library_id`.
    pub fn record(&self, library_id: Uuid) {
        let mut state = self.state.write();
        state.sequence += 1;
        let sequence = state.sequence;
        state.latest.insert(library_id, sequence);
    }

    /// The libraries changed after `since`.
    #[must_use]
    pub fn since(&self, since: Option<LibraryChangeCursor>) -> LibraryChanges {
        let state = self.state.read();
        let cursor = LibraryChangeCursor {
            epoch: self.epoch,
            sequence: state.sequence,
        };
        let changed = match since {
            Some(since) if since.epoch == self.epoch && since.sequence <= state.sequence => {
                ChangedLibraries::Only(
                    state
                        .latest
                        .iter()
                        .filter(|(_, sequence)| **sequence > since.sequence)
                        .map(|(library_id, _)| *library_id)
                        .collect(),
                )
            }
            Some(_) | None => ChangedLibraries::All,
        };
        LibraryChanges { cursor, changed }
    }
}

pub trait NotificationService: Send + Sync + std::fmt::Debug {
    fn publish(&self, event: AdminEvent);
    fn subscribe(&self) -> broadcast::Receiver<AdminEvent>;
    fn recent_events(&self, limit: usize) -> Vec<AdminEvent>;
    /// Publish `library_changed { library_id }` (issue #454): what the
    /// library holds changed, so a client showing it is to refetch. Unlike
    /// an [`AdminEvent`] it is for every user who can see the library.
    fn library_changed(&self, library_id: Uuid);
    /// The libraries changed since `since`.
    fn library_changes(&self, since: Option<LibraryChangeCursor>) -> LibraryChanges;
}

#[derive(Debug, Clone)]
pub struct LocalNotificationService {
    sender: broadcast::Sender<AdminEvent>,
    event_log: Arc<RwLock<VecDeque<AdminEvent>>>,
    max_log_size: usize,
    library_changes: Arc<LibraryChangeLog>,
}

impl LocalNotificationService {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            sender,
            event_log: Arc::new(RwLock::new(VecDeque::new())),
            max_log_size: DEFAULT_LOG_SIZE,
            library_changes: Arc::new(LibraryChangeLog::new(Uuid::new_v4())),
        }
    }
}

impl Default for LocalNotificationService {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationService for LocalNotificationService {
    fn publish(&self, event: AdminEvent) {
        // Progress is live state, not history: a subscriber sees it as it
        // happens, and the log keeps what an administrator reads later.
        if !event.category.is_live_only() {
            let mut log = self.event_log.write();
            if log.len() >= self.max_log_size {
                log.pop_front();
            }
            log.push_back(event.clone());
        }
        let _ = self.sender.send(event);
    }

    fn subscribe(&self) -> broadcast::Receiver<AdminEvent> {
        self.sender.subscribe()
    }

    fn recent_events(&self, limit: usize) -> Vec<AdminEvent> {
        let log = self.event_log.read();
        let events: Vec<_> = log.iter().rev().take(limit).cloned().collect();
        events.into_iter().rev().collect()
    }

    fn library_changed(&self, library_id: Uuid) {
        self.library_changes.record(library_id);
    }

    fn library_changes(&self, since: Option<LibraryChangeCursor>) -> LibraryChanges {
        self.library_changes.since(since)
    }
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build.
///
/// Collected into one module rather than left as loose `#[cfg(...)]` items so a
/// single `#[mutants::skip]` covers the lot: cargo-mutants recognises only the
/// literal `#[cfg(test)]` and would otherwise mutate these bodies and report the
/// scaffolding as untested product behaviour. `mise run check:mutants-skip-fakes`
/// enforces the attribute.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;

    /// In-memory notification service for use in tests and as a stub.
    /// Exposes `published_events()` to inspect what was emitted.
    #[derive(Debug, Clone)]
    pub struct InMemoryNotificationService {
        sender: broadcast::Sender<AdminEvent>,
        event_log: Arc<RwLock<VecDeque<AdminEvent>>>,
        library_changes: Arc<LibraryChangeLog>,
    }

    impl InMemoryNotificationService {
        pub fn new() -> Self {
            let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
            Self {
                sender,
                event_log: Arc::new(RwLock::new(VecDeque::new())),
                library_changes: Arc::new(LibraryChangeLog::new(Uuid::new_v4())),
            }
        }

        pub fn published_events(&self) -> Vec<AdminEvent> {
            self.event_log.read().iter().cloned().collect()
        }
    }

    impl Default for InMemoryNotificationService {
        fn default() -> Self {
            Self::new()
        }
    }

    impl NotificationService for InMemoryNotificationService {
        fn publish(&self, event: AdminEvent) {
            self.event_log.write().push_back(event.clone());
            let _ = self.sender.send(event);
        }

        fn subscribe(&self) -> broadcast::Receiver<AdminEvent> {
            self.sender.subscribe()
        }

        fn recent_events(&self, limit: usize) -> Vec<AdminEvent> {
            let log = self.event_log.read();
            let events: Vec<_> = log.iter().rev().take(limit).cloned().collect();
            events.into_iter().rev().collect()
        }

        fn library_changed(&self, library_id: Uuid) {
            self.library_changes.record(library_id);
        }

        fn library_changes(&self, since: Option<LibraryChangeCursor>) -> LibraryChanges {
            self.library_changes.since(since)
        }
    }
}

// Re-exported at the module root so the double keeps the path it had before it
// moved into `in_memory`.
#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::InMemoryNotificationService;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publish_and_recent_events() {
        let svc = LocalNotificationService::new();
        svc.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            "Scan started",
            Some(Uuid::from_u128(1)),
            Some("Movies".to_string()),
        ));
        svc.publish(AdminEvent::warning(
            EventCategory::LibraryScan,
            "File skipped",
            Some(Uuid::from_u128(1)),
            Some("Movies".to_string()),
        ));
        svc.publish(AdminEvent::error(
            EventCategory::System,
            "Disk full",
            None,
            None,
        ));

        let events = svc.recent_events(10);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].level, EventLevel::Info);
        assert_eq!(events[1].level, EventLevel::Warning);
        assert_eq!(events[2].level, EventLevel::Error);
    }

    /// Scan progress reaches a live subscriber but not the recent-event log:
    /// one scan of a large library would otherwise push every other event out
    /// of it.
    #[tokio::test]
    async fn scan_progress_is_broadcast_but_not_kept_in_the_log() {
        use crate::services::scan::{ScanEvent, ScanPhase, ScanProgress};

        let svc = LocalNotificationService::new();
        let mut live = svc.subscribe();
        svc.publish(
            AdminEvent::info(EventCategory::ScanProgress, "Scanning", None, None).with_scan(
                ScanEvent {
                    job_id: Uuid::nil(),
                    phase: ScanPhase::Progress,
                    progress: ScanProgress::default(),
                },
            ),
        );
        svc.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            "Scan complete",
            None,
            None,
        ));

        let first = live.recv().await.expect("the progress event is broadcast");
        assert_eq!(first.category, EventCategory::ScanProgress);
        assert_eq!(first.scan.map(|scan| scan.phase), Some(ScanPhase::Progress));
        let kept: Vec<EventCategory> = svc
            .recent_events(10)
            .into_iter()
            .map(|event| event.category)
            .collect();
        assert_eq!(kept, vec![EventCategory::LibraryScan]);
    }

    /// Enrichment outcomes are live too (FR-309): a sweep of a large library
    /// announces every title, and the log keeps what an administrator reads
    /// later.
    #[tokio::test]
    async fn enrichment_outcomes_are_broadcast_but_not_kept_in_the_log() {
        let svc = LocalNotificationService::new();
        let mut live = svc.subscribe();
        let title = EnrichmentEvent {
            target: EnrichmentTargetId::Movie(Uuid::nil()),
            title: Some("Heat".to_string()),
            status: EnrichmentStatus::Enriched,
            matched_ref: Some("tmdb:949".to_string()),
            error: None,
        };
        svc.publish(
            AdminEvent::info(EventCategory::Enrichment, "Enriched \"Heat\"", None, None)
                .with_enrichment(title.clone()),
        );
        svc.publish(AdminEvent::info(
            EventCategory::System,
            "Started",
            None,
            None,
        ));

        let first = live.recv().await.expect("the outcome is broadcast");
        assert_eq!(first.enrichment, Some(title));
        let kept: Vec<EventCategory> = svc
            .recent_events(10)
            .into_iter()
            .map(|event| event.category)
            .collect();
        assert_eq!(kept, vec![EventCategory::System]);
    }

    fn only(changes: &LibraryChanges) -> Vec<Uuid> {
        match &changes.changed {
            ChangedLibraries::Only(ids) => {
                let mut ids = ids.clone();
                ids.sort();
                ids
            }
            ChangedLibraries::All => panic!("expected the changed libraries, got a reset"),
        }
    }

    /// A reader learns each library changed after its cursor once, however
    /// many times it changed, and nothing it already read.
    #[test]
    fn library_changes_since_a_cursor_are_each_library_changed_after_it_once() {
        let svc = LocalNotificationService::new();
        let (films, shows) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let start = svc.library_changes(None);
        assert_eq!(start.changed, ChangedLibraries::All, "no cursor is a reset");

        svc.library_changed(films);
        svc.library_changed(shows);
        svc.library_changed(films);
        let first = svc.library_changes(Some(start.cursor));
        assert_eq!(only(&first), vec![films, shows]);
        assert_eq!(first.cursor.epoch, start.cursor.epoch);

        let nothing = svc.library_changes(Some(first.cursor));
        assert_eq!(only(&nothing), Vec::<Uuid>::new());
        assert_eq!(nothing.cursor, first.cursor, "no change, no new position");

        svc.library_changed(shows);
        assert_eq!(only(&svc.library_changes(Some(first.cursor))), vec![shows]);
        // An older cursor still sees everything after it.
        assert_eq!(
            only(&svc.library_changes(Some(start.cursor))),
            vec![films, shows]
        );
    }

    /// A cursor this process did not issue -- another process's, or one past
    /// its sequence -- cannot say what the reader missed, so it is a reset.
    #[test]
    fn a_cursor_from_another_process_or_ahead_of_this_one_is_a_reset() {
        let svc = LocalNotificationService::new();
        svc.library_changed(Uuid::from_u128(1));
        let now = svc.library_changes(None).cursor;

        let foreign = LibraryChangeCursor {
            epoch: Uuid::from_u128(99),
            sequence: now.sequence,
        };
        assert_eq!(
            svc.library_changes(Some(foreign)).changed,
            ChangedLibraries::All
        );
        let ahead = LibraryChangeCursor {
            sequence: now.sequence + 1,
            ..now
        };
        assert_eq!(
            svc.library_changes(Some(ahead)).changed,
            ChangedLibraries::All
        );
        assert_eq!(
            svc.library_changes(Some(now)).changed,
            ChangedLibraries::Only(Vec::new()),
            "the cursor at the head is not ahead of it"
        );
    }

    /// Two processes never issue the same epoch, so a restart is a reset.
    #[test]
    fn each_service_names_its_own_epoch() {
        let first = LocalNotificationService::new().library_changes(None).cursor;
        let second = LocalNotificationService::new().library_changes(None).cursor;
        assert_ne!(first.epoch, second.epoch);
    }

    /// The log is bounded: past its size the oldest event is evicted, and a
    /// limited read is the newest `limit` events, oldest of them first.
    #[test]
    fn the_log_evicts_its_oldest_event_and_a_limited_read_is_the_newest() {
        let svc = LocalNotificationService::new();
        let published = DEFAULT_LOG_SIZE + 3;
        for n in 0..published {
            svc.publish(AdminEvent::info(
                EventCategory::LibraryScan,
                n.to_string(),
                None,
                None,
            ));
        }
        let messages = |events: Vec<AdminEvent>| -> Vec<String> {
            events.into_iter().map(|event| event.message).collect()
        };

        let kept = messages(svc.recent_events(usize::MAX));
        let expected: Vec<String> = (3..published).map(|n| n.to_string()).collect();
        assert_eq!(kept, expected, "the three oldest are evicted, in order");
        let newest: Vec<String> = (published - 2..published).map(|n| n.to_string()).collect();
        assert_eq!(messages(svc.recent_events(2)), newest);
        assert!(svc.recent_events(0).is_empty());
    }
}
