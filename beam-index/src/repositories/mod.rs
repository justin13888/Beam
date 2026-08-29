pub mod admin_log;
pub mod enrichment;
pub mod file;
pub mod genre;
pub mod library;
pub mod movie;
pub mod playback_progress;
pub mod show;
pub mod stream;

// SQL implementations
pub use admin_log::SqlAdminLogRepository;
pub use enrichment::SqlEnrichmentStateRepository;
pub use file::SqlFileRepository;
pub use genre::SqlGenreRepository;
pub use library::SqlLibraryRepository;
pub use movie::SqlMovieRepository;
pub use playback_progress::SqlPlaybackProgressRepository;
pub use show::SqlShowRepository;
pub use stream::SqlMediaStreamRepository;

#[cfg(test)]
#[path = "sql_shape_tests.rs"]
mod sql_shape_tests;
