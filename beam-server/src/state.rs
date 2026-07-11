use sea_orm::DatabaseConnection;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Instant;

use beam_auth::server::OidcRuntimeConfig;
use beam_auth::utils::{
    oidc::{DiscoveredOidcClient, NotConfiguredOidcClient, OidcClient},
    pending_auth_store::{PendingAuthStore, SqlPendingAuthStore},
    repository::{SqlUserRepository, UserRepository},
    session_store::{PgSessionStore, SessionStore},
};
use beam_domain::providers::enrichment::{EnrichmentProvider, NoopEnrichmentProvider};
use beam_index::providers::cameo::{CameoEnrichmentProvider, CameoWiringConfig};
use beam_index::services::clock::RealClock;
use beam_index::services::enrichment::{EnrichmentPolicy, MetadataEnrichmentService};
use beam_index::services::index::{IndexService, LocalIndexService};

use crate::{
    config::ServerConfig,
    services::{
        admin_log::{AdminLogService, LocalAdminLogService},
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
    /// Surfaced as `uptime_secs` by the health endpoint and reused by a later
    /// admin status endpoint.
    pub start_instant: Instant,
}

impl AppState {
    pub fn new(
        config: ServerConfig,
        services: AppServices,
        probe: Arc<dyn DependencyProbe>,
    ) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                config,
                services,
                probe,
                start_instant: Instant::now(),
            }),
        }
    }

    /// Whole seconds elapsed since the process built its state.
    pub fn uptime_secs(&self) -> u64 {
        self.inner.start_instant.elapsed().as_secs()
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
        db: DatabaseConnection,
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
            admin_emails_csv: config.admin_emails.clone().unwrap_or_default(),
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

        // Real cameo client when at least TMDB or AniList is configured;
        // NoopEnrichmentProvider (every sweep a fast no-op) otherwise -- e.g.
        // a fresh dev environment with no TMDB_API_TOKEN set. A build failure
        // is only tolerated (warn + disable enrichment) when the enrichment
        // knobs were left implicit; an explicitly-set BEAM_METADATA_LANGUAGE
        // that fails the build (cameo validates the BCP-47 tag at
        // construction) fails startup instead -- an explicit knob must never
        // be silently ignored on a headless server, matching the fail-fast
        // precedent of `validate_values` and the cookie-Secure verdict.
        let enrichment_provider: Arc<dyn EnrichmentProvider> =
            match beam_index::providers::cameo::build_client(CameoWiringConfig {
                tmdb_api_token: config.tmdb_api_token.clone(),
                anilist_enabled: config.anilist_enabled,
                metadata_language: config.metadata_language.clone(),
            }) {
                Ok(Some(client)) => Arc::new(CameoEnrichmentProvider::new(client)),
                Ok(None) => Arc::new(NoopEnrichmentProvider),
                Err(e) if config.metadata_language.is_some() => {
                    return Err(eyre::eyre!(
                        "failed to build cameo client with BEAM_METADATA_LANGUAGE={:?}: {e}",
                        config.metadata_language.as_deref().unwrap_or_default(),
                    ));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to build cameo client; metadata enrichment disabled");
                    Arc::new(NoopEnrichmentProvider)
                }
            };

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

        let services = Self {
            hash: hash_service.clone() as Arc<dyn HashService>,
            library: Arc::new(LocalLibraryService::new(
                library_repo,
                file_repo.clone(),
                config.video_dir.clone(),
                notification_service.clone(),
                index_service.clone() as Arc<dyn IndexService>,
                Arc::new(OsPathValidator),
            )),
            metadata: Arc::new(
                DbMetadataService::new(movie_repo, show_repo, file_repo, stream_repo)
                    .with_enrichment_repo(enrichment_repo),
            ),
            notification: notification_service,
            admin_log: admin_log_service,
            user_repo,
            playback: playback_service,
            genre_repo,
            session_store,
            oidc_client,
            pending_auth_store,
            oidc_config,
        };

        Ok((services, index_service, enrichment_service))
    }
}
