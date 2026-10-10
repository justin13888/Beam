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

pub trait NotificationService: Send + Sync + std::fmt::Debug {
    fn publish(&self, event: AdminEvent);
    fn subscribe(&self) -> broadcast::Receiver<AdminEvent>;
    fn recent_events(&self, limit: usize) -> Vec<AdminEvent>;
}

#[derive(Debug, Clone)]
pub struct LocalNotificationService {
    sender: broadcast::Sender<AdminEvent>,
    event_log: Arc<RwLock<VecDeque<AdminEvent>>>,
    max_log_size: usize,
}

impl LocalNotificationService {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            sender,
            event_log: Arc::new(RwLock::new(VecDeque::new())),
            max_log_size: DEFAULT_LOG_SIZE,
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
    }

    impl InMemoryNotificationService {
        pub fn new() -> Self {
            let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
            Self {
                sender,
                event_log: Arc::new(RwLock::new(VecDeque::new())),
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

    #[test]
    fn test_in_memory_notification_service() {
        let svc = InMemoryNotificationService::new();
        svc.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            "Test event",
            Some(Uuid::from_u128(1)),
            None,
        ));
        let events = svc.published_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].library_id, Some(Uuid::from_u128(1)));
    }
}
