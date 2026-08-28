pub mod admin_log;
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
pub mod movie;
pub mod playback_progress;
pub mod show;
pub mod stream;

pub use admin_log::AdminLogRepository;
pub use enrichment::EnrichmentStateRepository;
pub use file::FileRepository;
pub use genre::GenreRepository;
pub use library::LibraryRepository;
pub use movie::MovieRepository;
pub use playback_progress::PlaybackProgressRepository;
pub use show::ShowRepository;
pub use stream::MediaStreamRepository;
