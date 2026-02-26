pub mod admin_log;
pub mod grpc_index;
pub mod hash;
pub mod library;
pub mod media_info;
pub mod metadata;
pub mod notification;
pub mod transcode;

pub use grpc_index::GrpcIndexService;
// Re-export IndexService from beam-index
pub use beam_index::services::index::{IndexError, IndexService};

// Re-export types for convenience
pub use metadata::{
    MediaConnection, MediaEdge, MediaSearchFilters, MediaSortField, MediaTypeFilter, PageInfo,
    SortOrder,
};
