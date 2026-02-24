use async_graphql::{Enum, SimpleObject};
use thiserror::Error;

use crate::models::{MediaMetadata, MovieMetadata, Title};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct MetadataConfig {
    pub cache_dir: PathBuf,
}

#[async_trait::async_trait]
pub trait MetadataService: Send + Sync + std::fmt::Debug {
    /// Get media metadata by media ID
    async fn get_media_metadata(&self, media_id: &str) -> Option<MediaMetadata>;

    /// Search/explore media with cursor-based pagination, sorting, and filtering
    #[allow(clippy::too_many_arguments)]
    async fn search_media(
        &self,
        first: Option<u32>,
        after: Option<String>,
        last: Option<u32>,
        before: Option<String>,
        sort_by: MediaSortField,
        sort_order: SortOrder,
        filters: MediaSearchFilters,
    ) -> MediaConnection;

    /// Refresh metadata for by media filter
    async fn refresh_metadata(&self, filter: MediaFilter) -> Result<(), MetadataError>;
}

#[derive(Debug)]
pub struct StubMetadataService {
    /// Directory where cached metadata is stored
    _cache_dir: std::path::PathBuf,
    /// Map of stub media IDs -> titles (demo purposes)
    /// In a real implementation, this would query a database or external API
    _media_stubs: Vec<(String, String)>,
}

impl StubMetadataService {
    pub fn new(config: MetadataConfig) -> Self {
        Self {
            _cache_dir: config.cache_dir,
            _media_stubs: vec![],
        }
    }
}

#[async_trait::async_trait]
impl MetadataService for StubMetadataService {
    /// Get media metadata by media ID
    async fn get_media_metadata(&self, _media_id: &str) -> Option<MediaMetadata> {
        // TODO: Convert output type to Result
        // let media_metadata = MediaMetadata::Show(ShowMetadata {
        //     title: Title {
        //         original: String::from("Unknown Title"),
        //         localized: None,
        //         alternatives: None,
        //     },
        //     description: None,
        //     year: None,
        //     seasons: vec![SeasonMetadata {
        //         season_number: 1,
        //         dates: ShowDates {
        //             first_aired: None,
        //             last_aired: None,
        //         },
        //         episode_runtime: None,
        //         episodes: vec![],
        //         poster_url: None,
        //         genres: vec![],
        //         ratings: None,
        //         identifiers: None,
        //     }],
        // });

        let media_metadata = MediaMetadata::Movie(MovieMetadata {
            title: Title {
                original: String::from("Unknown Movie"),
                localized: None,
                alternatives: None,
            },
            description: None,
            year: None,
            runtime: None,
            poster_url: None,
            genres: vec![],
            ratings: None,
            identifiers: None,
            release_date: None,
            duration: None,
            backdrop_url: None,
            streams: vec![],
        });

        Some(media_metadata) // TODO: Replace with actual metadata retrieval logic
    }

    /// Search/explore media with cursor-based pagination, sorting, and filtering
    #[allow(clippy::too_many_arguments)]
    async fn search_media(
        &self,
        first: Option<u32>,
        after: Option<String>,
        last: Option<u32>,
        _before: Option<String>,
        _sort_by: MediaSortField,
        _sort_order: SortOrder,
        _filters: MediaSearchFilters,
    ) -> MediaConnection {
        // TODO: Implement actual database query with filtering, sorting, and pagination
        // This is a stub implementation that returns mock data

        // Mock data for demonstration - in real implementation, this would come from DB
        let all_mock_items = vec![
            (
                "movie1",
                MediaMetadata::Movie(MovieMetadata {
                    title: Title {
                        original: String::from("Sample Movie 1"),
                        localized: None,
                        alternatives: None,
                    },
                    description: Some(String::from("A sample movie for testing")),
                    year: Some(2024),
                    runtime: Some(120),
                    poster_url: None,
                    genres: vec![String::from("Action"), String::from("Drama")],
                    ratings: None,
                    identifiers: None,
                    release_date: None,
                    duration: Some(7200.0),
                    backdrop_url: None,
                    streams: vec![],
                }),
            ),
            (
                "movie2",
                MediaMetadata::Movie(MovieMetadata {
                    title: Title {
                        original: String::from("Sample Movie 2"),
                        localized: None,
                        alternatives: None,
                    },
                    description: Some(String::from("Another sample movie")),
                    year: Some(2023),
                    runtime: Some(95),
                    poster_url: None,
                    genres: vec![String::from("Comedy")],
                    ratings: None,
                    identifiers: None,
                    release_date: None,
                    duration: Some(5700.0),
                    backdrop_url: None,
                    streams: vec![],
                }),
            ),
            (
                "movie3",
                MediaMetadata::Movie(MovieMetadata {
                    title: Title {
                        original: String::from("Sample Movie 3"),
                        localized: None,
                        alternatives: None,
                    },
                    description: Some(String::from("Yet another sample movie")),
                    year: Some(2022),
                    runtime: Some(110),
                    poster_url: None,
                    genres: vec![String::from("Thriller")],
                    ratings: None,
                    identifiers: None,
                    release_date: None,
                    duration: Some(6600.0),
                    backdrop_url: None,
                    streams: vec![],
                }),
            ),
        ];

        // Determine slice based on cursor pagination
        let limit = first.or(last).unwrap_or(20) as usize;
        let start_idx = if let Some(after_cursor) = after {
            // Find position after the cursor
            all_mock_items
                .iter()
                .position(|(id, _)| *id == after_cursor)
                .map(|pos| pos + 1)
                .unwrap_or(0)
        } else {
            0
        };

        let items: Vec<(String, MediaMetadata)> = all_mock_items
            .into_iter()
            .skip(start_idx)
            .take(limit + 1) // Take one extra to check if there's more
            .map(|(id, media)| (id.to_string(), media))
            .collect();

        let has_next_page = items.len() > limit;
        let has_previous_page = start_idx > 0;

        // Trim to actual limit
        let items: Vec<(String, MediaMetadata)> = items.into_iter().take(limit).collect();

        let edges: Vec<MediaEdge> = items
            .into_iter()
            .map(|(cursor, node)| MediaEdge { cursor, node })
            .collect();

        let page_info = PageInfo {
            has_next_page,
            has_previous_page,
            start_cursor: edges.first().map(|e| e.cursor.clone()),
            end_cursor: edges.last().map(|e| e.cursor.clone()),
        };

        MediaConnection { edges, page_info }
    }

    /// Refresh metadata for by media filter
    async fn refresh_metadata(&self, _filter: MediaFilter) -> Result<(), MetadataError> {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("Media not found")]
    MediaNotFound,
    #[error("Internal metadata service error: {0}")]
    InternalError(String),
}

#[derive(Debug, Clone)]
pub enum MediaFilter {
    All,
    ByMediaId(String),
    ByLibraryId(String),
}

/// Sort field options for media search
#[derive(Clone, Copy, Debug, PartialEq, Eq, Enum, Default)]
pub enum MediaSortField {
    /// Sort by title (alphabetical)
    #[default]
    Title,
    /// Sort by release year
    Year,
    /// Sort by rating
    Rating,
    /// Sort by date added to library
    DateAdded,
    /// Sort by runtime/duration
    Runtime,
}

/// Sort order
#[derive(Clone, Copy, Debug, PartialEq, Eq, Enum, Default)]
pub enum SortOrder {
    /// Ascending order
    #[default]
    Asc,
    /// Descending order
    Desc,
}

/// Media type filter
#[derive(Clone, Copy, Debug, PartialEq, Eq, Enum)]
pub enum MediaTypeFilter {
    /// Movies only
    Movie,
    /// TV Shows only
    Show,
}

/// Search filters for media
#[derive(Clone, Debug)]
pub struct MediaSearchFilters {
    pub media_type: Option<MediaTypeFilter>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub year_from: Option<u32>,
    pub year_to: Option<u32>,
    pub query: Option<String>,
    pub min_rating: Option<u32>,
}

/// Relay-style connection for media search results
#[derive(Clone, Debug, SimpleObject)]
pub struct MediaConnection {
    /// List of edges containing media items and cursors
    pub edges: Vec<MediaEdge>,
    /// Pagination information
    pub page_info: PageInfo,
}

/// Relay-style edge for media
#[derive(Clone, Debug, SimpleObject)]
pub struct MediaEdge {
    /// Cursor for this edge
    pub cursor: String,
    /// The media item
    pub node: MediaMetadata,
}

/// Relay-style page info
#[derive(Clone, Debug, SimpleObject)]
pub struct PageInfo {
    /// Whether there is a next page
    pub has_next_page: bool,
    /// Whether there is a previous page
    pub has_previous_page: bool,
    /// Cursor of the first edge
    pub start_cursor: Option<String>,
    /// Cursor of the last edge
    pub end_cursor: Option<String>,
}
#[cfg(test)]
#[path = "metadata_tests.rs"]
mod metadata_tests;
