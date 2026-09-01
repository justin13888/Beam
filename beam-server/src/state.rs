use sea_orm::DatabaseConnection;
use std::ops::Deref;
use std::sync::Arc;
use std::time::{Duration, Instant};

use beam_auth::utils::oidc_config::OidcRuntimeConfig;
use beam_auth::utils::{
    oidc::{DiscoveredOidcClient, NotConfiguredOidcClient, OidcClient},
    pending_auth_store::{PendingAuthStore, SqlPendingAuthStore},
    repository::{SqlUserRepository, UserRepository},
    session_store::{PgSessionStore, SessionStore},
};
use beam_domain::providers::artwork::ArtworkFetcher;
use beam_domain::providers::enrichment::{EnrichmentProvider, NoopEnrichmentProvider};
use beam_domain::services::{Clock, RealClock};
use beam_index::providers::artwork::{ArtworkFetchLimits, ReqwestArtworkFetcher};
use beam_index::providers::cameo::{CameoEnrichmentProvider, CameoWiringConfig};
use beam_index::services::enrichment::{EnrichmentPolicy, MetadataEnrichmentService};
use beam_index::services::index::{IndexService, LocalIndexService};
use metrics_exporter_prometheus::PrometheusHandle;

use crate::{
    config::ServerConfig,
    services::{
        admin_log::{AdminLogService, LocalAdminLogService},
        artwork::{ArtworkCache, ArtworkCacheConfig},
        hash::{HashConfig, HashService, LocalHashService},
        health::DependencyProbe,
        library::{LibraryService, LocalLibraryService, OsPathValidator},
        metadata::{DbMetadataService, MetadataService},
        notification::{LocalNotificationService, NotificationService},
        playback::{DbPlaybackService, PlaybackService},
    },
};

#[derive(Clone, Debug)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

#[derive(Debug)]
pub struct AppStateInner {
    pub config: ServerConfig,
    pub services: AppServices,
    /// Deep-health dependency probe backing `GET /v1/health`.
    pub probe: Arc<dyn DependencyProbe>,
    /// Monotonic process-start instant, captured when the state is built.
    /// Surfaced as `uptime_secs` by the health endpoint and by the admin
    /// status endpoint.
    pub start_instant: Instant,
    /// The clock `uptime_secs` measures against. Injected so a test can move
    /// time: with `Instant::now()` read directly, uptime is always ~0 and no
    /// assertion about it can fail.
    clock: Arc<dyn Clock>,
    /// The Prometheus handle `GET /metrics` renders, present only when
    /// `BEAM_ENABLE_METRICS=true` installed a recorder at startup. Carried on
    /// the state rather than passed to `create_router` so the router's shape --
    /// and therefore the exported description -- does not depend on
    /// configuration.
    metrics: Option<PrometheusHandle>,
}

impl AppState {
    pub fn new(
        config: ServerConfig,
        services: AppServices,
        probe: Arc<dyn DependencyProbe>,
        metrics: Option<PrometheusHandle>,
    ) -> Self {
        Self::with_clock(config, services, probe, Arc::new(RealClock), metrics)
    }

    pub fn with_clock(
        config: ServerConfig,
        services: AppServices,
        probe: Arc<dyn DependencyProbe>,
        clock: Arc<dyn Clock>,
        metrics: Option<PrometheusHandle>,
    ) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                config,
                services,
                probe,
                start_instant: clock.monotonic(),
                clock,
                metrics,
            }),
        }
    }

    /// The clock every time-dependent policy reads.
    ///
    /// Exposed because the rate limiter is now a Kynos `RateLimitPolicy`, which
    /// receives `&AppState` at check time rather than owning a clock handed to
    /// its constructor. The seam is unchanged; only where it is read from moved.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.inner.clock
    }

    /// The Prometheus handle, or `None` when no recorder was installed.
    pub fn metrics(&self) -> Option<&PrometheusHandle> {
        self.inner.metrics.as_ref()
    }

    /// Whole seconds elapsed since the process built its state.
    pub fn uptime_secs(&self) -> u64 {
        self.inner
            .clock
            .monotonic()
            .saturating_duration_since(self.inner.start_instant)
            .as_secs()
    }
}

impl Deref for AppState {
    type Target = AppStateInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Debug)]
pub struct AppServices {
    pub hash: Arc<dyn HashService>,
    pub library: Arc<dyn LibraryService>,
    pub metadata: Arc<dyn MetadataService>,
    pub notification: Arc<dyn NotificationService>,
    pub admin_log: Arc<dyn AdminLogService>,
    pub user_repo: Arc<dyn UserRepository>,
    pub playback: Arc<dyn PlaybackService>,
    /// Distinct genre catalog, backing `GET /v1/genres`. Shared with the
    /// enrichment service, which populates it as titles are enriched.
    pub genre_repo: Arc<dyn beam_domain::repositories::GenreRepository>,
    /// Raw repositories the admin status endpoint (`GET /v1/admin/status`)
    /// reads its counts from directly, following the `genre_repo` precedent
    /// (issue #85). Shared with the library/index/enrichment services.
    pub library_repo: Arc<dyn beam_domain::repositories::LibraryRepository>,
    pub file_repo: Arc<dyn beam_domain::repositories::FileRepository>,
    pub enrichment_repo: Arc<dyn beam_domain::repositories::EnrichmentStateRepository>,
    /// Read directly by the artwork endpoint, which resolves a title id to the
    /// provider URL enrichment stored on it. Same precedent as the repositories
    /// above; shared with the metadata and playback services.
    pub movie_repo: Arc<dyn beam_domain::repositories::MovieRepository>,
    pub show_repo: Arc<dyn beam_domain::repositories::ShowRepository>,
    /// Poster and backdrop art, served by Beam rather than by a provider CDN
    /// (ADR-0015). Concrete rather than a trait object: its two boundaries --
    /// the network and the database -- already have seams, and the repository
    /// tests filesystem code against a real `TempDir` rather than a fake.
    pub artwork: Arc<ArtworkCache>,
    /// Backs the `beam_session` cookie -- the only credential the server
    /// issues (see ADR-0003/ADR-0005).
    pub session_store: Arc<dyn SessionStore>,
    pub oidc_client: Arc<dyn OidcClient>,
    pub pending_auth_store: Arc<dyn PendingAuthStore>,
    pub oidc_config: OidcRuntimeConfig,
}

impl AppServices {
    /// Build the application's services. Also returns the concrete
    /// [`LocalIndexService`] and [`MetadataEnrichmentService`] (rather than
    /// folding them into `AppServices` itself) so the process entry point can
    /// spawn beam-index's background scan/watch/enrichment tasks via
    /// `beam_index::runtime` -- those need methods beyond the narrow
    /// `IndexService`/`MetadataService` trait objects stored on `library`/
    /// `metadata`. Test fixtures that only need an `AppServices` (not a real
    /// indexer) are unaffected by these extra return values.
    pub async fn new(
        config: &ServerConfig,
        db: Arc<DatabaseConnection>,
    ) -> eyre::Result<(Self, Arc<LocalIndexService>, Arc<MetadataEnrichmentService>)> {
        let hash_config = HashConfig::default();

        // Create repository implementations
        let library_repo = Arc::new(beam_index::repositories::SqlLibraryRepository::new(
            db.clone(),
        ));
        let file_repo: Arc<dyn beam_domain::repositories::FileRepository> =
            Arc::new(beam_index::repositories::SqlFileRepository::new(db.clone()));
        let movie_repo: Arc<dyn beam_domain::repositories::MovieRepository> = Arc::new(
            beam_index::repositories::SqlMovieRepository::new(db.clone()),
        );
        let show_repo: Arc<dyn beam_domain::repositories::ShowRepository> =
            Arc::new(beam_index::repositories::SqlShowRepository::new(db.clone()));
        let stream_repo: Arc<dyn beam_domain::repositories::MediaStreamRepository> = Arc::new(
            beam_index::repositories::SqlMediaStreamRepository::new(db.clone()),
        );
        let user_repo: Arc<dyn UserRepository> = Arc::new(SqlUserRepository::new(db.clone()));
        let admin_log_repo = Arc::new(beam_index::repositories::SqlAdminLogRepository::new(
            db.clone(),
        ));
        let enrichment_repo: Arc<dyn beam_domain::repositories::EnrichmentStateRepository> =
            Arc::new(beam_index::repositories::SqlEnrichmentStateRepository::new(
                db.clone(),
            ));
        let genre_repo: Arc<dyn beam_domain::repositories::GenreRepository> = Arc::new(
            beam_index::repositories::SqlGenreRepository::new(db.clone()),
        );
        let playback_repo: Arc<dyn beam_domain::repositories::PlaybackProgressRepository> =
            Arc::new(beam_index::repositories::SqlPlaybackProgressRepository::new(db.clone()));

        let notification_service = Arc::new(LocalNotificationService::new());
        let hash_service = Arc::new(LocalHashService::new(hash_config));
        let media_info_service =
            Arc::new(crate::services::media_info::LocalMediaInfoService::default());

        let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));

        let pending_auth_store: Arc<dyn PendingAuthStore> =
            Arc::new(SqlPendingAuthStore::new(db.clone()));

        // Real discovered client when issuer/client_id/client_secret are all
        // configured and discovery succeeds; a clear "not configured" error
        // otherwise (login is simply unavailable, never a panic).
        let oidc_client: Arc<dyn OidcClient> = if config.oidc_configured() {
            match DiscoveredOidcClient::discover(
                config.oidc_issuer.as_deref().unwrap_or_default(),
                config.oidc_client_id.as_deref().unwrap_or_default(),
                config.oidc_client_secret.as_deref().unwrap_or_default(),
                &config.oidc_redirect_url(),
                config
                    .oidc_scopes
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
            )
            .await
            {
                Ok(client) => Arc::new(client),
                Err(e) => {
                    tracing::warn!(error = %e, "OIDC discovery failed; login disabled until fixed");
                    Arc::new(NotConfiguredOidcClient::new(format!(
                        "OIDC discovery failed: {e}"
                    )))
                }
            }
        } else {
            Arc::new(NotConfiguredOidcClient::new(
                "OIDC not configured (set BEAM_OIDC_ISSUER, BEAM_OIDC_CLIENT_ID, BEAM_OIDC_CLIENT_SECRET)",
            ))
        };

        let oidc_config = OidcRuntimeConfig {
            web_url: config.web_url.clone(),
            cookie_secure: config.resolved_cookie_secure(),
            admin_claim: config.oidc_admin_claim.clone(),
            admin_value: config.oidc_admin_value.clone(),
            session_idle_days: config.session_idle_days,
            session_max_days: config.session_max_days,
        };

        let admin_log_service: Arc<dyn AdminLogService> =
            Arc::new(LocalAdminLogService::new(admin_log_repo.clone()));

        let index_service = Arc::new(
            LocalIndexService::new(
                library_repo.clone(),
                file_repo.clone(),
                movie_repo.clone(),
                show_repo.clone(),
                stream_repo.clone(),
                hash_service.clone(),
                media_info_service.clone(),
                notification_service.clone(),
                admin_log_service.clone(),
            )
            .with_hash_unknown_files(config.hash_unknown_files)
            .with_enrichment_repo(enrichment_repo.clone()),
        );

        let enrichment_provider = build_enrichment_provider(config)?;

        let enrichment_service = Arc::new(
            MetadataEnrichmentService::new(
                enrichment_repo.clone(),
                movie_repo.clone(),
                show_repo.clone(),
                genre_repo.clone(),
                enrichment_provider,
                admin_log_service.clone(),
                Arc::new(RealClock),
            )
            .with_policy(EnrichmentPolicy {
                batch_size: config.enrich_batch_size,
                min_confidence: config.enrich_min_confidence,
                ..EnrichmentPolicy::default()
            }),
        );

        let playback_service = Arc::new(DbPlaybackService::new(
            playback_repo,
            file_repo.clone(),
            movie_repo.clone(),
            show_repo.clone(),
        ));

        // Artwork is served by Beam, not by a provider CDN (ADR-0015). The
        // cache lives beside the rest of the server's state and is restored
        // from disk here, so a restart after a deploy does not re-fetch a
        // whole library's art. A directory that cannot be read is fatal: the
        // alternative is a server that silently re-fetches every poster on
        // every request.
        let artwork_fetcher: Arc<dyn ArtworkFetcher> =
            Arc::new(ReqwestArtworkFetcher::new(ArtworkFetchLimits {
                timeout: Duration::from_secs(config.artwork_fetch_timeout_secs),
                max_bytes: config.artwork_max_image_bytes,
            })?);
        let artwork = Arc::new(
            ArtworkCache::open(
                ArtworkCacheConfig {
                    root: config.data_dir.join("artwork"),
                    max_bytes: config.artwork_cache_max_bytes,
                    negative_ttl: Duration::from_secs(config.artwork_negative_ttl_secs),
                },
                artwork_fetcher,
                Arc::new(RealClock),
            )
            .await?,
        );

        let services = Self {
            hash: hash_service.clone() as Arc<dyn HashService>,
            library: Arc::new(LocalLibraryService::new(
                library_repo.clone(),
                file_repo.clone(),
                config.video_dir.clone(),
                notification_service.clone(),
                index_service.clone() as Arc<dyn IndexService>,
                Arc::new(OsPathValidator),
            )),
            metadata: Arc::new(
                DbMetadataService::new(
                    movie_repo.clone(),
                    show_repo.clone(),
                    file_repo.clone(),
                    stream_repo,
                )
                .with_enrichment_repo(enrichment_repo.clone()),
            ),
            notification: notification_service,
            admin_log: admin_log_service,
            user_repo,
            playback: playback_service,
            genre_repo,
            library_repo,
            file_repo,
            enrichment_repo,
            movie_repo,
            show_repo,
            artwork,
            session_store,
            oidc_client,
            pending_auth_store,
            oidc_config,
        };

        Ok((services, index_service, enrichment_service))
    }
}

/// Decide which [`EnrichmentProvider`] the configuration asks for.
///
/// Extracted from [`AppServices::new`] so the decision can be tested: inside
/// the constructor it sat behind a live database connection, so the whole
/// tree -- including the deliberate asymmetry between an implicit and an
/// explicit `BEAM_METADATA_LANGUAGE` -- was unreachable from any test.
///
/// A real cameo client is built when at least TMDB or AniList is configured;
/// [`NoopEnrichmentProvider`] (every sweep a fast no-op) otherwise -- e.g. a
/// fresh dev environment with no TMDB token set. A build failure is only
/// tolerated (warn + disable enrichment) when the enrichment knobs were left
/// implicit; an explicitly-set `BEAM_METADATA_LANGUAGE` that fails the build
/// (cameo validates the BCP-47 tag at construction) fails startup instead --
/// an explicit knob must never be silently ignored on a headless server,
/// matching the fail-fast precedent of `validate_values` and the
/// cookie-Secure verdict.
pub(crate) fn build_enrichment_provider(
    config: &ServerConfig,
) -> eyre::Result<Arc<dyn EnrichmentProvider>> {
    let built = beam_index::providers::cameo::build_client(CameoWiringConfig {
        tmdb_api_token: config.tmdb_api_token.clone(),
        anilist_enabled: config.anilist_enabled,
        metadata_language: config.metadata_language.clone(),
    });
    provider_from_build(built, config.metadata_language.as_deref())
}

/// The decision [`build_enrichment_provider`] makes about a build outcome,
/// separated from making the client.
///
/// Split out because the interesting branch -- a build failure with the
/// language knob left *implicit* -- cannot be produced through
/// `build_client` today: the language tag is the only input it validates. As
/// one function the two cases were indistinguishable to any test, so nothing
/// could tell the fail-fast path from the warn-and-continue one.
pub(crate) fn provider_from_build(
    built: Result<Option<cameo::CameoClient>, cameo::CameoClientError>,
    metadata_language: Option<&str>,
) -> eyre::Result<Arc<dyn EnrichmentProvider>> {
    match built {
        Ok(Some(client)) => Ok(Arc::new(CameoEnrichmentProvider::new(client))),
        Ok(None) => Ok(Arc::new(NoopEnrichmentProvider)),
        Err(e) if metadata_language.is_some() => Err(eyre::eyre!(
            "failed to build cameo client with BEAM_METADATA_LANGUAGE={:?}: {e}",
            metadata_language.unwrap_or_default(),
        )),
        Err(e) => {
            tracing::warn!(error = %e, "failed to build cameo client; metadata enrichment disabled");
            Ok(Arc::new(NoopEnrichmentProvider))
        }
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;
