//! Background metadata enrichment: sweeps `metadata_enrichment` for rows due
//! for (re-)processing, resolves each against the configured
//! [`EnrichmentProvider`], and applies accepted matches to the movie/show
//! repositories. Wired to [`beam_domain::providers::enrichment::NoopEnrichmentProvider`]
//! in production for this commit; the real cameo-backed adapter lands in D4.

pub mod matcher;

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use beam_domain::models::enrichment::{EnrichmentState, EnrichmentTargetId};
use beam_domain::providers::enrichment::{
    EnrichmentError, EnrichmentProvider, ExternalMediaRef, MediaQuery,
};
use beam_domain::repositories::{
    EnrichmentStateRepository, GenreRepository, MovieRepository, ShowRepository,
};

use crate::services::admin_log::AdminLogService;
use crate::services::clock::Clock;

/// Tunables for the enrichment sweep. All fields are independent of any
/// concrete provider so they can be exercised with
/// [`beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider`].
#[derive(Debug, Clone)]
pub struct EnrichmentPolicy {
    /// Max rows processed per `sweep_once` call.
    pub batch_size: u32,
    /// Minimum overall match confidence in `(0.0, 1.0]` a provider candidate
    /// must reach before its metadata is applied. Passed straight through to
    /// [`matcher::best_movie_match`]/[`matcher::best_show_match`].
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
            .unwrap_or(Duration::from_secs(60))
    }
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
    provider: Arc<dyn EnrichmentProvider>,
    admin_log: Arc<dyn AdminLogService>,
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
        provider: Arc<dyn EnrichmentProvider>,
        admin_log: Arc<dyn AdminLogService>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            state_repo,
            movie_repo,
            show_repo,
            genre_repo,
            provider,
            admin_log,
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
        match row.target {
            EnrichmentTargetId::Movie(id) => self.process_movie(row, id).await,
            EnrichmentTargetId::Show(id) => self.process_show(row, id).await,
        }
    }

    async fn process_movie(&self, row: &EnrichmentState, movie_id: uuid::Uuid) -> ProcessOutcome {
        let Ok(Some(movie)) = self.movie_repo.find_by_id(movie_id).await else {
            self.terminal_failure(row, "movie no longer exists").await;
            return ProcessOutcome::Failed;
        };

        let external_ref = if row.force_refresh {
            row.matched_ref.as_deref().and_then(ExternalMediaRef::parse)
        } else {
            None
        };

        let resolved = match external_ref {
            Some(ref_id) => {
                self.provider.invalidate(&ref_id).await;
                self.provider
                    .movie_enrichment(&ref_id)
                    .await
                    .map(|e| (ref_id, e, row.match_confidence.unwrap_or(1.0)))
            }
            None => {
                let query = MediaQuery {
                    title: movie.title.clone(),
                    year: movie.year,
                };
                match self.provider.search_movies(&query).await {
                    Ok(hits) => match matcher::best_movie_match(
                        &movie.title,
                        movie.year,
                        &hits,
                        self.policy.min_confidence,
                    ) {
                        Some((hit, score)) => {
                            let ref_id = hit.external_ref.clone();
                            self.provider
                                .movie_enrichment(&ref_id)
                                .await
                                .map(|e| (ref_id, e, score.total_score as f32))
                        }
                        None => {
                            self.log_unmatched_movie(row.id, &movie.title, movie.year, &hits)
                                .await;
                            return self
                                .mark_unmatched(row, "no candidate cleared the match threshold")
                                .await;
                        }
                    },
                    Err(err) => return self.handle_error(row, err).await,
                }
            }
        };

        match resolved {
            Ok((ref_id, enrichment, confidence)) => {
                let genres = enrichment.genres.clone();
                if let Err(err) = self
                    .movie_repo
                    .apply_enrichment(movie_id, &enrichment)
                    .await
                {
                    warn!(error = %err, movie_id = %movie_id, "failed to persist movie enrichment");
                    return self
                        .mark_unmatched(row, "failed to persist enrichment")
                        .await;
                }
                let _ = self.genre_repo.set_movie_genres(movie_id, &genres).await;
                self.mark_enriched(row, ref_id.as_str(), confidence).await
            }
            Err(err) => self.handle_error(row, err).await,
        }
    }

    async fn process_show(&self, row: &EnrichmentState, show_id: uuid::Uuid) -> ProcessOutcome {
        let Ok(Some(show)) = self.show_repo.find_by_id(show_id).await else {
            self.terminal_failure(row, "show no longer exists").await;
            return ProcessOutcome::Failed;
        };

        let external_ref = if row.force_refresh {
            row.matched_ref.as_deref().and_then(ExternalMediaRef::parse)
        } else {
            None
        };

        let resolved = match external_ref {
            Some(ref_id) => {
                self.provider.invalidate(&ref_id).await;
                self.provider
                    .show_enrichment(&ref_id)
                    .await
                    .map(|e| (ref_id, e, row.match_confidence.unwrap_or(1.0)))
            }
            None => {
                let query = MediaQuery {
                    title: show.title.clone(),
                    year: show.year,
                };
                match self.provider.search_shows(&query).await {
                    Ok(hits) => match matcher::best_show_match(
                        &show.title,
                        show.year,
                        &hits,
                        self.policy.min_confidence,
                    ) {
                        Some((hit, score)) => {
                            let ref_id = hit.external_ref.clone();
                            self.provider
                                .show_enrichment(&ref_id)
                                .await
                                .map(|e| (ref_id, e, score.total_score as f32))
                        }
                        None => {
                            self.log_unmatched_show(row.id, &show.title, show.year, &hits)
                                .await;
                            return self
                                .mark_unmatched(row, "no candidate cleared the match threshold")
                                .await;
                        }
                    },
                    Err(err) => return self.handle_error(row, err).await,
                }
            }
        };

        match resolved {
            Ok((ref_id, enrichment, confidence)) => {
                let genres = enrichment.genres.clone();
                if let Err(err) = self.show_repo.apply_enrichment(show_id, &enrichment).await {
                    warn!(error = %err, show_id = %show_id, "failed to persist show enrichment");
                    return self
                        .mark_unmatched(row, "failed to persist enrichment")
                        .await;
                }
                let _ = self.genre_repo.set_show_genres(show_id, &genres).await;

                // Best-effort: enrich every season the local library already
                // has files for. Never fabricates seasons/episodes that
                // scanning hasn't created.
                if let Ok(seasons) = self.show_repo.find_seasons_by_show_id(show_id).await {
                    for season in seasons {
                        if let Ok(season_enrichment) = self
                            .provider
                            .season_enrichment(&ref_id, season.season_number)
                            .await
                        {
                            let _ = self
                                .show_repo
                                .apply_season_enrichment(show_id, &season_enrichment)
                                .await;
                        }
                    }
                }

                self.mark_enriched(row, ref_id.as_str(), confidence).await
            }
            Err(err) => self.handle_error(row, err).await,
        }
    }

    async fn handle_error(&self, row: &EnrichmentState, err: EnrichmentError) -> ProcessOutcome {
        match err {
            EnrichmentError::RateLimited { retry_after } => {
                let delay = retry_after.unwrap_or(Duration::from_secs(60));
                let next_attempt_at = self.clock.now()
                    + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::seconds(60));
                let _ = self
                    .state_repo
                    .mark_retrying(
                        row.id,
                        "rate limited by provider",
                        row.attempts,
                        next_attempt_at,
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
                let attempts = row.attempts + 1;
                if attempts >= self.policy.max_attempts {
                    self.terminal_failure(row, &err.to_string()).await;
                    ProcessOutcome::Failed
                } else {
                    let delay = self.policy.backoff_for(attempts);
                    let next_attempt_at = self.clock.now()
                        + chrono::Duration::from_std(delay)
                            .unwrap_or(chrono::Duration::seconds(60));
                    let _ = self
                        .state_repo
                        .mark_retrying(row.id, &err.to_string(), attempts, next_attempt_at)
                        .await;
                    ProcessOutcome::Retrying
                }
            }
        }
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

    async fn log_unmatched_movie(
        &self,
        row_id: uuid::Uuid,
        title: &str,
        year: Option<u32>,
        hits: &[beam_domain::providers::enrichment::MovieSearchHit],
    ) {
        let top = matcher::top_movie_candidates(title, year, hits, 3);
        let details = serde_json::json!({ "row_id": row_id.to_string(), "title": title, "year": year, "candidates": top.iter().map(|(label, score)| serde_json::json!({ "candidate": label, "score": score.total_score })).collect::<Vec<_>>() });
        let _ = self
            .admin_log
            .log(
                beam_domain::models::AdminLogLevel::Warning,
                beam_domain::models::AdminLogCategory::Enrichment,
                format!("No enrichment match found for movie \"{title}\""),
                Some(details),
            )
            .await;
    }

    async fn log_unmatched_show(
        &self,
        row_id: uuid::Uuid,
        title: &str,
        year: Option<u32>,
        hits: &[beam_domain::providers::enrichment::ShowSearchHit],
    ) {
        let top = matcher::top_show_candidates(title, year, hits, 3);
        let details = serde_json::json!({ "row_id": row_id.to_string(), "title": title, "year": year, "candidates": top.iter().map(|(label, score)| serde_json::json!({ "candidate": label, "score": score.total_score })).collect::<Vec<_>>() });
        let _ = self
            .admin_log
            .log(
                beam_domain::models::AdminLogLevel::Warning,
                beam_domain::models::AdminLogCategory::Enrichment,
                format!("No enrichment match found for show \"{title}\""),
                Some(details),
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::admin_log::LocalAdminLogService;
    use crate::services::clock::TestClock;
    use beam_domain::models::{CreateMovie, CreateShow};
    use beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider;
    use beam_domain::providers::enrichment::{
        MovieEnrichment, MovieSearchHit, ShowEnrichment, ShowSearchHit,
    };
    use beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;
    use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use beam_domain::repositories::{MovieRepository, ShowRepository};

    type Harness = (
        MetadataEnrichmentService,
        Arc<InMemoryMovieRepository>,
        Arc<InMemoryShowRepository>,
        Arc<InMemoryEnrichmentStateRepository>,
        Arc<InMemoryGenreRepository>,
        Arc<TestClock>,
    );

    fn harness(provider: InMemoryEnrichmentProvider) -> Harness {
        let movie_repo = Arc::new(InMemoryMovieRepository::default());
        let show_repo = Arc::new(InMemoryShowRepository::default());
        let state_repo = Arc::new(InMemoryEnrichmentStateRepository::default());
        let genre_repo = Arc::new(InMemoryGenreRepository::default());
        let admin_log_repo = Arc::new(
            beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository::default(),
        );
        let admin_log = Arc::new(LocalAdminLogService::new(admin_log_repo));
        let clock = Arc::new(TestClock::new());

        let service = MetadataEnrichmentService::new(
            state_repo.clone(),
            movie_repo.clone(),
            show_repo.clone(),
            genre_repo.clone(),
            Arc::new(provider),
            admin_log,
            clock.clone(),
        );
        (
            service, movie_repo, show_repo, state_repo, genre_repo, clock,
        )
    }

    #[tokio::test]
    async fn no_providers_configured_skips_sweep_entirely() {
        let (service, ..) = harness(InMemoryEnrichmentProvider::new(&[]));
        let report = service.sweep_once().await;
        assert!(report.skipped_no_providers);
        assert_eq!(report.processed, 0);
    }

    #[tokio::test]
    async fn successful_movie_match_applies_enrichment_and_genres() {
        let hit = MovieSearchHit {
            external_ref: ExternalMediaRef::new("tmdb", "603"),
            title: "The Matrix".to_string(),
            original_title: None,
            year: Some(1999),
            popularity: Some(80.0),
            vote_average: Some(8.7),
        };
        let enrichment = MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            genres: vec!["Science Fiction".to_string(), "Action".to_string()],
            ..Default::default()
        };
        let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
            .with_movie_search("The Matrix", vec![hit])
            .with_movie_enrichment(enrichment);

        let (service, movie_repo, _show_repo, state_repo, genre_repo, _clock) = harness(provider);

        let movie = movie_repo
            .create(CreateMovie {
                title: "The Matrix".to_string(),
                year: Some(1999),
                runtime: None,
            })
            .await
            .unwrap();
        state_repo
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();

        let report = service.sweep_once().await;
        assert_eq!(report.enriched, 1);

        let updated = movie_repo.find_by_id(movie.id).await.unwrap().unwrap();
        assert_eq!(updated.tmdb_id, Some(603));
        assert_eq!(genre_repo.genres_for_movie(movie.id).len(), 2);
    }

    #[tokio::test]
    async fn strict_min_confidence_policy_rejects_borderline_match() {
        // Exact title, but the local movie carries no year, so the candidate
        // scores exactly the title weight (0.70): accepted under the default
        // policy, rejected once the operator raises BEAM_ENRICH_MIN_CONFIDENCE
        // above it. Same fixture, opposite outcome purely on the knob.
        let hit = MovieSearchHit {
            external_ref: ExternalMediaRef::new("tmdb", "603"),
            title: "The Matrix".to_string(),
            original_title: None,
            year: Some(1999),
            popularity: None,
            vote_average: None,
        };
        let provider =
            InMemoryEnrichmentProvider::new(&["tmdb"]).with_movie_search("The Matrix", vec![hit]);
        let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) = harness(provider);
        let service = service.with_policy(EnrichmentPolicy {
            min_confidence: 0.9,
            ..EnrichmentPolicy::default()
        });

        let movie = movie_repo
            .create(CreateMovie {
                title: "The Matrix".to_string(),
                year: None,
                runtime: None,
            })
            .await
            .unwrap();
        state_repo
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();

        let report = service.sweep_once().await;
        assert_eq!(report.unmatched, 1);
        assert_eq!(report.enriched, 0);
    }

    #[tokio::test]
    async fn no_match_marks_unmatched_and_logs_candidates() {
        let hit = MovieSearchHit {
            external_ref: ExternalMediaRef::new("tmdb", "1"),
            title: "Totally Unrelated".to_string(),
            original_title: None,
            year: Some(1950),
            popularity: None,
            vote_average: None,
        };
        let provider =
            InMemoryEnrichmentProvider::new(&["tmdb"]).with_movie_search("My Movie", vec![hit]);
        let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) = harness(provider);

        let movie = movie_repo
            .create(CreateMovie {
                title: "My Movie".to_string(),
                year: Some(2020),
                runtime: None,
            })
            .await
            .unwrap();
        state_repo
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();

        let report = service.sweep_once().await;
        assert_eq!(report.unmatched, 1);
    }

    #[tokio::test]
    async fn rate_limited_search_aborts_sweep_without_consuming_attempts() {
        let provider = InMemoryEnrichmentProvider::new(&["tmdb"]).with_search_error(|| {
            EnrichmentError::RateLimited {
                retry_after: Some(Duration::from_secs(30)),
            }
        });
        let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) = harness(provider);

        let movie = movie_repo
            .create(CreateMovie {
                title: "My Movie".to_string(),
                year: Some(2020),
                runtime: None,
            })
            .await
            .unwrap();
        state_repo
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();

        let report = service.sweep_once().await;
        assert!(report.rate_limited);
        assert_eq!(report.processed, 1);
    }

    #[tokio::test]
    async fn transient_error_retries_then_fails_after_max_attempts() {
        let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
            .with_search_error(|| EnrichmentError::Transport("boom".to_string()));
        let (service, movie_repo, _show_repo, state_repo, _genre_repo, clock) = harness(provider);

        let movie = movie_repo
            .create(CreateMovie {
                title: "My Movie".to_string(),
                year: Some(2020),
                runtime: None,
            })
            .await
            .unwrap();
        state_repo
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();

        // max_attempts defaults to 5; each sweep only picks the row up once
        // it's due, so advance the clock past each backoff between sweeps.
        let mut last_report = service.sweep_once().await;
        assert_eq!(last_report.retrying, 1);
        for _ in 1..5 {
            clock.advance(Duration::from_secs(90_000));
            last_report = service.sweep_once().await;
        }
        assert_eq!(last_report.failed, 1);

        let due = state_repo.fetch_due(clock.now(), 25).await.unwrap();
        assert!(due.is_empty(), "a Failed row must never be picked up again");
    }

    #[tokio::test]
    async fn refresh_reuses_matched_ref_and_invalidates_cache() {
        let ref_id = ExternalMediaRef::new("tmdb", "603");
        let enrichment = MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix Reloaded".to_string(),
            year: Some(2003),
            ..Default::default()
        };
        let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
            .with_movie_enrichment_at(ref_id.clone(), enrichment);
        let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) = harness(provider);

        let movie = movie_repo
            .create(CreateMovie {
                title: "The Matrix Reloaded".to_string(),
                year: Some(2003),
                runtime: None,
            })
            .await
            .unwrap();
        state_repo
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();
        state_repo
            .mark_enriched(
                state_repo.fetch_due(chrono::Utc::now(), 25).await.unwrap()[0].id,
                ref_id.as_str(),
                0.9,
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        state_repo
            .request_refresh(EnrichmentTargetId::Movie(movie.id), false)
            .await
            .unwrap();

        let report = service.sweep_once().await;
        assert_eq!(report.enriched, 1);
        let updated = movie_repo.find_by_id(movie.id).await.unwrap().unwrap();
        assert_eq!(updated.title, "The Matrix Reloaded");
    }

    #[tokio::test]
    async fn successful_show_match_applies_show_and_season_enrichment() {
        let show_ref = ExternalMediaRef::new("tmdb", "1399");
        let hit = ShowSearchHit {
            external_ref: show_ref.clone(),
            title: "Game of Thrones".to_string(),
            original_title: None,
            year: Some(2011),
            popularity: None,
            vote_average: None,
        };
        let show_enrichment = ShowEnrichment {
            tmdb_id: Some(1399),
            title: "Game of Thrones".to_string(),
            year: Some(2011),
            genres: vec!["Drama".to_string()],
            ..Default::default()
        };
        let season_enrichment = beam_domain::providers::enrichment::SeasonEnrichment {
            season_number: 1,
            poster_url: Some("https://example.com/poster.jpg".to_string()),
            ..Default::default()
        };
        let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
            .with_show_search("Game of Thrones", vec![hit])
            .with_show_enrichment(show_ref.clone(), show_enrichment)
            .with_season_enrichment(show_ref, season_enrichment);

        let (service, _movie_repo, show_repo, state_repo, genre_repo, _clock) = harness(provider);

        let show = show_repo
            .create(CreateShow {
                title: "Game of Thrones".to_string(),
                year: Some(2011),
            })
            .await
            .unwrap();
        show_repo.find_or_create_season(show.id, 1).await.unwrap();
        state_repo
            .ensure_pending(EnrichmentTargetId::Show(show.id))
            .await
            .unwrap();

        let report = service.sweep_once().await;
        assert_eq!(report.enriched, 1);

        let updated = show_repo.find_by_id(show.id).await.unwrap().unwrap();
        assert_eq!(updated.tmdb_id, Some(1399));
        assert_eq!(genre_repo.genres_for_show(show.id).len(), 1);

        let seasons = show_repo.find_seasons_by_show_id(show.id).await.unwrap();
        assert_eq!(
            seasons[0].poster_url.as_deref(),
            Some("https://example.com/poster.jpg")
        );
    }

    #[tokio::test]
    async fn missing_target_row_marks_failed() {
        let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) =
            harness(InMemoryEnrichmentProvider::new(&["tmdb"]));

        let movie = movie_repo
            .create(CreateMovie {
                title: "Ghost".to_string(),
                year: None,
                runtime: None,
            })
            .await
            .unwrap();
        state_repo
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();
        // Simulate the movie being deleted out from under the queue row.
        movie_repo.movies.lock().unwrap().remove(&movie.id);

        let report = service.sweep_once().await;
        assert_eq!(report.failed, 1);
    }

    #[test]
    fn backoff_schedule_uses_last_entry_beyond_its_length() {
        let policy = EnrichmentPolicy::default();
        assert_eq!(policy.backoff_for(1), Duration::from_secs(60));
        assert_eq!(policy.backoff_for(5), Duration::from_secs(86400));
        assert_eq!(policy.backoff_for(99), Duration::from_secs(86400));
    }

    /// The sweep's report tallies flow into
    /// `beam_enrichment_outcomes_total{outcome}` counters. Asserted through a
    /// thread-local recorder (never a global install, which would collide
    /// across parallel tests): the async sweep runs on a current-thread
    /// runtime *inside* the local-recorder scope so every sample is captured.
    #[test]
    fn sweep_outcomes_are_counted_as_metrics() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        metrics::with_local_recorder(&recorder, || {
            rt.block_on(async {
                // One row that enriches (exact title+year match) and one that
                // fails (target movie deleted out from under the queue row).
                let hit = MovieSearchHit {
                    external_ref: ExternalMediaRef::new("tmdb", "603"),
                    title: "The Matrix".to_string(),
                    original_title: None,
                    year: Some(1999),
                    popularity: Some(80.0),
                    vote_average: Some(8.7),
                };
                let enrichment = MovieEnrichment {
                    tmdb_id: Some(603),
                    title: "The Matrix".to_string(),
                    year: Some(1999),
                    ..Default::default()
                };
                let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
                    .with_movie_search("The Matrix", vec![hit])
                    .with_movie_enrichment(enrichment);
                let (service, movie_repo, _show_repo, state_repo, _genre_repo, _clock) =
                    harness(provider);

                let matched = movie_repo
                    .create(CreateMovie {
                        title: "The Matrix".to_string(),
                        year: Some(1999),
                        runtime: None,
                    })
                    .await
                    .unwrap();
                state_repo
                    .ensure_pending(EnrichmentTargetId::Movie(matched.id))
                    .await
                    .unwrap();

                let doomed = movie_repo
                    .create(CreateMovie {
                        title: "Ghost".to_string(),
                        year: None,
                        runtime: None,
                    })
                    .await
                    .unwrap();
                state_repo
                    .ensure_pending(EnrichmentTargetId::Movie(doomed.id))
                    .await
                    .unwrap();
                movie_repo.movies.lock().unwrap().remove(&doomed.id);

                let report = service.sweep_once().await;
                assert_eq!(report.enriched, 1);
                assert_eq!(report.failed, 1);
            })
        });

        let snapshot = snapshotter.snapshot().into_vec();
        let outcome_count = |outcome: &str| -> Option<u64> {
            snapshot.iter().find_map(|(key, _, _, value)| {
                let key = key.key();
                if key.name() != "beam_enrichment_outcomes_total" {
                    return None;
                }
                if !key
                    .labels()
                    .any(|l| l.key() == "outcome" && l.value() == outcome)
                {
                    return None;
                }
                match value {
                    DebugValue::Counter(v) => Some(*v),
                    _ => None,
                }
            })
        };

        assert_eq!(outcome_count("enriched"), Some(1), "snapshot: {snapshot:?}");
        assert_eq!(outcome_count("failed"), Some(1), "snapshot: {snapshot:?}");
        // Zero tallies must not mint a series at all.
        assert_eq!(outcome_count("unmatched"), None);
        assert_eq!(outcome_count("rate_limited"), None);
    }
}
