//! Background metadata enrichment: sweeps `metadata_enrichment` for rows due
//! for (re-)processing, resolves each against the configured
//! [`EnrichmentProvider`], and applies accepted matches to the movie/show
//! repositories.
//!
//! The provider is whatever `beam-server` wires: the cameo-backed
//! [`CameoEnrichmentProvider`](crate::providers::cameo::CameoEnrichmentProvider)
//! (TMDB and AniList) and TheTVDB's
//! [`TvdbEnrichmentProvider`](crate::providers::tvdb::TvdbEnrichmentProvider),
//! asked together through a
//! [`CompositeEnrichmentProvider`](crate::providers::composite::CompositeEnrichmentProvider)
//! when both are configured, either alone when only it is, and
//! [`NoopEnrichmentProvider`](beam_domain::providers::enrichment::NoopEnrichmentProvider)
//! -- every sweep a no-op -- when neither is.

pub mod control;
#[cfg(test)]
mod fault_tests;
pub mod matcher;

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use beam_domain::models::enrichment::{
    EnrichmentState, EnrichmentTargetId, FieldLocks, MetadataField,
};
use beam_domain::models::{CreateAdminLog, ProviderPin};
use beam_domain::providers::enrichment::{
    EnrichmentError, EnrichmentProvider, EpisodeEnrichment, ExternalMediaRef, MediaQuery,
    MovieEnrichment, SeasonEnrichment, ShowEnrichment,
};
use beam_domain::repositories::{
    AdminLogRepository, CollectionRepository, EnrichmentStateRepository, GenreRepository,
    MovieRepository, PersonRepository, ShowRepository,
};

use crate::services::notification::{
    EnrichmentEvent, EventCategory, EventLevel, NewAdminEvent, NotificationService,
};
use beam_domain::services::Clock;

/// Tunables for the enrichment sweep. All fields are independent of any
/// concrete provider so they can be exercised with
/// [`beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider`].
#[derive(Debug, Clone)]
pub struct EnrichmentPolicy {
    /// Max rows processed per `sweep_once` call.
    pub batch_size: u32,
    /// Minimum overall match confidence in `(0.0, 1.0]` a provider candidate
    /// must reach before its metadata is applied. Passed straight through to
    /// [`matcher::best_match`].
    pub min_confidence: f64,
    /// Attempts before a row is given up on (`Failed`).
    pub max_attempts: u32,
    /// Backoff delay applied after the Nth transient failure (0-indexed;
    /// the last entry is reused for any attempt count beyond its length).
    pub backoff_schedule: Vec<Duration>,
}

impl Default for EnrichmentPolicy {
    fn default() -> Self {
        Self {
            batch_size: 25,
            min_confidence: matcher::DEFAULT_MIN_CONFIDENCE,
            max_attempts: 5,
            backoff_schedule: vec![
                Duration::from_secs(60),
                Duration::from_secs(600),
                Duration::from_secs(3600),
                Duration::from_secs(21600),
                Duration::from_secs(86400),
            ],
        }
    }
}

impl EnrichmentPolicy {
    fn backoff_for(&self, attempts: u32) -> Duration {
        let idx = (attempts.saturating_sub(1)) as usize;
        self.backoff_schedule
            .get(idx)
            .or_else(|| self.backoff_schedule.last())
            .copied()
            .unwrap_or(FALLBACK_DELAY)
    }
}

/// The delay a retry waits when nothing better is known: a rate limit that
/// named no `Retry-After`, an empty backoff schedule, or a delay too long
/// for a timestamp to hold.
const FALLBACK_DELAY: Duration = Duration::from_secs(60);

/// The instant `delay` after `now`. A delay too long for `chrono` to
/// represent is read as [`FALLBACK_DELAY`] rather than failing the retry.
fn after(now: chrono::DateTime<chrono::Utc>, delay: Duration) -> chrono::DateTime<chrono::Utc> {
    let fallback = chrono::Duration::seconds(FALLBACK_DELAY.as_secs() as i64);
    now + chrono::Duration::from_std(delay).unwrap_or(fallback)
}

/// The provider ids a match reports, which a pin is checked against.
#[derive(Debug, Clone, Copy, Default)]
struct MatchIds<'a> {
    tmdb: Option<u32>,
    imdb: Option<&'a str>,
    anilist: Option<u32>,
    tvdb: Option<u32>,
}

impl<'a> MatchIds<'a> {
    fn of_movie(enrichment: &'a MovieEnrichment) -> Self {
        let MovieEnrichment {
            tmdb_id,
            imdb_id,
            anilist_id,
            ..
        } = enrichment;
        Self {
            tmdb: *tmdb_id,
            imdb: imdb_id.as_deref(),
            anilist: *anilist_id,
            // A movie carries no TheTVDB id: a TheTVDB pin names a series.
            tvdb: None,
        }
    }

    fn of_show(enrichment: &'a ShowEnrichment) -> Self {
        let ShowEnrichment {
            tmdb_id,
            imdb_id,
            anilist_id,
            tvdb_id,
            ..
        } = enrichment;
        Self {
            tmdb: *tmdb_id,
            imdb: imdb_id.as_deref(),
            anilist: *anilist_id,
            tvdb: *tvdb_id,
        }
    }
}

/// Why a match a search found contradicts `pin`: the match carries an id
/// of the pin's provider, and it is another. A match that carries no id of
/// that provider cannot contradict it.
fn contradiction(pin: &ProviderPin, ids: MatchIds<'_>) -> Option<String> {
    let found = match pin {
        ProviderPin::Tmdb(id) => ids
            .tmdb
            .filter(|found| found != id)
            .map(|f| format!("tmdb:{f}")),
        ProviderPin::Anilist(id) => ids
            .anilist
            .filter(|found| found != id)
            .map(|f| format!("anilist:{f}")),
        ProviderPin::Imdb(id) => ids
            .imdb
            .filter(|found| *found != id.as_str())
            .map(|f| format!("imdb:{f}")),
        ProviderPin::Tvdb(id) => ids
            .tvdb
            .filter(|found| found != id)
            .map(|f| format!("tvdb:{f}")),
    }?;
    Some(format!(
        "the NFO pins {pin}, but the best match found is {found}"
    ))
}

/// The most seasons read to find where an absolute number falls: past any
/// real show's length, so the walk is bounded however a provider answers.
const MAX_ORDER_SEASONS: u32 = 100;

/// A show's provider seasons, each fetched at most once per enrichment:
/// the season loop and the absolute order both read them. `None` records a
/// season the provider has none of, or failed to give.
#[derive(Debug, Default)]
struct SeasonCache {
    seasons: std::collections::BTreeMap<u32, Option<SeasonEnrichment>>,
}

impl SeasonCache {
    async fn get(
        &mut self,
        provider: &dyn EnrichmentProvider,
        show: &ExternalMediaRef,
        season_number: u32,
    ) -> Option<SeasonEnrichment> {
        if let Some(cached) = self.seasons.get(&season_number) {
            return cached.clone();
        }
        let fetched = match provider.season_enrichment(show, season_number).await {
            Ok(season) => Some(season),
            Err(EnrichmentError::NotFound) => None,
            Err(err) => {
                warn!(show = %show, season_number, error = %err, "fetching a season failed");
                None
            }
        };
        self.seasons.insert(season_number, fetched.clone());
        fetched
    }

    /// The provider's regular seasons from the first, in order, until one
    /// is missing or they hold `through` episodes -- enough to place every
    /// absolute number up to it.
    async fn absolute_order(
        &mut self,
        provider: &dyn EnrichmentProvider,
        show: &ExternalMediaRef,
        through: u32,
    ) -> Vec<SeasonEnrichment> {
        let mut order = Vec::new();
        let mut episodes = 0usize;
        for season_number in 1..=MAX_ORDER_SEASONS {
            if episodes >= through as usize {
                break;
            }
            let Some(season) = self.get(provider, show, season_number).await else {
                break;
            };
            episodes += season.episodes.len();
            order.push(season);
        }
        order
    }
}

/// A title's enrichment fetched by the id it was matched to, with the
/// confidence of the match, or why the fetch failed.
type Fetched<T> = Result<(ExternalMediaRef, T, f32), EnrichmentError>;

/// What an exhausted row becomes.
#[derive(Debug, Clone, Copy)]
enum Exhausted {
    /// The provider kept failing.
    Failed,
    /// The provider kept answering that it has no such title.
    Unmatched,
}

/// Outcome of processing a single due row.
#[derive(Debug, PartialEq, Eq)]
enum ProcessOutcome {
    Enriched,
    Unmatched,
    Retrying,
    Failed,
    RateLimited,
}

/// Summary of a `sweep_once` call, for logging/tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub processed: u32,
    pub enriched: u32,
    pub unmatched: u32,
    pub retrying: u32,
    pub failed: u32,
    /// Set when the sweep stopped early because the provider reported
    /// rate-limiting -- the sweep never spins against a provider that just
    /// asked us to back off.
    pub rate_limited: bool,
    /// Set when the sweep did no work because no providers are configured.
    pub skipped_no_providers: bool,
}

#[derive(Debug)]
pub struct MetadataEnrichmentService {
    state_repo: Arc<dyn EnrichmentStateRepository>,
    movie_repo: Arc<dyn MovieRepository>,
    show_repo: Arc<dyn ShowRepository>,
    genre_repo: Arc<dyn GenreRepository>,
    /// Where a match's cast and crew are stored (issue #261).
    person_repo: Arc<dyn PersonRepository>,
    /// Where a movie match's collection is stored (issue #262).
    collection_repo: Arc<dyn CollectionRepository>,
    provider: Arc<dyn EnrichmentProvider>,
    admin_log: Arc<dyn AdminLogRepository>,
    notifications: Arc<dyn NotificationService>,
    clock: Arc<dyn Clock>,
    policy: EnrichmentPolicy,
    notify: Arc<tokio::sync::Notify>,
}

impl MetadataEnrichmentService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state_repo: Arc<dyn EnrichmentStateRepository>,
        movie_repo: Arc<dyn MovieRepository>,
        show_repo: Arc<dyn ShowRepository>,
        genre_repo: Arc<dyn GenreRepository>,
        person_repo: Arc<dyn PersonRepository>,
        collection_repo: Arc<dyn CollectionRepository>,
        provider: Arc<dyn EnrichmentProvider>,
        admin_log: Arc<dyn AdminLogRepository>,
        notifications: Arc<dyn NotificationService>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            state_repo,
            movie_repo,
            show_repo,
            genre_repo,
            person_repo,
            collection_repo,
            provider,
            admin_log,
            notifications,
            clock,
            policy: EnrichmentPolicy::default(),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn with_policy(mut self, policy: EnrichmentPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// A handle scans (or admin refresh requests) can use to poke the worker
    /// loop into sweeping immediately instead of waiting for the interval
    /// backstop.
    pub fn notify_handle(&self) -> Arc<tokio::sync::Notify> {
        self.notify.clone()
    }

    /// Runs the sweep loop forever: sweep, then wait for either a poke via
    /// [`Self::notify_handle`] or `interval`, whichever comes first.
    pub async fn run(&self, interval: Duration) -> ! {
        loop {
            let report = self.sweep_once().await;
            if report.enriched > 0 || report.unmatched > 0 || report.failed > 0 {
                info!(
                    enriched = report.enriched,
                    unmatched = report.unmatched,
                    retrying = report.retrying,
                    failed = report.failed,
                    "metadata enrichment sweep completed"
                );
            }
            tokio::select! {
                _ = self.notify.notified() => {}
                _ = self.clock.sleep(interval) => {}
            }
        }
    }

    /// Processes up to `policy.batch_size` due rows. Stops early if the
    /// provider reports rate-limiting.
    pub async fn sweep_once(&self) -> SweepReport {
        if self.provider.available_providers().is_empty() {
            return SweepReport {
                skipped_no_providers: true,
                ..Default::default()
            };
        }

        let mut report = SweepReport::default();
        let due = match self
            .state_repo
            .fetch_due(self.clock.now(), self.policy.batch_size)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                warn!(error = %err, "failed to fetch due enrichment rows");
                return report;
            }
        };

        for row in due {
            report.processed += 1;
            match self.process_row(&row).await {
                ProcessOutcome::Enriched => report.enriched += 1,
                ProcessOutcome::Unmatched => report.unmatched += 1,
                ProcessOutcome::Retrying => report.retrying += 1,
                ProcessOutcome::Failed => report.failed += 1,
                ProcessOutcome::RateLimited => {
                    report.rate_limited = true;
                    break;
                }
            }
        }

        // Prometheus counters, emitted from the report tallies already kept
        // above so no extra state is threaded through the row processing.
        // Each call is a no-op unless beam-server installed a recorder
        // (BEAM_ENABLE_METRICS=true).
        for (outcome, count) in [
            ("enriched", u64::from(report.enriched)),
            ("unmatched", u64::from(report.unmatched)),
            ("retrying", u64::from(report.retrying)),
            ("failed", u64::from(report.failed)),
            ("rate_limited", u64::from(report.rate_limited)),
        ] {
            if count > 0 {
                metrics::counter!("beam_enrichment_outcomes_total", "outcome" => outcome)
                    .increment(count);
            }
        }

        report
    }

    async fn process_row(&self, row: &EnrichmentState) -> ProcessOutcome {
        let (outcome, title) = match row.target {
            EnrichmentTargetId::Movie(id) => self.process::<Movies>(row, id).await,
            EnrichmentTargetId::Show(id) => self.process::<Shows>(row, id).await,
        };
        self.announce(row, &outcome, title).await;
        outcome
    }

    /// Tell the admin event stream what became of `row` (FR-309): one event
    /// per title the sweep attempted, carrying the row as it now stands. A
    /// rate-limited row was not attempted, so it says nothing.
    async fn announce(
        &self,
        row: &EnrichmentState,
        outcome: &ProcessOutcome,
        title: Option<String>,
    ) {
        let level = match outcome {
            ProcessOutcome::Enriched => EventLevel::Info,
            ProcessOutcome::Unmatched | ProcessOutcome::Retrying => EventLevel::Warning,
            ProcessOutcome::Failed => EventLevel::Error,
            ProcessOutcome::RateLimited => return,
        };
        let stored = match self.state_repo.find_by_target(row.target).await {
            Ok(Some(stored)) => stored,
            // Gone with its title, or unreadable: nothing true to say.
            Ok(None) | Err(_) => return,
        };
        let EnrichmentState {
            status,
            matched_ref,
            last_error,
            ..
        } = stored;
        let name = title.as_deref().unwrap_or("a title");
        let message = match outcome {
            ProcessOutcome::Enriched => format!("Enriched \"{name}\""),
            ProcessOutcome::Unmatched => format!("No enrichment match for \"{name}\""),
            ProcessOutcome::Retrying => format!("Enrichment of \"{name}\" will be retried"),
            ProcessOutcome::Failed => format!("Enrichment of \"{name}\" failed"),
            ProcessOutcome::RateLimited => return,
        };
        let event = NewAdminEvent::new(level, EventCategory::Enrichment, message, None, None)
            .with_enrichment(EnrichmentEvent {
                target: row.target,
                title,
                status,
                matched_ref,
                error: match outcome {
                    ProcessOutcome::Enriched => None,
                    _ => last_error,
                },
            });
        self.notifications.publish(event);
    }

    /// Search for `title` by its name and year and fetch the best match,
    /// which must not contradict `pin`. `Err` is the outcome, already
    /// recorded, when the search settles the row without a match.
    async fn search<K: TitleResolver>(
        &self,
        row: &EnrichmentState,
        title_id: uuid::Uuid,
        title: &K::Title,
        pin: Option<&ProviderPin>,
    ) -> Result<Fetched<K::Enrichment>, ProcessOutcome> {
        let name = K::title(title);
        let year = K::year(title);
        let query = MediaQuery {
            title: name.to_string(),
            year,
        };
        let hits = match K::search(&*self.provider, &query).await {
            Ok(hits) => hits,
            Err(err) => return Err(self.handle_error(row, err).await),
        };
        let Some((hit, score)) = matcher::best_match(name, year, &hits, self.policy.min_confidence)
        else {
            self.log_unmatched::<K>(row.id, name, year, &hits).await;
            return Err(self
                .mark_unmatched(row, "no candidate cleared the match threshold")
                .await);
        };
        let ref_id = matcher::SearchHit::external_ref(hit).clone();
        let enrichment = match K::fetch(&*self.provider, &ref_id).await {
            Ok(enrichment) => enrichment,
            Err(err) => return Ok(Err(err)),
        };
        if let Some(reason) = pin.and_then(|pin| contradiction(pin, K::match_ids(&enrichment))) {
            warn!(
                kind = K::NOUN,
                title_id = %title_id,
                candidate = %ref_id,
                reason,
                "the best match contradicts the NFO's pin"
            );
            return Err(self.mark_unmatched(row, &reason).await);
        }
        Ok(Ok((ref_id, enrichment, score.total_score as f32)))
    }

    /// Clear `row`'s match, `gone`, which the provider no longer has, so no
    /// later pass asks for it again: whatever the search or the fetch by the
    /// pin that follows finds, or fails to, the row no longer names it.
    async fn forget_match(&self, row: &EnrichmentState, gone: &ExternalMediaRef) {
        info!(
            target = ?row.target,
            matched = %gone,
            "the provider no longer has a title's match; forgetting it"
        );
        if let Err(err) = self.state_repo.request_refresh(row.target, true).await {
            warn!(error = %err, "could not clear a match the provider no longer has");
        }
    }

    /// Resolve the title `row` targets, returning the outcome and, when the
    /// title still exists, its display title.
    async fn process<K: TitleResolver>(
        &self,
        row: &EnrichmentState,
        title_id: uuid::Uuid,
    ) -> (ProcessOutcome, Option<String>) {
        let Some(title) = K::find(self, title_id).await else {
            self.terminal_failure(row, K::GONE).await;
            return (ProcessOutcome::Failed, None);
        };
        let name = Some(K::title(&title).to_string());
        (self.resolve::<K>(row, title_id, title).await, name)
    }

    async fn resolve<K: TitleResolver>(
        &self,
        row: &EnrichmentState,
        title_id: uuid::Uuid,
        title: K::Title,
    ) -> ProcessOutcome {
        let external_ref = if row.force_refresh {
            row.matched_ref.as_deref().and_then(ExternalMediaRef::parse)
        } else {
            None
        };

        // A title an NFO or an administrator pins (issues #184, #185) is
        // fetched by its pin when a configured provider resolves it, never
        // searched for; one pinned to an id only a match can be checked
        // against is searched, and the match must not contradict the pin. A
        // refresh of a match already made (`force_refresh` with
        // `matched_ref`) comes first. When the provider has no title for the
        // match, an unpinned title is searched for afresh, and a pinned one
        // whose match differs from its pin forgets the match and is fetched
        // by its pin instead (issue #246). Only a fetch by the pin itself --
        // a dead match equal to the pin, the pin fetched in place of a dead
        // match, or the pin when no match is refreshed -- is retried with
        // backoff (FR-304, FR-313).
        let (pin, pinned_ref) = self.pin_of(K::pinned_ref(&title));
        let mut by_id = external_ref.clone().or_else(|| pinned_ref.clone());
        let resolved = match (external_ref, pinned_ref) {
            (Some(ref_id), pinned_ref) => {
                self.provider.invalidate(&ref_id).await;
                match K::fetch(&*self.provider, &ref_id).await {
                    // The match of a title no pin holds, gone from the
                    // provider: it is searched for again, as a title never
                    // matched is, rather than asked for by a dead id.
                    Err(EnrichmentError::NotFound) if pinned_ref.is_none() => {
                        self.forget_match(row, &ref_id).await;
                        by_id = None;
                        match self.search::<K>(row, title_id, &title, pin.as_ref()).await {
                            Ok(fetched) => fetched,
                            Err(outcome) => return outcome,
                        }
                    }
                    // The match of a pinned title, gone from the provider: it
                    // is fetched by its pin instead -- one a provider
                    // configured since the match was made resolves (issue
                    // #246) -- rather than asked for by the dead id forever.
                    Err(EnrichmentError::NotFound)
                        if pinned_ref.as_ref().is_some_and(|pin| *pin != ref_id) =>
                    {
                        self.forget_match(row, &ref_id).await;
                        let pinned_ref = pinned_ref.unwrap_or(ref_id);
                        by_id = Some(pinned_ref.clone());
                        K::fetch(&*self.provider, &pinned_ref)
                            .await
                            .map(|e| (pinned_ref, e, 1.0))
                    }
                    fetched => fetched.map(|e| (ref_id, e, row.match_confidence.unwrap_or(1.0))),
                }
            }
            (None, Some(ref_id)) => K::fetch(&*self.provider, &ref_id)
                .await
                .map(|e| (ref_id, e, 1.0)),
            (None, None) => match self.search::<K>(row, title_id, &title, pin.as_ref()).await {
                Ok(fetched) => fetched,
                Err(outcome) => return outcome,
            },
        };

        match resolved {
            Ok((ref_id, enrichment, confidence)) => {
                let locks = &row.locked_fields;
                if let Err(err) = K::apply(self, title_id, &enrichment, locks).await {
                    warn!(
                        error = %err,
                        kind = K::NOUN,
                        title_id = %title_id,
                        "failed to persist a title's enrichment"
                    );
                    return self
                        .mark_unmatched(row, "failed to persist enrichment")
                        .await;
                }
                K::after_match(self, title_id, &enrichment, &ref_id, locks).await;
                self.mark_enriched(row, ref_id.as_str(), confidence).await
            }
            Err(EnrichmentError::NotFound) if by_id.is_some() => {
                self.missing_by_id(row, by_id.as_ref()).await
            }
            Err(err) => self.handle_error(row, err).await,
        }
    }

    /// Best-effort: enrich every season the local library already has files
    /// for from the provider's season of the same number. Never fabricates
    /// seasons or episodes that scanning hasn't created.
    ///
    /// A local episode the provider's season has no episode for is read as
    /// an absolute number (issue #267): `Show - 030` is indexed as episode 30
    /// of season 1, or of its season folder's season, and the provider's
    /// absolute order ([`matcher::absolute_episode`]) says which episode that
    /// is. It takes that episode's metadata where it stands. Episode 30 of
    /// season 1 is taken wherever the order places it; of a later season,
    /// only when the order places it in that season, since a season folder
    /// and the number must agree. Specials keep their own numbering: the
    /// absolute order counts none of them.
    async fn enrich_seasons(&self, show_id: uuid::Uuid, ref_id: &ExternalMediaRef) {
        let Ok(seasons) = self.show_repo.find_seasons_by_show_id(show_id).await else {
            return;
        };
        let mut fetched = SeasonCache::default();
        for season in seasons {
            let season_number = season.season_number;
            let provider_season = fetched.get(&*self.provider, ref_id, season_number).await;
            let unplaced: Vec<u32> = match self
                .show_repo
                .find_episodes_by_season_id(season.id)
                .await
            {
                Ok(episodes) => episodes
                    .into_iter()
                    .map(|episode| episode.episode_number)
                    .filter(|number| {
                        provider_season.as_ref().is_none_or(|provider| {
                            !provider
                                .episodes
                                .iter()
                                .any(|episode| episode.episode_number == *number)
                        })
                    })
                    .collect(),
                Err(err) => {
                    warn!(error = %err, show_id = %show_id, "failed to read a season's episodes");
                    Vec::new()
                }
            };

            let mut placed = Vec::new();
            if season_number != matcher::SPECIALS_SEASON && !unplaced.is_empty() {
                let order = fetched
                    .absolute_order(
                        &*self.provider,
                        ref_id,
                        unplaced.iter().copied().max().unwrap_or(0),
                    )
                    .await;
                for number in unplaced {
                    let Some((in_season, episode)) = matcher::absolute_episode(&order, number)
                    else {
                        continue;
                    };
                    if season_number != 1 && in_season != season_number {
                        continue;
                    }
                    placed.push(EpisodeEnrichment {
                        episode_number: number,
                        ..episode.clone()
                    });
                }
            }

            // With no provider season of this number nothing is ever placed:
            // the absolute order stops at the first season the provider lacks,
            // so it holds no episode of this season. There is nothing to apply,
            // and applying an empty season would clear its poster and air date.
            let Some(mut season_enrichment) = provider_season else {
                continue;
            };
            season_enrichment.episodes.extend(placed);
            if let Err(err) = self
                .show_repo
                .apply_season_enrichment(show_id, &season_enrichment)
                .await
            {
                warn!(error = %err, show_id = %show_id, season_number, "failed to persist season enrichment");
            }
        }
    }

    /// A pinned title fetched by an id -- its pin, or its match being
    /// refreshed -- that the provider says it has no title for. A provider's
    /// "not found" can be a passing fault, so it is retried with backoff like
    /// any other failure (FR-304); once the attempts are spent the title is
    /// left unmatched, naming the id, for an administrator to fix the match
    /// (issue #185). An unpinned title never gets here: its lost match is
    /// searched for again instead.
    async fn missing_by_id(
        &self,
        row: &EnrichmentState,
        id: Option<&ExternalMediaRef>,
    ) -> ProcessOutcome {
        let id = id.map_or_else(String::new, ToString::to_string);
        self.retry_or(
            row,
            &format!("the provider has no title {id}"),
            Exhausted::Unmatched,
        )
        .await
    }

    /// The pin a title carries, and the reference to fetch it by when a
    /// configured provider resolves the pin's ids: a pin is fetched by its id
    /// when its provider is configured -- TMDB and AniList through cameo
    /// (decision D184-2), TheTVDB through its own client when one is
    /// configured (issue #267) -- and otherwise, as an IMDb pin always is,
    /// only checked against a match.
    fn pin_of(&self, pinned_ref: Option<&str>) -> (Option<ProviderPin>, Option<ExternalMediaRef>) {
        let pin = pinned_ref.and_then(ProviderPin::parse);
        let fetchable = pin
            .as_ref()
            .filter(|pin| {
                self.provider
                    .available_providers()
                    .iter()
                    .any(|provider| provider == pin.provider())
            })
            .and_then(|pin| ExternalMediaRef::parse(&pin.to_ref_string()));
        (pin, fetchable)
    }

    async fn handle_error(&self, row: &EnrichmentState, err: EnrichmentError) -> ProcessOutcome {
        match err {
            EnrichmentError::RateLimited { retry_after } => {
                let now = self.clock.now();
                let next_attempt_at = after(now, retry_after.unwrap_or(FALLBACK_DELAY));
                let _ = self
                    .state_repo
                    .mark_retrying(
                        row.id,
                        "rate limited by provider",
                        row.attempts,
                        next_attempt_at,
                        now,
                    )
                    .await;
                ProcessOutcome::RateLimited
            }
            EnrichmentError::NotConfigured => {
                self.terminal_failure(row, "no enrichment providers configured")
                    .await;
                ProcessOutcome::Failed
            }
            EnrichmentError::NotFound
            | EnrichmentError::Transport(_)
            | EnrichmentError::Provider(_) => {
                self.retry_or(row, &err.to_string(), Exhausted::Failed)
                    .await
            }
        }
    }

    /// Schedule `row`'s next attempt after the backoff its attempts have
    /// earned, recording `reason`; once they are spent, settle it as
    /// `exhausted` says instead.
    async fn retry_or(
        &self,
        row: &EnrichmentState,
        reason: &str,
        exhausted: Exhausted,
    ) -> ProcessOutcome {
        let attempts = row.attempts + 1;
        if attempts >= self.policy.max_attempts {
            return match exhausted {
                Exhausted::Failed => {
                    self.terminal_failure(row, reason).await;
                    ProcessOutcome::Failed
                }
                Exhausted::Unmatched => self.mark_unmatched(row, reason).await,
            };
        }
        let now = self.clock.now();
        let next_attempt_at = after(now, self.policy.backoff_for(attempts));
        let _ = self
            .state_repo
            .mark_retrying(row.id, reason, attempts, next_attempt_at, now)
            .await;
        ProcessOutcome::Retrying
    }

    async fn mark_enriched(
        &self,
        row: &EnrichmentState,
        matched_ref: &str,
        confidence: f32,
    ) -> ProcessOutcome {
        let _ = self
            .state_repo
            .mark_enriched(row.id, matched_ref, confidence, self.clock.now())
            .await;
        ProcessOutcome::Enriched
    }

    async fn mark_unmatched(&self, row: &EnrichmentState, reason: &str) -> ProcessOutcome {
        let _ = self
            .state_repo
            .mark_unmatched(row.id, reason, self.clock.now())
            .await;
        ProcessOutcome::Unmatched
    }

    async fn terminal_failure(&self, row: &EnrichmentState, reason: &str) {
        let _ = self
            .state_repo
            .mark_failed(row.id, reason, self.clock.now())
            .await;
    }

    async fn log_unmatched<K: TitleResolver>(
        &self,
        row_id: uuid::Uuid,
        title: &str,
        year: Option<u32>,
        hits: &[K::Hit],
    ) {
        let top = matcher::top_candidates(title, year, hits, 3);
        let details = serde_json::json!({ "row_id": row_id.to_string(), "title": title, "year": year, "candidates": top.iter().map(|(label, score)| serde_json::json!({ "candidate": label, "score": score.total_score })).collect::<Vec<_>>() });
        let _ = self
            .admin_log
            .create(CreateAdminLog {
                level: beam_domain::models::AdminLogLevel::Warning,
                category: beam_domain::models::AdminLogCategory::Enrichment,
                message: format!("No enrichment match found for {} \"{title}\"", K::NOUN),
                details: Some(details),
            })
            .await;
    }
}

/// One kind of title the sweep enriches, and the steps of enriching it that
/// differ by kind. Which id is fetched -- a refresh of the match already
/// made, else the pin, else a search -- is
/// [`MetadataEnrichmentService::resolve`]'s, the same for every kind.
trait TitleResolver {
    /// The stored title.
    type Title: Send + Sync;
    /// What a provider knows of one.
    type Enrichment: Send + Sync;
    /// One result of a search for one.
    type Hit: matcher::SearchHit + Send + Sync;
    /// The kind's name in messages.
    const NOUN: &'static str;
    /// Why a row whose title no longer exists failed.
    const GONE: &'static str;

    fn title(title: &Self::Title) -> &str;
    fn year(title: &Self::Title) -> Option<u32>;
    fn pinned_ref(title: &Self::Title) -> Option<&str>;
    /// The provider ids `enrichment` reports, which a pin is checked against.
    fn match_ids(enrichment: &Self::Enrichment) -> MatchIds<'_>;

    /// The title `id`, `None` when it no longer exists or cannot be read.
    fn find(
        service: &MetadataEnrichmentService,
        id: uuid::Uuid,
    ) -> impl Future<Output = Option<Self::Title>> + Send;
    fn search(
        provider: &dyn EnrichmentProvider,
        query: &MediaQuery,
    ) -> impl Future<Output = Result<Vec<Self::Hit>, EnrichmentError>> + Send;
    fn fetch(
        provider: &dyn EnrichmentProvider,
        id: &ExternalMediaRef,
    ) -> impl Future<Output = Result<Self::Enrichment, EnrichmentError>> + Send;
    /// Store `enrichment` over the title's unlocked fields. A failure
    /// leaves the title unmatched.
    fn apply(
        service: &MetadataEnrichmentService,
        id: uuid::Uuid,
        enrichment: &Self::Enrichment,
        locks: &FieldLocks,
    ) -> impl Future<Output = Result<(), sea_orm::DbErr>> + Send;
    /// What a stored match brings beside the title's own fields -- genres,
    /// credits, and a movie's collection or a show's seasons -- each
    /// best-effort: a title whose genres, credits, collection or seasons
    /// fail to store is still enriched, and keeps what it had.
    fn after_match(
        service: &MetadataEnrichmentService,
        id: uuid::Uuid,
        enrichment: &Self::Enrichment,
        ref_id: &ExternalMediaRef,
        locks: &FieldLocks,
    ) -> impl Future<Output = ()> + Send;
}

/// Movies, for [`TitleResolver`].
struct Movies;

impl TitleResolver for Movies {
    type Title = beam_domain::models::Movie;
    type Enrichment = MovieEnrichment;
    type Hit = beam_domain::providers::enrichment::MovieSearchHit;
    const NOUN: &'static str = "movie";
    const GONE: &'static str = "movie no longer exists";

    fn title(title: &Self::Title) -> &str {
        &title.title
    }

    fn year(title: &Self::Title) -> Option<u32> {
        title.year
    }

    fn pinned_ref(title: &Self::Title) -> Option<&str> {
        title.pinned_ref.as_deref()
    }

    fn match_ids(enrichment: &Self::Enrichment) -> MatchIds<'_> {
        MatchIds::of_movie(enrichment)
    }

    async fn find(service: &MetadataEnrichmentService, id: uuid::Uuid) -> Option<Self::Title> {
        service.movie_repo.find_by_id(id).await.ok().flatten()
    }

    async fn search(
        provider: &dyn EnrichmentProvider,
        query: &MediaQuery,
    ) -> Result<Vec<Self::Hit>, EnrichmentError> {
        provider.search_movies(query).await
    }

    async fn fetch(
        provider: &dyn EnrichmentProvider,
        id: &ExternalMediaRef,
    ) -> Result<Self::Enrichment, EnrichmentError> {
        provider.movie_enrichment(id).await
    }

    async fn apply(
        service: &MetadataEnrichmentService,
        id: uuid::Uuid,
        enrichment: &Self::Enrichment,
        locks: &FieldLocks,
    ) -> Result<(), sea_orm::DbErr> {
        service
            .movie_repo
            .apply_enrichment(id, enrichment, locks)
            .await
    }

    async fn after_match(
        service: &MetadataEnrichmentService,
        id: uuid::Uuid,
        enrichment: &Self::Enrichment,
        _ref_id: &ExternalMediaRef,
        locks: &FieldLocks,
    ) {
        if !locks.is_locked(MetadataField::Genres) {
            let _ = service
                .genre_repo
                .set_movie_genres(id, &enrichment.genres)
                .await;
        }
        if let Some(credits) = &enrichment.credits
            && let Err(err) = service.person_repo.set_movie_credits(id, credits).await
        {
            warn!(error = %err, movie_id = %id, "failed to persist movie credits");
        }
        if let Err(err) = service
            .collection_repo
            .set_movie_collection(id, enrichment.collection.as_ref())
            .await
        {
            warn!(error = %err, movie_id = %id, "failed to persist movie collection");
        }
    }
}

/// Shows, for [`TitleResolver`].
struct Shows;

impl TitleResolver for Shows {
    type Title = beam_domain::models::Show;
    type Enrichment = ShowEnrichment;
    type Hit = beam_domain::providers::enrichment::ShowSearchHit;
    const NOUN: &'static str = "show";
    const GONE: &'static str = "show no longer exists";

    fn title(title: &Self::Title) -> &str {
        &title.title
    }

    fn year(title: &Self::Title) -> Option<u32> {
        title.year
    }

    fn pinned_ref(title: &Self::Title) -> Option<&str> {
        title.pinned_ref.as_deref()
    }

    fn match_ids(enrichment: &Self::Enrichment) -> MatchIds<'_> {
        MatchIds::of_show(enrichment)
    }

    async fn find(service: &MetadataEnrichmentService, id: uuid::Uuid) -> Option<Self::Title> {
        service.show_repo.find_by_id(id).await.ok().flatten()
    }

    async fn search(
        provider: &dyn EnrichmentProvider,
        query: &MediaQuery,
    ) -> Result<Vec<Self::Hit>, EnrichmentError> {
        provider.search_shows(query).await
    }

    async fn fetch(
        provider: &dyn EnrichmentProvider,
        id: &ExternalMediaRef,
    ) -> Result<Self::Enrichment, EnrichmentError> {
        provider.show_enrichment(id).await
    }

    async fn apply(
        service: &MetadataEnrichmentService,
        id: uuid::Uuid,
        enrichment: &Self::Enrichment,
        locks: &FieldLocks,
    ) -> Result<(), sea_orm::DbErr> {
        service
            .show_repo
            .apply_enrichment(id, enrichment, locks)
            .await
    }

    async fn after_match(
        service: &MetadataEnrichmentService,
        id: uuid::Uuid,
        enrichment: &Self::Enrichment,
        ref_id: &ExternalMediaRef,
        locks: &FieldLocks,
    ) {
        if !locks.is_locked(MetadataField::Genres) {
            let _ = service
                .genre_repo
                .set_show_genres(id, &enrichment.genres)
                .await;
        }
        if let Some(credits) = &enrichment.credits
            && let Err(err) = service.person_repo.set_show_credits(id, credits).await
        {
            warn!(error = %err, show_id = %id, "failed to persist show credits");
        }
        service.enrich_seasons(id, ref_id).await;
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
