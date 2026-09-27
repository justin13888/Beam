pub mod admin_log;
pub mod applied_nfo;
// Shared behavioural contracts, instantiated over the in-memory doubles here and
// over the SeaORM implementations in `beam-index` under `pg-integration`.
// Not `cfg`-gated: the module contains only `macro_rules!` definitions, which
// are inert until invoked, and a `#[macro_export]` inside a `cfg`-gated module
// cannot be referred to by an absolute path from within this crate.
pub mod contract;
pub mod enrichment;
pub mod file;
pub mod genre;
pub mod library;
pub mod library_shape;
pub mod movie;
pub mod playback_progress;
pub mod playback_telemetry;
pub mod show;
pub mod sidecar_subtitle;
pub mod stream;

pub use admin_log::AdminLogRepository;
pub use applied_nfo::AppliedNfoRepository;
pub use enrichment::EnrichmentStateRepository;
pub use file::FileRepository;
pub use genre::GenreRepository;
pub use library::LibraryRepository;
pub use library_shape::LibraryShapeRepository;
pub use movie::MovieRepository;
pub use playback_progress::PlaybackProgressRepository;
pub use playback_telemetry::PlaybackTelemetryRepository;
pub use show::ShowRepository;
pub use sidecar_subtitle::SidecarSubtitleRepository;
pub use stream::MediaStreamRepository;
