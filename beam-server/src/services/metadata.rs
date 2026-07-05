use salvo::oapi::ToSchema;
use serde::Serialize;
use std::sync::Arc;
use thiserror::Error;
use tracing::warn;
use uuid::Uuid;

use crate::models::{
    AudioSourceInfo, EpisodeMetadata, ExternalIdentifiers, MediaMetadata, MediaSource,
    MovieMetadata, Ratings, SeasonMetadata, ShowDates, ShowMetadata, Title, VideoSourceInfo,
};
use beam_domain::models::enrichment::EnrichmentTargetId;
use beam_domain::repositories::{
    EnrichmentStateRepository, FileRepository, MediaStreamRepository, MovieRepository,
    ShowRepository,
};

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

    /// List the playable/downloadable source files for a media item. Only
    /// movies are supported for now -- shows don't have files at the show
    /// level (episodes do), and there's no per-episode lookup by id yet.
    async fn get_media_sources(&self, media_id: &str) -> Result<Vec<MediaSource>, MetadataError>;
}

/// Database-backed metadata service
#[derive(Debug)]
pub struct DbMetadataService {
    movie_repo: Arc<dyn MovieRepository>,
    show_repo: Arc<dyn ShowRepository>,
    file_repo: Arc<dyn FileRepository>,
    stream_repo: Arc<dyn MediaStreamRepository>,
    enrichment_repo: Option<Arc<dyn EnrichmentStateRepository>>,
}

impl DbMetadataService {
    pub fn new(
        movie_repo: Arc<dyn MovieRepository>,
        show_repo: Arc<dyn ShowRepository>,
        file_repo: Arc<dyn FileRepository>,
        stream_repo: Arc<dyn MediaStreamRepository>,
    ) -> Self {
        Self {
            movie_repo,
            show_repo,
            file_repo,
            stream_repo,
            enrichment_repo: None,
        }
    }

    /// Wire up `refresh_metadata` to actually flip enrichment-queue rows back
    /// to `Pending` rather than being a no-op. Defaults to `None`, under
    /// which `refresh_metadata` is a no-op (matches this service's prior
    /// behavior when no enrichment pipeline is configured).
    pub fn with_enrichment_repo(mut self, repo: Arc<dyn EnrichmentStateRepository>) -> Self {
        self.enrichment_repo = Some(repo);
        self
    }

    /// Build MediaMetadata for a movie by its DB model
    async fn build_movie_metadata(
        &self,
        movie: beam_domain::models::Movie,
    ) -> Result<MediaMetadata, MetadataError> {
        // Get all movie entries, then files for each, then streams
        let entries = self
            .movie_repo
            .find_entries_by_movie_id(movie.id)
            .await
            .map_err(|e| MetadataError::InternalError(e.to_string()))?;

        let mut streams = Vec::new();
        let mut duration: Option<f64> = None;
        let mut file_id: Option<String> = None;

        for entry in &entries {
            let files = self
                .file_repo
                .find_by_movie_entry_id(entry.id)
                .await
                .map_err(|e| MetadataError::InternalError(e.to_string()))?;

            for file in &files {
                // Capture the first file's id as the movie's streamable handle.
                if file_id.is_none() {
                    file_id = Some(file.id.to_string());
                }
                // Use the first file's duration as the movie duration
                if duration.is_none() {
                    duration = file.duration.map(|d| d.as_secs_f64());
                }

                let file_streams = self
                    .stream_repo
                    .find_by_file_id(file.id)
                    .await
                    .map_err(|e| MetadataError::InternalError(e.to_string()))?;

                if !file_streams.is_empty() {
                    // Build a StreamConfiguration from the file's streams and convert
                    let stream_meta =
                        build_media_stream_metadata_from_domain_streams(&file_streams);
                    streams.push(stream_meta);
                }
            }
        }

        let identifiers =
            if movie.imdb_id.is_some() || movie.tmdb_id.is_some() || movie.tvdb_id.is_some() {
                Some(ExternalIdentifiers {
                    imdb_id: movie.imdb_id.clone(),
                    tmdb_id: movie.tmdb_id,
                    tvdb_id: movie.tvdb_id,
                })
            } else {
                None
            };

        let ratings = movie.rating_tmdb.map(|r| Ratings {
            tmdb: Some((r * 10.0) as u32),
        });

        Ok(MediaMetadata::Movie(MovieMetadata {
            id: movie.id.to_string(),
            title: Title {
                original: movie.title.clone(),
                localized: movie.title_localized.clone(),
                alternatives: None,
            },
            description: movie.description.clone(),
            year: movie.year,
            release_date: movie.release_date.map(|d| {
                chrono::DateTime::from_naive_utc_and_offset(
                    d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                    chrono::Utc,
                )
            }),
            runtime: movie.runtime.map(|d| (d.as_secs() / 60) as u32),
            duration,
            poster_url: movie.poster_url.clone(),
            backdrop_url: movie.backdrop_url.clone(),
            genres: vec![],
            ratings,
            identifiers,
            streams,
            file_id,
        }))
    }

    /// Build MediaMetadata for a show by its DB model
    async fn build_show_metadata(
        &self,
        show: beam_domain::models::Show,
    ) -> Result<MediaMetadata, MetadataError> {
        let seasons_domain = self
            .show_repo
            .find_seasons_by_show_id(show.id)
            .await
            .map_err(|e| MetadataError::InternalError(e.to_string()))?;

        let mut seasons = Vec::new();
        for season in seasons_domain {
            let episodes_domain = self
                .show_repo
                .find_episodes_by_season_id(season.id)
                .await
                .map_err(|e| MetadataError::InternalError(e.to_string()))?;

            let mut episodes = Vec::new();
            for ep in episodes_domain {
                let files = self
                    .file_repo
                    .find_by_episode_id(ep.id)
                    .await
                    .map_err(|e| MetadataError::InternalError(e.to_string()))?;

                let duration = files
                    .first()
                    .and_then(|f| f.duration.map(|d| d.as_secs_f64()));
                let file_id = files.first().map(|f| f.id.to_string());

                let mut ep_streams = Vec::new();
                for file in &files {
                    let file_streams = self
                        .stream_repo
                        .find_by_file_id(file.id)
                        .await
                        .map_err(|e| MetadataError::InternalError(e.to_string()))?;
                    if !file_streams.is_empty() {
                        ep_streams.push(build_media_stream_metadata_from_domain_streams(
                            &file_streams,
                        ));
                    }
                }

                episodes.push(EpisodeMetadata {
                    id: ep.id.to_string(),
                    episode_number: ep.episode_number,
                    title: ep.title,
                    description: ep.description,
                    air_date: ep.air_date,
                    thumbnail_url: ep.thumbnail_url,
                    duration,
                    streams: ep_streams,
                    file_id,
                });
            }

            let dates = ShowDates {
                first_aired: season.first_aired.map(|d| {
                    chrono::DateTime::from_naive_utc_and_offset(
                        d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                        chrono::Utc,
                    )
                }),
                last_aired: season.last_aired.map(|d| {
                    chrono::DateTime::from_naive_utc_and_offset(
                        d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                        chrono::Utc,
                    )
                }),
            };

            seasons.push(SeasonMetadata {
                season_number: season.season_number,
                dates,
                episode_runtime: None,
                episodes,
                poster_url: season.poster_url,
                genres: vec![],
                ratings: None,
                identifiers: None,
            });
        }

        Ok(MediaMetadata::Show(ShowMetadata {
            id: show.id.to_string(),
            title: Title {
                original: show.title.clone(),
                localized: show.title_localized.clone(),
                alternatives: None,
            },
            description: show.description.clone(),
            year: show.year,
            seasons,
        }))
    }
}

/// Build a `MediaStreamMetadata` from domain `MediaStream` records
fn build_media_stream_metadata_from_domain_streams(
    streams: &[beam_domain::models::MediaStream],
) -> crate::models::MediaStreamMetadata {
    use crate::models::{AudioTrack, MediaStreamMetadata, SubtitleTrack, VideoTrack};
    use crate::models::{OutputAudioCodec, OutputSubtitleCodec, OutputVideoCodec, Resolution};
    use beam_domain::models::stream::StreamMetadata;
    use rust_decimal::Decimal;

    let mut video_tracks = Vec::new();
    let mut audio_tracks = Vec::new();
    let mut subtitle_tracks = Vec::new();

    for stream in streams {
        match &stream.metadata {
            StreamMetadata::Video(v) => {
                video_tracks.push(VideoTrack {
                    codec: OutputVideoCodec::H264, // Simplified: map from codec string
                    max_rate: v.bit_rate.unwrap_or(0),
                    bit_rate: v.bit_rate.unwrap_or(0),
                    resolution: Resolution {
                        width: v.width,
                        height: v.height,
                    },
                    frame_rate: v
                        .frame_rate
                        .map(|f| Decimal::from_f64_retain(f).unwrap_or(Decimal::new(2997, 2)))
                        .unwrap_or(Decimal::new(2997, 2)),
                });
            }
            StreamMetadata::Audio(a) => {
                audio_tracks.push(AudioTrack {
                    codec: OutputAudioCodec::Aac, // Simplified: map from codec string
                    language: a.language.clone(),
                    title: a.title.clone().unwrap_or_else(|| "Unknown".to_string()),
                    channel_layout: a.channel_layout.clone(),
                    is_default: a.is_default,
                    is_autoselect: a.is_default,
                });
            }
            StreamMetadata::Subtitle(s) => {
                subtitle_tracks.push(SubtitleTrack {
                    codec: OutputSubtitleCodec::WebVTT, // Simplified
                    language: s.language.clone(),
                    title: s.title.clone(),
                    is_default: s.is_default,
                    is_autoselect: s.is_default,
                    is_forced: s.is_forced,
                });
            }
        }
    }

    MediaStreamMetadata {
        video_tracks,
        audio_tracks,
        subtitle_tracks,
    }
}

/// All media items (movies + shows) for search results
#[derive(Debug, Clone)]
enum MediaItem {
    Movie {
        id: Uuid,
        title: String,
        year: Option<u32>,
        metadata: MediaMetadata,
    },
    Show {
        id: Uuid,
        title: String,
        year: Option<u32>,
        metadata: MediaMetadata,
    },
}

impl MediaItem {
    fn id(&self) -> Uuid {
        match self {
            MediaItem::Movie { id, .. } => *id,
            MediaItem::Show { id, .. } => *id,
        }
    }

    fn title(&self) -> &str {
        match self {
            MediaItem::Movie { title, .. } => title,
            MediaItem::Show { title, .. } => title,
        }
    }

    fn year(&self) -> Option<u32> {
        match self {
            MediaItem::Movie { year, .. } => *year,
            MediaItem::Show { year, .. } => *year,
        }
    }

    fn into_metadata(self) -> MediaMetadata {
        match self {
            MediaItem::Movie { metadata, .. } => metadata,
            MediaItem::Show { metadata, .. } => metadata,
        }
    }
}

#[async_trait::async_trait]
impl MetadataService for DbMetadataService {
    async fn get_media_metadata(&self, media_id: &str) -> Option<MediaMetadata> {
        let id = Uuid::parse_str(media_id).ok()?;

        // Try movie first
        match self.movie_repo.find_by_id(id).await {
            Ok(Some(movie)) => match self.build_movie_metadata(movie).await {
                Ok(metadata) => return Some(metadata),
                Err(e) => warn!("Failed to build movie metadata for {}: {}", id, e),
            },
            Ok(None) => {}
            Err(e) => warn!("DB error looking up movie {}: {}", id, e),
        }

        // Try show
        match self.show_repo.find_by_id(id).await {
            Ok(Some(show)) => match self.build_show_metadata(show).await {
                Ok(metadata) => return Some(metadata),
                Err(e) => warn!("Failed to build show metadata for {}: {}", id, e),
            },
            Ok(None) => {}
            Err(e) => warn!("DB error looking up show {}: {}", id, e),
        }

        None
    }

    #[allow(clippy::too_many_arguments)]
    async fn search_media(
        &self,
        first: Option<u32>,
        after: Option<String>,
        last: Option<u32>,
        _before: Option<String>,
        sort_by: MediaSortField,
        sort_order: SortOrder,
        filters: MediaSearchFilters,
    ) -> MediaConnection {
        let limit = first.or(last).unwrap_or(20) as usize;

        // Fetch candidates based on media_type filter
        let mut items: Vec<MediaItem> = Vec::new();

        let include_movies = filters.media_type != Some(MediaTypeFilter::Show);
        let include_shows = filters.media_type != Some(MediaTypeFilter::Movie);

        if include_movies {
            let movie_query = beam_domain::models::MovieSearchQuery {
                query: filters.query.clone(),
                year: filters.year,
                year_from: filters.year_from,
                year_to: filters.year_to,
                min_rating: filters.min_rating,
            };
            match self.movie_repo.search(&movie_query).await {
                Ok(movies) => {
                    for movie in movies {
                        let year = movie.year;
                        let title = movie.title.clone();
                        let id = movie.id;
                        // Build lightweight metadata (no deep stream loading for search)
                        let metadata = MediaMetadata::Movie(MovieMetadata {
                            id: id.to_string(),
                            title: Title {
                                original: movie.title.clone(),
                                localized: movie.title_localized.clone(),
                                alternatives: None,
                            },
                            description: movie.description.clone(),
                            year: movie.year,
                            release_date: None,
                            runtime: movie.runtime.map(|d| (d.as_secs() / 60) as u32),
                            duration: None,
                            poster_url: movie.poster_url.clone(),
                            backdrop_url: movie.backdrop_url.clone(),
                            genres: vec![],
                            ratings: movie.rating_tmdb.map(|r| Ratings {
                                tmdb: Some((r * 10.0) as u32),
                            }),
                            identifiers: if movie.imdb_id.is_some()
                                || movie.tmdb_id.is_some()
                                || movie.tvdb_id.is_some()
                            {
                                Some(ExternalIdentifiers {
                                    imdb_id: movie.imdb_id,
                                    tmdb_id: movie.tmdb_id,
                                    tvdb_id: movie.tvdb_id,
                                })
                            } else {
                                None
                            },
                            streams: vec![],
                            file_id: None,
                        });
                        items.push(MediaItem::Movie {
                            id,
                            title,
                            year,
                            metadata,
                        });
                    }
                }
                Err(e) => warn!("Failed to fetch movies for search: {}", e),
            }
        }

        if include_shows {
            let show_query = beam_domain::models::ShowSearchQuery {
                query: filters.query.clone(),
                year: filters.year,
                year_from: filters.year_from,
                year_to: filters.year_to,
            };
            match self.show_repo.search(&show_query).await {
                Ok(shows) => {
                    for show in shows {
                        let year = show.year;
                        let title = show.title.clone();
                        let id = show.id;
                        let metadata = MediaMetadata::Show(ShowMetadata {
                            id: id.to_string(),
                            title: Title {
                                original: show.title.clone(),
                                localized: show.title_localized.clone(),
                                alternatives: None,
                            },
                            description: show.description.clone(),
                            year: show.year,
                            seasons: vec![],
                        });
                        items.push(MediaItem::Show {
                            id,
                            title,
                            year,
                            metadata,
                        });
                    }
                }
                Err(e) => warn!("Failed to fetch shows for search: {}", e),
            }
        }

        // Sort
        match sort_by {
            MediaSortField::Title => {
                items.sort_by(|a, b| a.title().cmp(b.title()));
            }
            MediaSortField::Year => {
                items.sort_by_key(|item| item.year().unwrap_or(0));
            }
            MediaSortField::Rating => {
                // Ratings not stored on MediaItem for shows; title fallback
                items.sort_by(|a, b| a.title().cmp(b.title()));
            }
            MediaSortField::DateAdded | MediaSortField::Runtime => {
                items.sort_by(|a, b| a.title().cmp(b.title()));
            }
        }

        if sort_order == SortOrder::Desc {
            items.reverse();
        }

        // Cursor-based pagination (cursor = uuid string of the last seen item)
        let start_idx = if let Some(after_cursor) = after {
            items
                .iter()
                .position(|item| item.id().to_string() == after_cursor)
                .map(|pos| pos + 1)
                .unwrap_or(0)
        } else {
            0
        };

        let paged: Vec<MediaItem> = items.into_iter().skip(start_idx).take(limit + 1).collect();

        let has_next_page = paged.len() > limit;
        let has_previous_page = start_idx > 0;

        let paged: Vec<MediaItem> = paged.into_iter().take(limit).collect();

        let edges: Vec<MediaEdge> = paged
            .into_iter()
            .map(|item| {
                let cursor = item.id().to_string();
                MediaEdge {
                    cursor,
                    node: item.into_metadata(),
                }
            })
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
    async fn refresh_metadata(&self, filter: MediaFilter) -> Result<(), MetadataError> {
        let Some(enrichment_repo) = &self.enrichment_repo else {
            return Ok(());
        };

        match filter {
            MediaFilter::All => {
                enrichment_repo
                    .request_refresh_all(false)
                    .await
                    .map_err(|e| MetadataError::InternalError(e.to_string()))?;
                Ok(())
            }
            MediaFilter::ByMediaId(media_id) => {
                let id = Uuid::parse_str(&media_id)
                    .map_err(|_| MetadataError::InternalError("invalid media id".to_string()))?;

                let target = if matches!(self.movie_repo.find_by_id(id).await, Ok(Some(_))) {
                    EnrichmentTargetId::Movie(id)
                } else if matches!(self.show_repo.find_by_id(id).await, Ok(Some(_))) {
                    EnrichmentTargetId::Show(id)
                } else {
                    return Err(MetadataError::MediaNotFound);
                };

                let found = enrichment_repo
                    .request_refresh(target, false)
                    .await
                    .map_err(|e| MetadataError::InternalError(e.to_string()))?;
                if !found {
                    warn!("No enrichment row exists for media {media_id}; nothing to refresh");
                }
                Ok(())
            }
            MediaFilter::ByLibraryId(_) => {
                // Library-scoped bulk refresh needs a "titles in this
                // library" query this crate doesn't expose yet; the REST
                // admin API (E3) adds a proper library-scoped endpoint.
                // GraphQL's ByLibraryId variant is left a no-op until then,
                // rather than approximating it as a global refresh.
                Ok(())
            }
        }
    }

    async fn get_media_sources(&self, media_id: &str) -> Result<Vec<MediaSource>, MetadataError> {
        let id = Uuid::parse_str(media_id)
            .map_err(|_| MetadataError::InternalError("invalid media id".to_string()))?;

        if matches!(self.movie_repo.find_by_id(id).await, Ok(Some(_))) {
            let entries = self
                .movie_repo
                .find_entries_by_movie_id(id)
                .await
                .map_err(|e| MetadataError::InternalError(e.to_string()))?;

            let mut sources = Vec::new();
            for entry in entries {
                let files = self
                    .file_repo
                    .find_by_movie_entry_id(entry.id)
                    .await
                    .map_err(|e| MetadataError::InternalError(e.to_string()))?;
                for file in files {
                    sources.push(self.build_source(file).await?);
                }
            }
            return Ok(sources);
        }

        if matches!(self.show_repo.find_by_id(id).await, Ok(Some(_))) {
            return Err(MetadataError::Unsupported(
                "sources are not available at the show level; use an episode id".to_string(),
            ));
        }

        Err(MetadataError::MediaNotFound)
    }
}

impl DbMetadataService {
    async fn build_source(
        &self,
        file: beam_domain::models::MediaFile,
    ) -> Result<MediaSource, MetadataError> {
        let streams = self
            .stream_repo
            .find_by_file_id(file.id)
            .await
            .map_err(|e| MetadataError::InternalError(e.to_string()))?;

        let mut video = None;
        let mut audio_tracks = Vec::new();
        for stream in &streams {
            match &stream.metadata {
                beam_domain::models::StreamMetadata::Video(v) => {
                    video = Some(VideoSourceInfo {
                        codec: stream.codec.clone(),
                        width: v.width,
                        height: v.height,
                        bit_rate: v.bit_rate,
                        hdr_format: v.hdr_format.clone(),
                    });
                }
                beam_domain::models::StreamMetadata::Audio(a) => {
                    audio_tracks.push(AudioSourceInfo {
                        codec: stream.codec.clone(),
                        language: a.language.clone(),
                        channels: a.channels,
                        is_default: a.is_default,
                    });
                }
                beam_domain::models::StreamMetadata::Subtitle(_) => {}
            }
        }

        Ok(MediaSource {
            file_id: file.id.to_string(),
            size_bytes: file.size_bytes,
            mime_type: file.mime_type,
            container_format: file.container_format,
            duration_secs: file.duration.map(|d| d.as_secs_f64()),
            video,
            audio_tracks,
            stream_url: format!("/v1/files/{}/stream", file.id),
            download_url: format!("/v1/files/{}/download", file.id),
        })
    }
}

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("Media not found")]
    MediaNotFound,
    #[error("Internal metadata service error: {0}")]
    InternalError(String),
    /// The request was well-formed and the target exists, but this operation
    /// doesn't apply to it (e.g. requesting sources for a show id).
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

#[derive(Debug, Clone)]
pub enum MediaFilter {
    All,
    ByMediaId(String),
    ByLibraryId(String),
}

/// Sort field options for media search
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// Ascending order
    #[default]
    Asc,
    /// Descending order
    Desc,
}

/// Media type filter
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
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
#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct MediaConnection {
    /// List of edges containing media items and cursors
    pub edges: Vec<MediaEdge>,
    /// Pagination information
    pub page_info: PageInfo,
}

/// Relay-style edge for media
#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
pub struct MediaEdge {
    /// Cursor for this edge
    pub cursor: String,
    /// The media item
    pub node: MediaMetadata,
}

/// Relay-style page info
#[derive(Clone, Debug, Serialize, serde::Deserialize, ToSchema)]
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
