//! The opt-in anonymous library report (issue #93, ADR-0019).
//!
//! Off unless `BEAM_TELEMETRY_URL` names a collector. When on, a weekly
//! OTLP/HTTP JSON request of aggregate gauges -- counts of titles and files,
//! container and codec distributions, size buckets, the server version -- and
//! nothing that names a title, path, user or server. An admin can see the
//! exact bytes before opting in, at `GET /v1/admin/telemetry/library`.

pub mod library_report;
pub mod otlp;
pub mod scheduler;

pub use library_report::build_library_report;
pub use otlp::{OTLP_JSON_CONTENT_TYPE, encode_otlp_json};
pub use scheduler::{
    LibraryReportConfig, LibraryReportPreview, LibraryReportService, TelemetryError,
};
