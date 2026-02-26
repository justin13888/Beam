use std::collections::VecDeque;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use tokio::sync::broadcast;
use uuid::Uuid;

const BROADCAST_CAPACITY: usize = 256;
const DEFAULT_LOG_SIZE: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, async_graphql::Enum)]
pub enum EventLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, async_graphql::Enum)]
pub enum EventCategory {
    LibraryScan,
    System,
}

#[derive(Clone, Debug, async_graphql::SimpleObject)]
pub struct AdminEvent {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub level: EventLevel,
    pub category: EventCategory,
    pub message: String,
    pub library_id: Option<String>,
    pub library_name: Option<String>,
}

impl AdminEvent {
    pub fn info(
        category: EventCategory,
        message: impl Into<String>,
        library_id: Option<String>,
        library_name: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            level: EventLevel::Info,
            category,
            message: message.into(),
            library_id,
            library_name,
        }
    }

    pub fn warning(
        category: EventCategory,
        message: impl Into<String>,
        library_id: Option<String>,
        library_name: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            level: EventLevel::Warning,
            category,
            message: message.into(),
            library_id,
            library_name,
        }
    }

    pub fn error(
        category: EventCategory,
        message: impl Into<String>,
        library_id: Option<String>,
        library_name: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            level: EventLevel::Error,
            category,
            message: message.into(),
            library_id,
            library_name,
        }
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
        let mut log = self.event_log.write();
        if log.len() >= self.max_log_size {
            log.pop_front();
        }
        log.push_back(event.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publish_and_recent_events() {
        let svc = LocalNotificationService::new();
        svc.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            "Scan started",
            Some("lib-1".to_string()),
            Some("Movies".to_string()),
        ));
        svc.publish(AdminEvent::warning(
            EventCategory::LibraryScan,
            "File skipped",
            Some("lib-1".to_string()),
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

    #[test]
    fn test_in_memory_notification_service() {
        let svc = InMemoryNotificationService::new();
        svc.publish(AdminEvent::info(
            EventCategory::LibraryScan,
            "Test event",
            Some("lib-1".to_string()),
            None,
        ));
        let events = svc.published_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].library_id, Some("lib-1".to_string()));
    }
}
