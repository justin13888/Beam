pub mod admin_log;
pub mod artwork;
pub mod collections;
pub mod cursor;
pub mod enrichment_cursor;
pub mod hash;
pub mod health;
pub mod history_cursor;
pub mod library;
pub mod library_files_cursor;
pub mod lists;
pub mod media_info;
pub mod metadata;
pub mod notification;
pub mod offset_cursor;
pub mod people;
pub mod playback;
pub mod playback_telemetry;
pub mod sources;
pub mod subtitle;
pub mod telemetry;
pub mod user_list_cursor;

// Re-export IndexService from beam-index (LocalIndexService now runs
// in-process; there is no separate gRPC indexer service to wrap).
pub use beam_index::services::index::{IndexError, IndexService, LocalIndexService};

// Re-export types for convenience
pub use metadata::{
    MediaConnection, MediaSearchFilters, MediaSortField, PageInfo, SortOrder, TitleKind,
};
