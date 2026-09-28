use std::path::Path;

pub mod admin_log;
pub mod applied_nfo;
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

// SQL implementations
pub use admin_log::SqlAdminLogRepository;
pub use applied_nfo::SqlAppliedNfoRepository;
pub use enrichment::SqlEnrichmentStateRepository;
pub use file::SqlFileRepository;
pub use genre::SqlGenreRepository;
pub use library::SqlLibraryRepository;
pub use library_shape::SqlLibraryShapeRepository;
pub use movie::SqlMovieRepository;
pub use playback_progress::SqlPlaybackProgressRepository;
pub use playback_telemetry::SqlPlaybackTelemetryRepository;
pub use show::SqlShowRepository;
pub use sidecar_subtitle::SqlSidecarSubtitleRepository;
pub use stream::SqlMediaStreamRepository;

/// The `LIKE` pattern, escaped with `\`, that matches a stored path strictly
/// beneath the directory `dir`: its text, wildcards escaped, then a
/// separator and anything. The separator keeps `/a/S1` from matching
/// `/a/S10/x.mkv`, and escaping keeps a `_` or `%` in a directory's name
/// from matching any character.
pub(crate) fn beneath_pattern(dir: &Path) -> String {
    let dir = dir.to_string_lossy();
    let dir = dir.trim_end_matches(std::path::MAIN_SEPARATOR);
    let mut pattern = String::with_capacity(dir.len() + 2);
    for c in dir.chars() {
        if matches!(c, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push(std::path::MAIN_SEPARATOR);
    pattern.push('%');
    pattern
}

#[cfg(test)]
#[path = "sql_shape_tests.rs"]
mod sql_shape_tests;
