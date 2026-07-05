//! Playback progress ("resume point") and continue-watching (FR-507, FR-508).
//!
//! Reporting progress is a hot path during playback (the client beacons it at
//! a bounded interval per FR-702), so it stays a single upsert with no
//! media/movie/show resolution. Resolving a file id to its movie/show is only
//! needed to render the continue-watching list, so that work happens once
//! per row at read time instead of on every progress report.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use salvo::oapi::ToSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use beam_domain::models::MediaFileContent;
use beam_domain::repositories::{
    FileRepository, MovieRepository, PlaybackProgressRepository, ShowRepository,
};

#[derive(Debug, Error)]
pub enum PlaybackError {
    #[error("file not found")]
    FileNotFound,
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct PlaybackProgressDto {
    pub file_id: String,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub completed: bool,
    pub updated_at: DateTime<Utc>,
}

impl From<beam_domain::models::PlaybackProgress> for PlaybackProgressDto {
    fn from(p: beam_domain::models::PlaybackProgress) -> Self {
        Self {
            file_id: p.file_id.to_string(),
            position_secs: p.position_secs,
            duration_secs: p.duration_secs,
            completed: p.completed,
            updated_at: p.updated_at,
        }
    }
}

/// One row in the continue-watching list. `media_id`/`media_type` let the
/// client link to the detail page; `episode_id` is set only for episodes, so
/// the client can additionally deep-link to the right episode within a show.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ContinueWatchingItem {
    pub file_id: String,
    pub media_id: String,
    pub media_type: String,
    pub episode_id: Option<String>,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub updated_at: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait PlaybackService: Send + Sync + std::fmt::Debug {
    async fn report_progress(
        &self,
        user_id: Uuid,
        file_id: Uuid,
        position_secs: f64,
        duration_secs: Option<f64>,
    ) -> Result<PlaybackProgressDto, PlaybackError>;

    async fn get_continue_watching(
        &self,
        user_id: Uuid,
        limit: u32,
    ) -> Result<Vec<ContinueWatchingItem>, PlaybackError>;
}

#[derive(Debug)]
pub struct DbPlaybackService {
    playback_repo: Arc<dyn PlaybackProgressRepository>,
    file_repo: Arc<dyn FileRepository>,
    movie_repo: Arc<dyn MovieRepository>,
    show_repo: Arc<dyn ShowRepository>,
}

impl DbPlaybackService {
    pub fn new(
        playback_repo: Arc<dyn PlaybackProgressRepository>,
        file_repo: Arc<dyn FileRepository>,
        movie_repo: Arc<dyn MovieRepository>,
        show_repo: Arc<dyn ShowRepository>,
    ) -> Self {
        Self {
            playback_repo,
            file_repo,
            movie_repo,
            show_repo,
        }
    }

    /// Resolve a file id to `(media_id, media_type, episode_id)`.
    async fn resolve_media_ref(
        &self,
        file_id: Uuid,
    ) -> Result<(String, String, Option<String>), PlaybackError> {
        let file = self
            .file_repo
            .find_by_id(file_id)
            .await?
            .ok_or(PlaybackError::FileNotFound)?;

        match file.content {
            Some(MediaFileContent::Movie { movie_entry_id }) => {
                let entry = self
                    .movie_repo
                    .find_entry_by_id(movie_entry_id)
                    .await?
                    .ok_or(PlaybackError::FileNotFound)?;
                Ok((entry.movie_id.to_string(), "movie".to_string(), None))
            }
            Some(MediaFileContent::Episode { episode_id }) => {
                let episode = self
                    .show_repo
                    .find_episode_by_id(episode_id)
                    .await?
                    .ok_or(PlaybackError::FileNotFound)?;
                let season = self
                    .show_repo
                    .find_season_by_id(episode.season_id)
                    .await?
                    .ok_or(PlaybackError::FileNotFound)?;
                Ok((
                    season.show_id.to_string(),
                    "show".to_string(),
                    Some(episode_id.to_string()),
                ))
            }
            None => Err(PlaybackError::FileNotFound),
        }
    }
}

#[async_trait::async_trait]
impl PlaybackService for DbPlaybackService {
    async fn report_progress(
        &self,
        user_id: Uuid,
        file_id: Uuid,
        position_secs: f64,
        duration_secs: Option<f64>,
    ) -> Result<PlaybackProgressDto, PlaybackError> {
        if self.file_repo.find_by_id(file_id).await?.is_none() {
            return Err(PlaybackError::FileNotFound);
        }

        let progress = self
            .playback_repo
            .upsert(beam_domain::models::UpsertPlaybackProgress {
                user_id,
                file_id,
                position_secs,
                duration_secs,
            })
            .await?;

        Ok(PlaybackProgressDto::from(progress))
    }

    async fn get_continue_watching(
        &self,
        user_id: Uuid,
        limit: u32,
    ) -> Result<Vec<ContinueWatchingItem>, PlaybackError> {
        let rows = self
            .playback_repo
            .find_in_progress_by_user(user_id, limit)
            .await?;

        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            // The underlying file/movie/show may have been removed by a
            // rescan since progress was recorded; skip that row rather than
            // fail the whole list for one stale entry.
            if let Ok((media_id, media_type, episode_id)) =
                self.resolve_media_ref(row.file_id).await
            {
                items.push(ContinueWatchingItem {
                    file_id: row.file_id.to_string(),
                    media_id,
                    media_type,
                    episode_id,
                    position_secs: row.position_secs,
                    duration_secs: row.duration_secs,
                    updated_at: row.updated_at,
                });
            }
        }
        Ok(items)
    }
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod playback_tests;
