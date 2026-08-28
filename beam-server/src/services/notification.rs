pub use beam_index::services::notification::{
    AdminEvent, EventCategory, EventLevel, LocalNotificationService, NotificationService,
};
// Test-only: gated so it does not reach a release build, and so cargo-mutants does
// not treat the double as product code.
#[cfg(any(test, feature = "test-utils"))]
pub use beam_index::services::notification::InMemoryNotificationService;
