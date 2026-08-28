pub use beam_index::services::admin_log::{AdminLogError, AdminLogService, LocalAdminLogService};
// Test-only: gated so it does not reach a release build, and so cargo-mutants does
// not treat the double as product code.
#[cfg(any(test, feature = "test-utils"))]
pub use beam_index::services::admin_log::NoOpAdminLogService;
