//! An administrator's control of enrichment (issue #185): where each title
//! stands (FR-303), the provider's candidates for one, fixing its match to a
//! chosen id, locking the fields enrichment must leave alone, and queueing a
//! title, a library or everything for another pass (FR-308).
//!
//! Nothing here talks to a provider on the write path. A fix-match pins the
//! title to the chosen id -- an administrator's pin, which no NFO replaces
//! (FR-312) -- and queues it; the sweep fetches it by that pin, as it fetches
//! any pinned title. So every mutation answers at once, and the outcome
//! arrives on the admin event stream (FR-309).

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use tokio::sync::Notify;
use uuid::Uuid;

use beam_domain::models::enrichment::{
    EnrichmentListFilter, EnrichmentListPosition, EnrichmentListQuery, EnrichmentState,
    EnrichmentStatus, EnrichmentTargetId, FieldLocks, MetadataField,
};
use beam_domain::models::{AdminLogCategory, AdminLogLevel, PinSource, ProviderPin};
use beam_domain::providers::enrichment::{EnrichmentError, EnrichmentProvider, MediaQuery};
use beam_domain::repositories::{
    EnrichmentStateRepository, LibraryRepository, MovieRepository, ShowRepository,
};

use super::matcher;
use crate::services::admin_log::AdminLogService;
use crate::services::index::{IndexError, LocalIndexService};

/// How many candidates a search offers an administrator.
pub const MAX_CANDIDATES: usize = 10;

/// Pins a title by its NFO again, as classification would now: the step
/// after an administrator's pin is cleared. The seam over the indexer, which
/// reads NFOs from the library; [`LocalIndexService`] is the implementation.
#[async_trait]
pub trait TitleNfoPins: Send + Sync + std::fmt::Debug {
    async fn repin_from_nfos(&self, target: EnrichmentTargetId) -> Result<(), IndexError>;
}

#[async_trait]
impl TitleNfoPins for LocalIndexService {
    async fn repin_from_nfos(&self, target: EnrichmentTargetId) -> Result<(), IndexError> {
        self.repin_title_from_nfos(target).await
    }
}

/// Why an administrator's request was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("media {0} not found")]
    MediaNotFound(Uuid),
    #[error("library {0} not found")]
    LibraryNotFound(Uuid),
    /// Not a `"provider:id"` any provider issues.
    #[error("{0}")]
    InvalidExternalRef(String),
    /// No configured provider can resolve what was asked of it.
    #[error("{0}")]
    ProviderNotConfigured(String),
    /// Another title is already pinned to the id.
    #[error("{0}")]
    ExternalRefTaken(String),
    /// Fields enrichment never writes on a title of this kind: each one's
    /// position in the request, and why.
    #[error("{} field(s) cannot be locked on this title", .0.len())]
    FieldNotLockable(Vec<(usize, String)>),
    /// The provider failed to answer a search.
    #[error("the metadata provider failed: {0}")]
    Provider(String),
    #[error(transparent)]
    Db(#[from] DbErr),
    #[error(transparent)]
    Index(#[from] IndexError),
}

/// Where one title's enrichment stands, with the title it is for.
#[derive(Debug, Clone, PartialEq)]
pub struct TitleEnrichment {
    pub target: EnrichmentTargetId,
    pub title: String,
    pub year: Option<u32>,
    pub pinned_ref: Option<String>,
    pub pin_source: Option<PinSource>,
    pub status: EnrichmentStatus,
    pub attempts: u32,
    pub matched_ref: Option<String>,
    pub match_confidence: Option<f32>,
    pub last_error: Option<String>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub enriched_at: Option<DateTime<Utc>>,
    pub locked_fields: FieldLocks,
    /// When the enrichment row last changed; `None` for a title with no row
    /// yet (one indexed before enrichment existed, which the startup
    /// backfill queues).
    pub updated_at: Option<DateTime<Utc>>,
    /// Where the title sits in the admin list; `None` with no row.
    pub position: Option<EnrichmentListPosition>,
}

/// A page of the admin list.
#[derive(Debug, Clone, PartialEq)]
pub struct EnrichmentPage {
    pub items: Vec<TitleEnrichment>,
    /// Whether rows follow this page.
    pub has_next_page: bool,
    /// How many rows the filter admits in all.
    pub total: u64,
}

/// One title a provider offers for a fix-match, scored as the sweep scores
/// a search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchCandidate {
    /// The `"provider:id"` to fix the match to.
    pub external_ref: String,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u32>,
    /// How well it matches the query, `0.0..=1.0`.
    pub score: f64,
}

/// What a refresh queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshScope {
    Title(Uuid),
    Library(Uuid),
    All,
}

/// The stores and seams [`EnrichmentControl`] works through.
#[derive(Debug, Clone)]
pub struct EnrichmentControlDeps {
    pub movies: Arc<dyn MovieRepository>,
    pub shows: Arc<dyn ShowRepository>,
    pub states: Arc<dyn EnrichmentStateRepository>,
    pub libraries: Arc<dyn LibraryRepository>,
    pub provider: Arc<dyn EnrichmentProvider>,
    pub admin_log: Arc<dyn AdminLogService>,
    pub nfo_pins: Arc<dyn TitleNfoPins>,
    /// The enrichment worker's wake-up
    /// ([`super::MetadataEnrichmentService::notify_handle`]): every change
    /// that queues work pokes it, so the sweep starts at once rather than at
    /// its next interval.
    pub worker: Arc<Notify>,
}

/// An administrator's control of enrichment.
#[derive(Debug)]
pub struct EnrichmentControl {
    deps: EnrichmentControlDeps,
}

/// A title as the control reads it: its kind and the names it shows.
struct Title {
    target: EnrichmentTargetId,
    title: String,
    year: Option<u32>,
    pinned_ref: Option<String>,
    pin_source: Option<PinSource>,
}

impl EnrichmentControl {
    #[must_use]
    pub fn new(deps: EnrichmentControlDeps) -> Self {
        Self { deps }
    }

    /// The providers configured, as `available_providers` names them.
    #[must_use]
    pub fn available_providers(&self) -> Vec<String> {
        self.deps.provider.available_providers()
    }

    /// The title `id` names, a movie or a show.
    async fn resolve(&self, id: Uuid) -> Result<Title, ControlError> {
        if let Some(movie) = self.deps.movies.find_by_id(id).await? {
            return Ok(Title {
                target: EnrichmentTargetId::Movie(movie.id),
                title: movie.title,
                year: movie.year,
                pinned_ref: movie.pinned_ref,
                pin_source: movie.pin_source,
            });
        }
        if let Some(show) = self.deps.shows.find_by_id(id).await? {
            return Ok(Title {
                target: EnrichmentTargetId::Show(show.id),
                title: show.title,
                year: show.year,
                pinned_ref: show.pinned_ref,
                pin_source: show.pin_source,
            });
        }
        Err(ControlError::MediaNotFound(id))
    }

    fn view(title: Title, state: Option<EnrichmentState>) -> TitleEnrichment {
        let Title {
            target,
            title,
            year,
            pinned_ref,
            pin_source,
        } = title;
        match state {
            Some(state) => {
                let position = state.list_position();
                let EnrichmentState {
                    id: _,
                    target: _,
                    status,
                    attempts,
                    next_attempt_at,
                    enriched_at,
                    match_confidence,
                    matched_ref,
                    force_refresh: _,
                    last_error,
                    locked_fields,
                    updated_at,
                } = state;
                TitleEnrichment {
                    target,
                    title,
                    year,
                    pinned_ref,
                    pin_source,
                    status,
                    attempts,
                    matched_ref,
                    match_confidence,
                    last_error,
                    next_attempt_at,
                    enriched_at,
                    locked_fields,
                    updated_at: Some(updated_at),
                    position: Some(position),
                }
            }
            // Not queued yet: the startup backfill queues it, pending.
            None => TitleEnrichment {
                target,
                title,
                year,
                pinned_ref,
                pin_source,
                status: EnrichmentStatus::Pending,
                attempts: 0,
                matched_ref: None,
                match_confidence: None,
                last_error: None,
                next_attempt_at: None,
                enriched_at: None,
                locked_fields: FieldLocks::none(),
                updated_at: None,
                position: None,
            },
        }
    }

    /// Where the title `id` stands.
    pub async fn detail(&self, id: Uuid) -> Result<TitleEnrichment, ControlError> {
        let title = self.resolve(id).await?;
        let state = self.deps.states.find_by_target(title.target).await?;
        Ok(Self::view(title, state))
    }

    /// A page of the titles `filter` admits, most recently changed first,
    /// after `after` (FR-303).
    pub async fn list(
        &self,
        filter: EnrichmentListFilter,
        after: Option<EnrichmentListPosition>,
        limit: NonZeroU32,
    ) -> Result<EnrichmentPage, ControlError> {
        // One row past the page says whether another follows.
        let rows = self
            .deps
            .states
            .list(&EnrichmentListQuery {
                filter,
                after,
                limit: limit.saturating_add(1),
            })
            .await?;
        let total = self.deps.states.count(&filter).await?;
        let has_next_page = rows.len() > limit.get() as usize;
        let rows: Vec<EnrichmentState> = rows.into_iter().take(limit.get() as usize).collect();

        let mut movie_ids = Vec::new();
        let mut show_ids = Vec::new();
        for row in &rows {
            match row.target {
                EnrichmentTargetId::Movie(id) => movie_ids.push(id),
                EnrichmentTargetId::Show(id) => show_ids.push(id),
            }
        }
        let mut titles: HashMap<EnrichmentTargetId, Title> = HashMap::new();
        for movie in self.deps.movies.find_by_ids(&movie_ids).await? {
            titles.insert(
                EnrichmentTargetId::Movie(movie.id),
                Title {
                    target: EnrichmentTargetId::Movie(movie.id),
                    title: movie.title,
                    year: movie.year,
                    pinned_ref: movie.pinned_ref,
                    pin_source: movie.pin_source,
                },
            );
        }
        for show in self.deps.shows.find_by_ids(&show_ids).await? {
            titles.insert(
                EnrichmentTargetId::Show(show.id),
                Title {
                    target: EnrichmentTargetId::Show(show.id),
                    title: show.title,
                    year: show.year,
                    pinned_ref: show.pinned_ref,
                    pin_source: show.pin_source,
                },
            );
        }
        // A row whose title went between the two reads went with it
        // (`ON DELETE CASCADE`); it is simply not listed.
        let items = rows
            .into_iter()
            .filter_map(|row| {
                titles
                    .remove(&row.target)
                    .map(|title| Self::view(title, Some(row)))
            })
            .collect();
        Ok(EnrichmentPage {
            items,
            has_next_page,
            total,
        })
    }

    /// The configured providers' candidates for the title `id`, best first:
    /// searched by `query` (the title's own title when absent or blank) and
    /// `year` (the title's own when absent), scored as the sweep scores them.
    pub async fn candidates(
        &self,
        id: Uuid,
        query: Option<&str>,
        year: Option<u32>,
    ) -> Result<Vec<MatchCandidate>, ControlError> {
        let title = self.resolve(id).await?;
        if self.deps.provider.available_providers().is_empty() {
            return Err(no_providers());
        }
        let query = MediaQuery {
            title: query
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .map_or_else(|| title.title.clone(), str::to_owned),
            year: year.or(title.year),
        };
        let hits: Vec<(String, String, Option<String>, Option<u32>)> = match title.target {
            EnrichmentTargetId::Movie(_) => self
                .deps
                .provider
                .search_movies(&query)
                .await
                .map_err(provider_error)?
                .into_iter()
                .map(|hit| {
                    (
                        hit.external_ref.to_string(),
                        hit.title,
                        hit.original_title,
                        hit.year,
                    )
                })
                .collect(),
            EnrichmentTargetId::Show(_) => self
                .deps
                .provider
                .search_shows(&query)
                .await
                .map_err(provider_error)?
                .into_iter()
                .map(|hit| {
                    (
                        hit.external_ref.to_string(),
                        hit.title,
                        hit.original_title,
                        hit.year,
                    )
                })
                .collect(),
        };
        let mut candidates: Vec<MatchCandidate> = hits
            .into_iter()
            .map(|(external_ref, title, original_title, year)| {
                let score = matcher::score(
                    &query.title,
                    query.year,
                    &title,
                    original_title.as_deref(),
                    year,
                );
                MatchCandidate {
                    external_ref,
                    title,
                    original_title,
                    year,
                    score: score.total_score.clamp(0.0, 1.0),
                }
            })
            .collect();
        // Best first; a tie keeps the provider's order.
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        candidates.truncate(MAX_CANDIDATES);
        Ok(candidates)
    }

    /// Fix the title `id`'s match to `external_ref`, a `"provider:id"` a
    /// configured provider resolves: the title is pinned to it by
    /// `admin_user_id` -- an administrator's pin, which outranks and is never
    /// replaced by an NFO's (FR-312) -- and queued, its old match cleared, so
    /// the next pass fetches it by that id. `None` clears an administrator's
    /// pin instead: the title is pinned by its NFO again, or by nothing, and
    /// queued to be matched afresh.
    pub async fn fix_match(
        &self,
        id: Uuid,
        external_ref: Option<&str>,
        admin_user_id: &str,
    ) -> Result<TitleEnrichment, ControlError> {
        let title = self.resolve(id).await?;
        let target = title.target;
        let message = match external_ref {
            Some(external_ref) => {
                let pin = ProviderPin::parse(external_ref).ok_or_else(|| {
                    ControlError::InvalidExternalRef(format!(
                        "{external_ref:?} is not a provider id; expected \"provider:id\", \
                         such as \"tmdb:603\""
                    ))
                })?;
                if !self
                    .deps
                    .provider
                    .available_providers()
                    .iter()
                    .any(|provider| provider == pin.provider())
                {
                    return Err(ControlError::ProviderNotConfigured(format!(
                        "no configured metadata provider resolves {} ids",
                        pin.provider()
                    )));
                }
                let pinned = match target {
                    EnrichmentTargetId::Movie(id) => {
                        self.deps
                            .movies
                            .set_pinned_ref(id, &pin, PinSource::Admin)
                            .await?
                    }
                    EnrichmentTargetId::Show(id) => {
                        self.deps
                            .shows
                            .set_pinned_ref(id, &pin, PinSource::Admin)
                            .await?
                    }
                };
                if !pinned {
                    return Err(ControlError::ExternalRefTaken(format!(
                        "another title is already pinned to {pin}"
                    )));
                }
                format!("Fixed the match of \"{}\" to {pin}", title.title)
            }
            None => {
                let cleared = match target {
                    EnrichmentTargetId::Movie(id) => self.deps.movies.clear_admin_pin(id).await?,
                    EnrichmentTargetId::Show(id) => self.deps.shows.clear_admin_pin(id).await?,
                };
                if cleared {
                    self.deps.nfo_pins.repin_from_nfos(target).await?;
                }
                format!(
                    "Cleared the match of \"{}\" to be matched again",
                    title.title
                )
            }
        };
        self.deps.states.ensure_pending(target).await?;
        self.deps.states.request_refresh(target, true).await?;
        self.audit(
            message,
            admin_user_id,
            serde_json::json!({
                "media_id": target.id(),
                "kind": target.kind().as_str(),
                "external_ref": external_ref,
            }),
        )
        .await;
        self.deps.worker.notify_one();
        self.detail(id).await
    }

    /// Lock exactly `fields` on the title `id`, replacing what it had locked.
    pub async fn set_locks(
        &self,
        id: Uuid,
        fields: &[MetadataField],
        admin_user_id: &str,
    ) -> Result<TitleEnrichment, ControlError> {
        let title = self.resolve(id).await?;
        let kind = title.target.kind();
        let unlockable: Vec<(usize, String)> = fields
            .iter()
            .enumerate()
            .filter(|(_, field)| !field.applies_to(kind))
            .map(|(index, field)| {
                (
                    index,
                    format!(
                        "a {} has no {} for enrichment to write",
                        kind.as_str(),
                        field.as_str()
                    ),
                )
            })
            .collect();
        if !unlockable.is_empty() {
            return Err(ControlError::FieldNotLockable(unlockable));
        }
        let locks: FieldLocks = fields.iter().copied().collect();
        let state = self
            .deps
            .states
            .set_locked_fields(title.target, &locks)
            .await?;
        self.audit(
            format!(
                "Locked {} field(s) of \"{}\"",
                locks.to_stored().len(),
                title.title
            ),
            admin_user_id,
            serde_json::json!({
                "media_id": title.target.id(),
                "kind": kind.as_str(),
                "locked_fields": locks.to_stored(),
            }),
        )
        .await;
        Ok(Self::view(title, Some(state)))
    }

    /// Queue `scope` for another pass, regardless of where each title
    /// stands (FR-308); `rematch` clears each match so it is searched for
    /// again. Returns how many titles were queued.
    pub async fn refresh(
        &self,
        scope: RefreshScope,
        rematch: bool,
        admin_user_id: &str,
    ) -> Result<u64, ControlError> {
        let (queued, message) = match scope {
            RefreshScope::Title(id) => {
                let title = self.resolve(id).await?;
                self.deps.states.ensure_pending(title.target).await?;
                self.deps
                    .states
                    .request_refresh(title.target, rematch)
                    .await?;
                (1, format!("Queued \"{}\" for enrichment", title.title))
            }
            RefreshScope::Library(library_id) => {
                let library = self
                    .deps
                    .libraries
                    .find_by_id(library_id)
                    .await?
                    .ok_or(ControlError::LibraryNotFound(library_id))?;
                // A title with no row yet is given one, as a title refresh
                // gives it, so every title of the library is counted.
                let queued = self
                    .deps
                    .states
                    .request_refresh_library(library_id, rematch)
                    .await?;
                (
                    queued,
                    format!(
                        "Queued {queued} title(s) of library \"{}\" for enrichment",
                        library.name
                    ),
                )
            }
            RefreshScope::All => {
                let queued = self.deps.states.request_refresh_all(rematch).await?;
                (
                    queued,
                    format!("Queued all {queued} title(s) for enrichment"),
                )
            }
        };
        let scope_detail = match scope {
            RefreshScope::Title(id) => serde_json::json!({ "media_id": id }),
            RefreshScope::Library(id) => serde_json::json!({ "library_id": id }),
            RefreshScope::All => serde_json::json!({ "all": true }),
        };
        self.audit(
            message,
            admin_user_id,
            serde_json::json!({
                "scope": scope_detail,
                "rematch": rematch,
                "queued": queued,
            }),
        )
        .await;
        self.deps.worker.notify_one();
        Ok(queued)
    }

    /// Record what an administrator did, and who, in the admin log.
    async fn audit(&self, message: String, admin_user_id: &str, mut details: serde_json::Value) {
        if let Some(details) = details.as_object_mut() {
            details.insert(
                "admin_user_id".to_owned(),
                serde_json::Value::from(admin_user_id),
            );
        }
        if let Err(err) = self
            .deps
            .admin_log
            .log(
                AdminLogLevel::Info,
                AdminLogCategory::Enrichment,
                message,
                Some(details),
            )
            .await
        {
            tracing::warn!(error = %err, "could not record an administrator's enrichment action");
        }
    }
}

fn no_providers() -> ControlError {
    ControlError::ProviderNotConfigured(
        "no metadata provider is configured (set BEAM_TMDB_API_TOKEN, or enable AniList)"
            .to_owned(),
    )
}

fn provider_error(err: EnrichmentError) -> ControlError {
    match err {
        EnrichmentError::NotConfigured => no_providers(),
        EnrichmentError::NotFound
        | EnrichmentError::RateLimited { .. }
        | EnrichmentError::Transport(_)
        | EnrichmentError::Provider(_) => ControlError::Provider(err.to_string()),
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod control_tests;
