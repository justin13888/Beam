//! Watched state, continue-watching and history (FR-507, FR-508, FR-510,
//! FR-511; issue #188).
//!
//! A report names a file; the state it writes is the file's *title*'s -- a
//! movie, or one episode -- so switching to another source of the same title
//! resumes where the viewer stopped. The file last played is kept beside the
//! position, and a client is sent back to it while it is still present.
//!
//! Continue-watching lists one row per title, most recently played first: a
//! movie to resume, or the episode of a show to watch next, which
//! [`beam_domain::utils::next_up`] decides. Every row carries what it
//! displays -- title, artwork, episode numbers -- so a client renders the
//! shelf without a request per row.
//!
//! The per-viewer state on detail and browse payloads is laid over them here,
//! after [`crate::services::metadata::MetadataService`] built them: the
//! metadata service stays the same for every viewer.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use beam_domain::models::watch_state::{
    HistoryPosition, RecordProgress, TitleRef, WatchState, WatchTarget,
};
use beam_domain::models::{Episode, MediaFileContent, Movie, Season, Show};
use beam_domain::repositories::{
    FileRepository, MovieRepository, ShowRepository, WatchStateRepository,
};
use beam_domain::utils::next_up::{EpisodeWatch, NextUp, next_up};
use beam_domain::utils::progress_validation::{ReportFault, validate_report};

use crate::models::{
    ArtworkKind, ArtworkVariant, EpisodeDetail, MediaMetadata, MediaTypeFilter, SeasonDetail,
    UserGroupState, UserTitleState, artwork_path,
};
use crate::services::sources::{PlayableFile, SourceCatalog};

pub use crate::models::playback::{ContinueWatchingItem, ContinueWatchingReason, HistoryItem};

/// Recording a report against one file.
#[derive(Debug, Error)]
pub enum PlaybackError {
    /// No such file, or a file that belongs to no title. The detail says
    /// which.
    #[error("{0}")]
    FileNotFound(String),
    /// The report breaks one or more rules; each fault names the member.
    #[error("the report breaks {} rule(s)", .0.len())]
    Invalid(Vec<ReportFault>),
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

/// Reading or changing the state of a title named by id.
#[derive(Debug, Error)]
pub enum TitleStateError {
    /// No movie, show, season or episode has the id.
    #[error("media {0} not found")]
    NotFound(Uuid),
    /// The id is a show's or a season's, and the operation is one only a
    /// movie or an episode has: a resume position.
    #[error("{0} is a show or a season; progress is kept per movie and per episode")]
    NotPlayable(Uuid),
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

/// Reading a viewer's own lists, or laying their state over a payload.
///
/// Its own type because none of these names a title that could be missing:
/// the only failure is the store's.
#[derive(Debug, Error)]
pub enum PlaybackReadError {
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

/// One page of history, with where it starts and ends for the cursors.
#[derive(Debug, Clone)]
pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    /// The first and last rows read. A row whose title can no longer be
    /// played is read but not listed, so these are the rows', not the
    /// items': the next page starts after everything this one read.
    pub start: Option<HistoryPosition>,
    pub end: Option<HistoryPosition>,
    pub has_next_page: bool,
    pub total: u64,
}

#[async_trait::async_trait]
pub trait PlaybackService: Send + Sync + std::fmt::Debug {
    /// Record that `user_id` is `position_secs` into `file_id`.
    async fn report_progress(
        &self,
        user_id: Uuid,
        file_id: Uuid,
        position_secs: f64,
        duration_secs: Option<f64>,
    ) -> Result<UserTitleState, PlaybackError>;

    /// The viewer's state for the movie or episode `media_id`; all zeros if
    /// never played.
    async fn get_title_progress(
        &self,
        user_id: Uuid,
        media_id: Uuid,
    ) -> Result<UserTitleState, TitleStateError>;

    /// Drop the resume position of the movie or episode `media_id`. Played
    /// stays played.
    async fn clear_progress(&self, user_id: Uuid, media_id: Uuid) -> Result<(), TitleStateError>;

    /// Mark the movie, episode, season or show `media_id` watched -- every
    /// episode of a season or show -- or, with `watched` false, forget it.
    async fn set_watched(
        &self,
        user_id: Uuid,
        media_id: Uuid,
        watched: bool,
    ) -> Result<(), TitleStateError>;

    /// Hide the title `media_id` belongs to from continue-watching until the
    /// viewer next plays it. An episode or season id hides its show.
    async fn dismiss(&self, user_id: Uuid, media_id: Uuid) -> Result<(), TitleStateError>;

    /// At most `first` rows of continue-watching, most recently played first.
    async fn get_continue_watching(
        &self,
        user_id: Uuid,
        first: NonZeroU32,
    ) -> Result<Vec<ContinueWatchingItem>, PlaybackReadError>;

    /// One page of watch history, newest first, after `after`.
    async fn get_history(
        &self,
        user_id: Uuid,
        after: Option<HistoryPosition>,
        first: NonZeroU32,
    ) -> Result<HistoryPage, PlaybackReadError>;

    /// Lay the viewer's state over one title's detail: the movie, or every
    /// episode, season and the show.
    async fn overlay_media(
        &self,
        user_id: Uuid,
        media: &mut MediaMetadata,
    ) -> Result<(), PlaybackReadError>;

    /// Lay the viewer's state over a browse page: each movie's. A show in a
    /// browse result carries no episodes, and no state.
    async fn overlay_titles(
        &self,
        user_id: Uuid,
        titles: &mut [MediaMetadata],
    ) -> Result<(), PlaybackReadError>;

    /// Lay the viewer's state over an episode's detail.
    async fn overlay_episode(
        &self,
        user_id: Uuid,
        detail: &mut EpisodeDetail,
    ) -> Result<(), PlaybackReadError>;

    /// Lay the viewer's state over a season's detail.
    async fn overlay_season(
        &self,
        user_id: Uuid,
        detail: &mut SeasonDetail,
    ) -> Result<(), PlaybackReadError>;
}

/// Every store [`DbPlaybackService`] reads.
#[derive(Debug)]
pub struct PlaybackRepositories {
    pub watch_state: Arc<dyn WatchStateRepository>,
    pub files: Arc<dyn FileRepository>,
    pub movies: Arc<dyn MovieRepository>,
    pub shows: Arc<dyn ShowRepository>,
    /// A title's present files, ranked.
    pub sources: Arc<SourceCatalog>,
}

#[derive(Debug)]
pub struct DbPlaybackService {
    watch_state: Arc<dyn WatchStateRepository>,
    files: Arc<dyn FileRepository>,
    movies: Arc<dyn MovieRepository>,
    shows: Arc<dyn ShowRepository>,
    sources: Arc<SourceCatalog>,
}

/// What an id names.
enum Resolved {
    Movie(Movie),
    Episode { episode: Episode, season: Season },
    Season(Season),
    Show(Show),
}

/// How many pages of candidates continue-watching reads, at most, to fill
/// itself past titles with nothing left to play. A viewer caught up on more
/// shows than this many pages hold gets a short shelf rather than an
/// unbounded read.
const MAX_CANDIDATE_PAGES: u64 = 5;

/// What a continue-watching or history row displays of its title.
///
/// The title is the one browse lists the title by: `title_localized` holds
/// the provider's original-language title, which the detail carries beside
/// it.
#[derive(Debug, Default)]
struct Display {
    title: String,
    poster_url: Option<String>,
    backdrop_url: Option<String>,
    season_number: Option<u32>,
    episode_number: Option<u32>,
    episode_title: Option<String>,
    thumbnail_url: Option<String>,
}

fn movie_display(movie: &Movie) -> Display {
    Display {
        title: movie.title.clone(),
        poster_url: movie
            .poster_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Movie, movie.id, ArtworkVariant::Poster)),
        backdrop_url: movie
            .backdrop_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Movie, movie.id, ArtworkVariant::Backdrop)),
        ..Display::default()
    }
}

fn episode_display(show: &Show, season: &Season, episode: &Episode) -> Display {
    let show_poster = show
        .poster_url
        .as_ref()
        .map(|_| artwork_path(ArtworkKind::Show, show.id, ArtworkVariant::Poster));
    let season_poster = season
        .poster_url
        .as_ref()
        .map(|_| artwork_path(ArtworkKind::Season, season.id, ArtworkVariant::Poster));
    Display {
        title: show.title.clone(),
        poster_url: show_poster.or(season_poster),
        backdrop_url: show
            .backdrop_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Show, show.id, ArtworkVariant::Backdrop)),
        season_number: Some(season.season_number),
        episode_number: Some(episode.episode_number),
        episode_title: Some(episode.title.clone()),
        thumbnail_url: episode
            .thumbnail_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Episode, episode.id, ArtworkVariant::Thumbnail)),
    }
}

/// A title's row, resolved: what it shows, what it plays, and which title.
struct Row {
    media_id: Uuid,
    media_type: MediaTypeFilter,
    episode_id: Option<Uuid>,
    file: PlayableFile,
    display: Display,
}

fn episode_key(state: &WatchState) -> Option<Uuid> {
    match state.target {
        WatchTarget::Episode { episode_id, .. } => Some(episode_id),
        WatchTarget::Movie { .. } => None,
    }
}

impl DbPlaybackService {
    pub fn new(repositories: PlaybackRepositories) -> Self {
        let PlaybackRepositories {
            watch_state,
            files,
            movies,
            shows,
            sources,
        } = repositories;
        Self {
            watch_state,
            files,
            movies,
            shows,
            sources,
        }
    }

    /// What `id` names: a movie, an episode, a season or a show.
    async fn resolve(&self, id: Uuid) -> Result<Option<Resolved>, sea_orm::DbErr> {
        if let Some(movie) = self.movies.find_by_id(id).await? {
            return Ok(Some(Resolved::Movie(movie)));
        }
        if let Some(episode) = self.shows.find_episode_by_id(id).await? {
            return Ok(self
                .shows
                .find_season_by_id(episode.season_id)
                .await?
                .map(|season| Resolved::Episode { episode, season }));
        }
        if let Some(season) = self.shows.find_season_by_id(id).await? {
            return Ok(Some(Resolved::Season(season)));
        }
        Ok(self.shows.find_by_id(id).await?.map(Resolved::Show))
    }

    /// The movie or episode `id` names, as the target a resume position is
    /// kept for.
    async fn playable_target(&self, id: Uuid) -> Result<WatchTarget, TitleStateError> {
        match self.resolve(id).await? {
            Some(Resolved::Movie(movie)) => Ok(WatchTarget::Movie { movie_id: movie.id }),
            Some(Resolved::Episode { episode, season }) => Ok(WatchTarget::Episode {
                episode_id: episode.id,
                show_id: season.show_id,
            }),
            Some(Resolved::Season(_) | Resolved::Show(_)) => Err(TitleStateError::NotPlayable(id)),
            None => Err(TitleStateError::NotFound(id)),
        }
    }

    /// Every episode of `show_id`, as targets.
    async fn show_targets(&self, show_id: Uuid) -> Result<Vec<WatchTarget>, sea_orm::DbErr> {
        Ok(self
            .shows
            .episode_outline(show_id)
            .await?
            .into_iter()
            .map(|e| WatchTarget::Episode {
                episode_id: e.episode_id,
                show_id,
            })
            .collect())
    }

    /// The other episodes a multi-episode file of `episode` holds: those of
    /// its season numbered after it, up to `last`.
    async fn span_targets(
        &self,
        episode: &Episode,
        show_id: Uuid,
        last: u32,
    ) -> Result<Vec<WatchTarget>, sea_orm::DbErr> {
        Ok(self
            .shows
            .find_episodes_by_season_id(episode.season_id)
            .await?
            .into_iter()
            .filter(|e| e.episode_number > episode.episode_number && e.episode_number <= last)
            .map(|e| WatchTarget::Episode {
                episode_id: e.id,
                show_id,
            })
            .collect())
    }

    /// The row a movie resolves to, with the file to play; `None` when the
    /// movie is gone or has no present file.
    async fn movie_row(
        &self,
        movie_id: Uuid,
        last_file_id: Option<Uuid>,
    ) -> Result<Option<Row>, sea_orm::DbErr> {
        let Some(movie) = self.movies.find_by_id(movie_id).await? else {
            return Ok(None);
        };
        let files = self.sources.movie_files(movie_id).await?;
        Ok(PlayableFile::pick(&files, last_file_id).map(|file| Row {
            media_id: movie_id,
            media_type: MediaTypeFilter::Movie,
            episode_id: None,
            file: file.clone(),
            display: movie_display(&movie),
        }))
    }

    /// The row an episode resolves to, likewise.
    async fn episode_row(
        &self,
        episode_id: Uuid,
        last_file_id: Option<Uuid>,
    ) -> Result<Option<Row>, sea_orm::DbErr> {
        let Some(episode) = self.shows.find_episode_by_id(episode_id).await? else {
            return Ok(None);
        };
        let Some(season) = self.shows.find_season_by_id(episode.season_id).await? else {
            return Ok(None);
        };
        let Some(show) = self.shows.find_by_id(season.show_id).await? else {
            return Ok(None);
        };
        let files = self.sources.episode_files(&episode).await?;
        Ok(PlayableFile::pick(&files, last_file_id).map(|file| Row {
            media_id: show.id,
            media_type: MediaTypeFilter::Show,
            episode_id: Some(episode.id),
            file: file.clone(),
            display: episode_display(&show, &season, &episode),
        }))
    }

    /// The continue-watching row of one candidate title, or `None` when it
    /// has nothing to offer: a movie with no present file, or a show whose
    /// viewer is caught up.
    async fn continue_item(
        &self,
        user_id: Uuid,
        title: TitleRef,
    ) -> Result<Option<ContinueWatchingItem>, sea_orm::DbErr> {
        match title {
            TitleRef::Movie(movie_id) => {
                let target = WatchTarget::Movie { movie_id };
                let Some(state) = self.watch_state.find(user_id, target).await? else {
                    return Ok(None);
                };
                if !state.is_resumable() {
                    return Ok(None);
                }
                let Some(row) = self.movie_row(movie_id, state.last_file_id).await? else {
                    return Ok(None);
                };
                Ok(Some(continue_item(
                    ContinueWatchingReason::Resume,
                    row,
                    state.position_secs,
                    state.duration_secs,
                    state.last_played_at,
                )))
            }
            TitleRef::Show(show_id) => {
                let states = self.watch_state.find_for_show(user_id, show_id).await?;
                let Some(latest) = states.iter().map(|s| s.last_played_at).max() else {
                    return Ok(None);
                };
                let outline = self.shows.episode_outline(show_id).await?;
                let watched: Vec<EpisodeWatch> = states
                    .iter()
                    .filter_map(|state| {
                        episode_key(state).map(|episode_id| EpisodeWatch {
                            episode_id,
                            position_secs: state.position_secs,
                            completed: state.completed,
                            last_played_at: state.last_played_at,
                        })
                    })
                    .collect();
                let (reason, episode_id, state) = match next_up(&outline, &watched) {
                    NextUp::Resume { episode_id } => (
                        ContinueWatchingReason::Resume,
                        episode_id,
                        states.iter().find(|s| episode_key(s) == Some(episode_id)),
                    ),
                    NextUp::Next { episode_id } => {
                        // The next episode may already be under way -- left
                        // part-way before the viewer went back to an earlier
                        // one. Its resume point and source still stand.
                        match states
                            .iter()
                            .find(|s| episode_key(s) == Some(episode_id) && s.is_resumable())
                        {
                            Some(state) => {
                                (ContinueWatchingReason::Resume, episode_id, Some(state))
                            }
                            None => (ContinueWatchingReason::NextUp, episode_id, None),
                        }
                    }
                    NextUp::Finished => return Ok(None),
                };
                let Some(row) = self
                    .episode_row(episode_id, state.and_then(|s| s.last_file_id))
                    .await?
                else {
                    return Ok(None);
                };
                let (position, duration) = match state {
                    Some(state) => (state.position_secs, state.duration_secs),
                    // A file holding a run of episodes lasts the run.
                    None => (
                        0.0,
                        row.file.duration_secs.filter(|_| !row.file.spans_episodes),
                    ),
                };
                Ok(Some(continue_item(reason, row, position, duration, latest)))
            }
        }
    }

    /// The history row of one state, or `None` when its title can no longer
    /// be played.
    async fn history_item(
        &self,
        state: &WatchState,
    ) -> Result<Option<HistoryItem>, sea_orm::DbErr> {
        let row = match state.target {
            WatchTarget::Movie { movie_id } => self.movie_row(movie_id, state.last_file_id).await?,
            WatchTarget::Episode { episode_id, .. } => {
                self.episode_row(episode_id, state.last_file_id).await?
            }
        };
        Ok(row.map(|row| {
            let Row {
                media_id,
                media_type,
                episode_id,
                file,
                display,
            } = row;
            let Display {
                title,
                poster_url,
                backdrop_url,
                season_number,
                episode_number,
                episode_title,
                thumbnail_url,
            } = display;
            HistoryItem {
                media_id,
                media_type,
                episode_id,
                file_id: file.file_id,
                position_secs: state.position_secs,
                duration_secs: state.duration_secs,
                played: state.completed,
                play_count: state.play_count,
                last_played_at: state.last_played_at,
                title,
                poster_url,
                backdrop_url,
                season_number,
                episode_number,
                episode_title,
                thumbnail_url,
            }
        }))
    }

    /// The viewer's state for each episode of `show_id`, by episode id.
    async fn episode_states(
        &self,
        user_id: Uuid,
        show_id: Uuid,
    ) -> Result<HashMap<Uuid, UserTitleState>, sea_orm::DbErr> {
        Ok(self
            .watch_state
            .find_for_show(user_id, show_id)
            .await?
            .into_iter()
            .filter_map(|state| episode_key(&state).map(|id| (id, UserTitleState::from(state))))
            .collect())
    }
}

fn continue_item(
    reason: ContinueWatchingReason,
    row: Row,
    position_secs: f64,
    duration_secs: Option<f64>,
    last_played_at: chrono::DateTime<chrono::Utc>,
) -> ContinueWatchingItem {
    let Row {
        media_id,
        media_type,
        episode_id,
        file,
        display,
    } = row;
    let Display {
        title,
        poster_url,
        backdrop_url,
        season_number,
        episode_number,
        episode_title,
        thumbnail_url,
    } = display;
    ContinueWatchingItem {
        reason,
        media_id,
        media_type,
        episode_id,
        file_id: file.file_id,
        position_secs,
        duration_secs,
        last_played_at,
        title,
        poster_url,
        backdrop_url,
        season_number,
        episode_number,
        episode_title,
        thumbnail_url,
    }
}

/// Set each episode's state from `states` (unplayed where absent), and each
/// season's from its episodes'. Returns every episode's state, for the show.
fn overlay_seasons(
    seasons: &mut [crate::models::SeasonMetadata],
    states: &HashMap<Uuid, UserTitleState>,
) -> Vec<UserTitleState> {
    let mut all = Vec::new();
    for season in seasons {
        for episode in &mut season.episodes {
            episode.user_state = states.get(&episode.id).cloned().unwrap_or_default();
        }
        season.user_state = Some(UserGroupState::of(
            season.episodes.iter().map(|e| &e.user_state),
        ));
        all.extend(season.episodes.iter().map(|e| e.user_state.clone()));
    }
    all
}

#[async_trait::async_trait]
impl PlaybackService for DbPlaybackService {
    async fn report_progress(
        &self,
        user_id: Uuid,
        file_id: Uuid,
        position_secs: f64,
        duration_secs: Option<f64>,
    ) -> Result<UserTitleState, PlaybackError> {
        let Some(file) = self.files.find_by_id(file_id).await? else {
            return Err(PlaybackError::FileNotFound(format!(
                "file {file_id} not found"
            )));
        };
        let report = validate_report(
            position_secs,
            duration_secs,
            file.duration.map(|d| d.as_secs_f64()),
        )
        .map_err(PlaybackError::Invalid)?;
        let belongs_to_no_title = || {
            PlaybackError::FileNotFound(format!("file {file_id} belongs to no movie or episode"))
        };

        let mut span = None;
        let target = match file.content {
            Some(MediaFileContent::Movie { movie_entry_id }) => {
                let entry = self
                    .movies
                    .find_entry_by_id(movie_entry_id)
                    .await?
                    .ok_or_else(belongs_to_no_title)?;
                WatchTarget::Movie {
                    movie_id: entry.movie_id,
                }
            }
            Some(MediaFileContent::Episode {
                episode_id,
                last_episode_number,
            }) => {
                let episode = self
                    .shows
                    .find_episode_by_id(episode_id)
                    .await?
                    .ok_or_else(belongs_to_no_title)?;
                let season = self
                    .shows
                    .find_season_by_id(episode.season_id)
                    .await?
                    .ok_or_else(belongs_to_no_title)?;
                if let Some(last) = last_episode_number.filter(|l| *l > episode.episode_number) {
                    span = Some((episode, last));
                }
                WatchTarget::Episode {
                    episode_id,
                    show_id: season.show_id,
                }
            }
            None => return Err(belongs_to_no_title()),
        };

        let record = RecordProgress {
            user_id,
            target,
            file_id,
            position_secs: report.position_secs,
            duration_secs: report.duration_secs,
        };
        let reaches_end = record.reaches_end();
        let state = self.watch_state.record_progress(record).await?;

        // A file holding a run of episodes watched to its end has played
        // every one of them.
        if reaches_end
            && let (Some((episode, last)), WatchTarget::Episode { show_id, .. }) = (span, target)
        {
            let rest = self.span_targets(&episode, show_id, last).await?;
            self.watch_state.mark_played(user_id, &rest).await?;
        }
        Ok(UserTitleState::from(state))
    }

    async fn get_title_progress(
        &self,
        user_id: Uuid,
        media_id: Uuid,
    ) -> Result<UserTitleState, TitleStateError> {
        let target = self.playable_target(media_id).await?;
        Ok(self
            .watch_state
            .find(user_id, target)
            .await?
            .map(UserTitleState::from)
            .unwrap_or_default())
    }

    async fn clear_progress(&self, user_id: Uuid, media_id: Uuid) -> Result<(), TitleStateError> {
        let target = self.playable_target(media_id).await?;
        self.watch_state.clear_progress(user_id, target).await?;
        Ok(())
    }

    async fn set_watched(
        &self,
        user_id: Uuid,
        media_id: Uuid,
        watched: bool,
    ) -> Result<(), TitleStateError> {
        let targets = match self.resolve(media_id).await? {
            Some(Resolved::Movie(movie)) => vec![WatchTarget::Movie { movie_id: movie.id }],
            Some(Resolved::Episode { episode, season }) => vec![WatchTarget::Episode {
                episode_id: episode.id,
                show_id: season.show_id,
            }],
            Some(Resolved::Season(season)) => self
                .shows
                .find_episodes_by_season_id(season.id)
                .await?
                .into_iter()
                .map(|e| WatchTarget::Episode {
                    episode_id: e.id,
                    show_id: season.show_id,
                })
                .collect(),
            Some(Resolved::Show(show)) => self.show_targets(show.id).await?,
            None => return Err(TitleStateError::NotFound(media_id)),
        };
        if watched {
            self.watch_state.mark_played(user_id, &targets).await?;
        } else {
            self.watch_state.mark_unplayed(user_id, &targets).await?;
        }
        Ok(())
    }

    async fn dismiss(&self, user_id: Uuid, media_id: Uuid) -> Result<(), TitleStateError> {
        let title = match self.resolve(media_id).await? {
            Some(Resolved::Movie(movie)) => TitleRef::Movie(movie.id),
            Some(Resolved::Episode { season, .. } | Resolved::Season(season)) => {
                TitleRef::Show(season.show_id)
            }
            Some(Resolved::Show(show)) => TitleRef::Show(show.id),
            None => return Err(TitleStateError::NotFound(media_id)),
        };
        self.watch_state.dismiss(user_id, title).await?;
        Ok(())
    }

    async fn get_continue_watching(
        &self,
        user_id: Uuid,
        first: NonZeroU32,
    ) -> Result<Vec<ContinueWatchingItem>, PlaybackReadError> {
        let wanted = first.get() as usize;
        // Twice the shelf per read: most candidates yield a row, and a
        // caught-up show that yields none should not cost a read of its own.
        let page = u64::from(first.get()).saturating_mul(2);
        let mut items = Vec::with_capacity(wanted);
        for read in 0..MAX_CANDIDATE_PAGES {
            let candidates = self
                .watch_state
                .find_continue_candidates(user_id, page, read * page)
                .await?;
            let exhausted = (candidates.len() as u64) < page;
            for title in candidates {
                if let Some(item) = self.continue_item(user_id, title).await? {
                    items.push(item);
                    if items.len() == wanted {
                        return Ok(items);
                    }
                }
            }
            if exhausted {
                break;
            }
        }
        Ok(items)
    }

    async fn get_history(
        &self,
        user_id: Uuid,
        after: Option<HistoryPosition>,
        first: NonZeroU32,
    ) -> Result<HistoryPage, PlaybackReadError> {
        let size = first.get() as usize;
        // One row past the page says whether another follows.
        let mut rows = self
            .watch_state
            .find_history_page(user_id, after, u64::from(first.get()) + 1)
            .await?;
        let has_next_page = rows.len() > size;
        rows.truncate(size);
        let total = self.watch_state.count_by_user(user_id).await?;

        let mut items = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(item) = self.history_item(row).await? {
                items.push(item);
            }
        }
        Ok(HistoryPage {
            items,
            start: rows.first().map(HistoryPosition::from),
            end: rows.last().map(HistoryPosition::from),
            has_next_page,
            total,
        })
    }

    async fn overlay_media(
        &self,
        user_id: Uuid,
        media: &mut MediaMetadata,
    ) -> Result<(), PlaybackReadError> {
        match media {
            MediaMetadata::Movie(movie) => {
                movie.user_state = self
                    .watch_state
                    .find(user_id, WatchTarget::Movie { movie_id: movie.id })
                    .await?
                    .map(UserTitleState::from)
                    .unwrap_or_default();
            }
            MediaMetadata::Show(show) => {
                let states = self.episode_states(user_id, show.id).await?;
                let all = overlay_seasons(&mut show.seasons, &states);
                show.user_state = Some(UserGroupState::of(&all));
            }
        }
        Ok(())
    }

    async fn overlay_titles(
        &self,
        user_id: Uuid,
        titles: &mut [MediaMetadata],
    ) -> Result<(), PlaybackReadError> {
        let movie_ids: Vec<Uuid> = titles
            .iter()
            .filter_map(|title| match title {
                MediaMetadata::Movie(movie) => Some(movie.id),
                MediaMetadata::Show(_) => None,
            })
            .collect();
        let mut states: HashMap<Uuid, UserTitleState> = self
            .watch_state
            .find_for_movies(user_id, &movie_ids)
            .await?
            .into_iter()
            .filter_map(|state| match state.target {
                WatchTarget::Movie { movie_id } => Some((movie_id, UserTitleState::from(state))),
                WatchTarget::Episode { .. } => None,
            })
            .collect();
        for title in titles {
            if let MediaMetadata::Movie(movie) = title {
                movie.user_state = states.remove(&movie.id).unwrap_or_default();
            }
        }
        Ok(())
    }

    async fn overlay_episode(
        &self,
        user_id: Uuid,
        detail: &mut EpisodeDetail,
    ) -> Result<(), PlaybackReadError> {
        detail.episode.user_state = self
            .watch_state
            .find(
                user_id,
                WatchTarget::Episode {
                    episode_id: detail.episode.id,
                    show_id: detail.show.id,
                },
            )
            .await?
            .map(UserTitleState::from)
            .unwrap_or_default();
        Ok(())
    }

    async fn overlay_season(
        &self,
        user_id: Uuid,
        detail: &mut SeasonDetail,
    ) -> Result<(), PlaybackReadError> {
        let states = self.episode_states(user_id, detail.show.id).await?;
        overlay_seasons(std::slice::from_mut(&mut detail.season), &states);
        Ok(())
    }
}
