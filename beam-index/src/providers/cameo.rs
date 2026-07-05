//! `cameo`-backed [`EnrichmentProvider`] adapter. This is the only module in
//! the workspace that depends on the `cameo` crate directly -- every cameo
//! type is mapped to beam-domain's provider-agnostic DTOs here and nowhere
//! else, mirroring how ffmpeg is confined to this crate's `probe` module.

use tracing::warn;

use beam_domain::providers::enrichment::{
    EnrichmentError, EnrichmentProvider, ExternalMediaRef, MediaQuery, MovieEnrichment,
    MovieSearchHit, SeasonEnrichment, ShowEnrichment, ShowSearchHit,
};

/// Configuration for constructing the production `cameo` client. Absent TMDB
/// token / disabled AniList are valid states -- `build_client` returns `None`
/// when neither is configured, and the caller falls back to
/// `NoopEnrichmentProvider`.
///
/// Note: cameo's `cache` feature (a bundled-SQLite response cache) is
/// deliberately not enabled. Its `rusqlite` dependency hard-pins
/// `libsqlite3-sys` to a version range that conflicts with the one
/// `sea-orm-migration`'s CLI tooling pulls in transitively (via
/// `sqlx-sqlite`, always compiled in regardless of our postgres-only
/// `sea-orm` feature selection) -- two crates cannot link the same native
/// `sqlite3` library at two different versions. Every request hits the
/// provider directly; the worker's own backoff/retry (`EnrichmentPolicy`)
/// plus cameo's built-in per-provider rate limiting cover the resulting
/// extra request volume. See
/// docs/architecture/decisions/ADR-0006-cameo-enrichment.md.
#[derive(Debug, Clone)]
pub struct CameoWiringConfig {
    pub tmdb_api_token: Option<String>,
    pub anilist_enabled: bool,
}

/// Build a `cameo` client from config, or `None` if no provider is
/// configured (absent TMDB token and AniList disabled).
pub fn build_client(
    config: CameoWiringConfig,
) -> Result<Option<cameo::CameoClient>, cameo::CameoClientError> {
    let mut builder = cameo::CameoClient::builder();
    let mut any = false;

    if let Some(token) = config.tmdb_api_token.filter(|t| !t.is_empty()) {
        builder = builder.with_tmdb(cameo::TmdbConfig::new(token));
        any = true;
    }
    if config.anilist_enabled {
        builder = builder.with_anilist(cameo::AniListConfig::new());
        any = true;
    }
    if !any {
        return Ok(None);
    }

    builder = builder.with_priority(["tmdb", "anilist"]);
    Ok(Some(builder.build()?))
}

/// [`EnrichmentProvider`] backed by a configured `cameo::CameoClient`.
#[derive(Debug)]
pub struct CameoEnrichmentProvider {
    client: cameo::CameoClient,
}

impl CameoEnrichmentProvider {
    pub fn new(client: cameo::CameoClient) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl EnrichmentProvider for CameoEnrichmentProvider {
    fn available_providers(&self) -> Vec<String> {
        self.client
            .provider_ids()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    async fn search_movies(
        &self,
        query: &MediaQuery,
    ) -> Result<Vec<MovieSearchHit>, EnrichmentError> {
        let mut hits = Vec::new();
        let mut last_err = None;
        for provider in self.client.provider_ids() {
            match self
                .client
                .search_movies_with(provider, &query.title, None)
                .await
            {
                Ok(page) => hits.extend(page.results.iter().map(map_movie_hit)),
                Err(cameo::CameoClientError::Provider(cameo::ProviderError::RateLimited {
                    retry_after,
                })) => {
                    return Err(EnrichmentError::RateLimited { retry_after });
                }
                Err(e) => {
                    warn!(provider = %provider, error = %e, "cameo movie search failed for provider; trying next");
                    last_err = Some(e);
                }
            }
        }
        if hits.is_empty()
            && let Some(err) = last_err
        {
            return Err(map_client_error(err));
        }
        Ok(hits)
    }

    async fn search_shows(
        &self,
        query: &MediaQuery,
    ) -> Result<Vec<ShowSearchHit>, EnrichmentError> {
        let mut hits = Vec::new();
        let mut last_err = None;
        for provider in self.client.provider_ids() {
            match self
                .client
                .search_tv_shows_with(provider, &query.title, None)
                .await
            {
                Ok(page) => hits.extend(page.results.iter().map(map_show_hit)),
                Err(cameo::CameoClientError::Provider(cameo::ProviderError::RateLimited {
                    retry_after,
                })) => {
                    return Err(EnrichmentError::RateLimited { retry_after });
                }
                Err(e) => {
                    warn!(provider = %provider, error = %e, "cameo show search failed for provider; trying next");
                    last_err = Some(e);
                }
            }
        }
        if hits.is_empty()
            && let Some(err) = last_err
        {
            return Err(map_client_error(err));
        }
        Ok(hits)
    }

    async fn movie_enrichment(
        &self,
        id: &ExternalMediaRef,
    ) -> Result<MovieEnrichment, EnrichmentError> {
        let media_id = to_media_id(id)?;
        let details = self
            .client
            .movie_details(&media_id)
            .await
            .map_err(map_client_error)?;
        Ok(map_movie_enrichment(&details))
    }

    async fn show_enrichment(
        &self,
        id: &ExternalMediaRef,
    ) -> Result<ShowEnrichment, EnrichmentError> {
        let media_id = to_media_id(id)?;
        let details = self
            .client
            .tv_show_details(&media_id)
            .await
            .map_err(map_client_error)?;
        Ok(map_show_enrichment(&details))
    }

    async fn season_enrichment(
        &self,
        show: &ExternalMediaRef,
        season_number: u32,
    ) -> Result<SeasonEnrichment, EnrichmentError> {
        let media_id = to_media_id(show)?;
        let details = self
            .client
            .season_details(&media_id, season_number)
            .await
            .map_err(map_client_error)?;
        Ok(map_season_enrichment(&details))
    }

    async fn invalidate(&self, _id: &ExternalMediaRef) {
        // cameo v0.2.0's cache backend only exposes `purge_expired` (no
        // per-key delete), so an explicit refresh relies on the cache TTL
        // expiring rather than a targeted invalidation.
    }
}

fn to_media_id(id: &ExternalMediaRef) -> Result<cameo::MediaId, EnrichmentError> {
    cameo::MediaId::parse(id.as_str())
        .map_err(|e| EnrichmentError::Provider(format!("invalid external ref {id}: {e}")))
}

fn map_client_error(err: cameo::CameoClientError) -> EnrichmentError {
    match err {
        cameo::CameoClientError::NotConfigured => EnrichmentError::NotConfigured,
        cameo::CameoClientError::Unsupported => {
            EnrichmentError::Provider("no configured provider supports this operation".to_string())
        }
        cameo::CameoClientError::Provider(provider_err) => map_provider_error(provider_err),
        other => EnrichmentError::Provider(other.to_string()),
    }
}

fn map_provider_error(err: cameo::ProviderError) -> EnrichmentError {
    match err {
        cameo::ProviderError::NotFound => EnrichmentError::NotFound,
        cameo::ProviderError::Unsupported => {
            EnrichmentError::Provider("operation not supported by this provider".to_string())
        }
        cameo::ProviderError::RateLimited { retry_after } => {
            EnrichmentError::RateLimited { retry_after }
        }
        cameo::ProviderError::Auth(msg) => EnrichmentError::Provider(format!("auth error: {msg}")),
        cameo::ProviderError::Api { status, message } => {
            EnrichmentError::Provider(format!("api error (HTTP {status}): {message}"))
        }
        cameo::ProviderError::Transport(msg) => EnrichmentError::Transport(msg),
        cameo::ProviderError::Deserialization(msg) => {
            EnrichmentError::Provider(format!("deserialization error: {msg}"))
        }
        cameo::ProviderError::InvalidInput(msg) => {
            EnrichmentError::Provider(format!("invalid input: {msg}"))
        }
        cameo::ProviderError::Other(msg) => EnrichmentError::Provider(msg),
        other => EnrichmentError::Provider(other.to_string()),
    }
}

fn external_ref_for(provider_id: &cameo::MediaId) -> ExternalMediaRef {
    ExternalMediaRef::new(provider_id.provider(), &provider_id.native().to_string())
}

fn year_of(date: Option<&cameo::PartialDate>) -> Option<u32> {
    date.and_then(|d| u32::try_from(d.year()).ok())
}

fn map_movie_hit(m: &cameo::UnifiedMovie) -> MovieSearchHit {
    MovieSearchHit {
        external_ref: external_ref_for(&m.provider_id),
        title: m.title.clone(),
        original_title: m.original_title.clone(),
        year: year_of(m.release_date.as_ref()),
        popularity: m.popularity,
        vote_average: m.vote_average,
    }
}

fn map_show_hit(s: &cameo::UnifiedTvShow) -> ShowSearchHit {
    ShowSearchHit {
        external_ref: external_ref_for(&s.provider_id),
        title: s.name.clone(),
        original_title: s.original_name.clone(),
        year: year_of(s.first_air_date.as_ref()),
        popularity: s.popularity,
        vote_average: s.vote_average,
    }
}

fn map_movie_enrichment(details: &cameo::UnifiedMovieDetails) -> MovieEnrichment {
    let movie = &details.movie;
    let is_tmdb = movie.provider_id.provider() == "tmdb";
    let is_anilist = movie.provider_id.provider() == "anilist";
    let native_id = movie.provider_id.as_u64().map(|id| id as u32);

    MovieEnrichment {
        tmdb_id: is_tmdb.then_some(native_id).flatten(),
        anilist_id: is_anilist.then_some(native_id).flatten(),
        imdb_id: details.imdb_id.clone(),
        title: movie.title.clone(),
        original_title: movie.original_title.clone(),
        description: movie.overview.clone(),
        year: year_of(movie.release_date.as_ref()),
        release_date: movie.release_date.as_ref().and_then(|d| d.to_naive_date()),
        runtime_mins: details.runtime,
        poster_url: movie.poster_url.clone(),
        backdrop_url: movie.backdrop_url.clone(),
        rating: movie.vote_average.map(|v| v as f32),
        genres: movie.genres.iter().map(|g| g.name().to_string()).collect(),
    }
}

fn map_show_enrichment(details: &cameo::UnifiedTvShowDetails) -> ShowEnrichment {
    let show = &details.show;
    let is_tmdb = show.provider_id.provider() == "tmdb";
    let is_anilist = show.provider_id.provider() == "anilist";
    let native_id = show.provider_id.as_u64().map(|id| id as u32);

    ShowEnrichment {
        tmdb_id: is_tmdb.then_some(native_id).flatten(),
        anilist_id: is_anilist.then_some(native_id).flatten(),
        // cameo's UnifiedTvShowDetails doesn't surface an external IMDB id at
        // this level (TMDB's TV endpoints don't map one to the unified model).
        imdb_id: None,
        title: show.name.clone(),
        original_title: show.original_name.clone(),
        description: show.overview.clone(),
        year: year_of(show.first_air_date.as_ref()),
        poster_url: show.poster_url.clone(),
        backdrop_url: show.backdrop_url.clone(),
        genres: show.genres.iter().map(|g| g.name().to_string()).collect(),
    }
}

fn map_season_enrichment(details: &cameo::UnifiedSeasonDetails) -> SeasonEnrichment {
    SeasonEnrichment {
        season_number: details.season_number,
        poster_url: details.poster_url.clone(),
        air_date: details.air_date.as_ref().and_then(|d| d.to_naive_date()),
        episodes: details
            .episodes
            .iter()
            .map(|ep| beam_domain::providers::enrichment::EpisodeEnrichment {
                episode_number: ep.episode_number,
                title: ep.name.clone(),
                description: ep.overview.clone(),
                air_date: ep.air_date.as_ref().and_then(|d| d.to_naive_date()),
                runtime_mins: ep.runtime,
                thumbnail_url: ep.still_url.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cameo::{
        MediaId, PartialDate, UnifiedEpisode, UnifiedMovie, UnifiedMovieDetails,
        UnifiedSeasonDetails, UnifiedTvShow, UnifiedTvShowDetails,
    };

    #[test]
    fn maps_movie_hit_fields() {
        let movie = UnifiedMovie::new(MediaId::tmdb(603), "The Matrix")
            .with_original_title("The Matrix")
            .with_release_date(PartialDate::from_year(1999))
            .with_popularity(80.0)
            .with_vote_average(8.7);
        let hit = map_movie_hit(&movie);
        assert_eq!(hit.external_ref.as_str(), "tmdb:603");
        assert_eq!(hit.title, "The Matrix");
        assert_eq!(hit.year, Some(1999));
        assert_eq!(hit.popularity, Some(80.0));
        assert_eq!(hit.vote_average, Some(8.7));
    }

    #[test]
    fn maps_show_hit_fields() {
        let show = UnifiedTvShow::new(MediaId::anilist(5114), "Arcane")
            .with_first_air_date(PartialDate::from_year(2021));
        let hit = map_show_hit(&show);
        assert_eq!(hit.external_ref.as_str(), "anilist:5114");
        assert_eq!(hit.title, "Arcane");
        assert_eq!(hit.year, Some(2021));
    }

    #[test]
    fn maps_movie_enrichment_tmdb_id_and_fields() {
        let movie = UnifiedMovie::new(MediaId::tmdb(603), "The Matrix")
            .with_overview("A hacker discovers reality is a simulation.")
            .with_release_date(PartialDate::ymd(1999, 3, 31).unwrap())
            .with_poster_url("https://image.tmdb.org/poster.jpg")
            .with_vote_average(8.7);
        let details = UnifiedMovieDetails::new(movie)
            .with_runtime(136)
            .with_imdb_id("tt0133093");

        let enrichment = map_movie_enrichment(&details);
        assert_eq!(enrichment.tmdb_id, Some(603));
        assert_eq!(enrichment.anilist_id, None);
        assert_eq!(enrichment.imdb_id.as_deref(), Some("tt0133093"));
        assert_eq!(enrichment.year, Some(1999));
        assert!(enrichment.release_date.is_some());
        assert_eq!(enrichment.runtime_mins, Some(136));
        assert_eq!(enrichment.rating, Some(8.7_f32));
    }

    #[test]
    fn maps_show_enrichment_anilist_id() {
        let show = UnifiedTvShow::new(MediaId::anilist(5114), "Cowboy Bebop")
            .with_first_air_date(PartialDate::from_year(1998));
        let details = UnifiedTvShowDetails::new(show);

        let enrichment = map_show_enrichment(&details);
        assert_eq!(enrichment.anilist_id, Some(5114));
        assert_eq!(enrichment.tmdb_id, None);
        assert_eq!(enrichment.imdb_id, None);
        assert_eq!(enrichment.title, "Cowboy Bebop");
    }

    #[test]
    fn maps_season_and_episode_enrichment() {
        let episode = UnifiedEpisode::new(1)
            .with_name("Pilot")
            .with_overview("The beginning.")
            .with_runtime(45)
            .with_still_url("https://example.com/still.jpg");
        let details = UnifiedSeasonDetails::new(MediaId::tmdb(1399), 1)
            .with_poster_url("https://example.com/poster.jpg")
            .with_episodes(vec![episode]);

        let enrichment = map_season_enrichment(&details);
        assert_eq!(enrichment.season_number, 1);
        assert_eq!(
            enrichment.poster_url.as_deref(),
            Some("https://example.com/poster.jpg")
        );
        assert_eq!(enrichment.episodes.len(), 1);
        assert_eq!(enrichment.episodes[0].title.as_deref(), Some("Pilot"));
        assert_eq!(enrichment.episodes[0].runtime_mins, Some(45));
    }

    #[test]
    fn to_media_id_roundtrips_external_ref() {
        let external_ref = ExternalMediaRef::new("tmdb", "603");
        let media_id = to_media_id(&external_ref).unwrap();
        assert_eq!(media_id.provider(), "tmdb");
        assert_eq!(media_id.as_u64(), Some(603));
    }

    #[test]
    fn to_media_id_rejects_missing_separator() {
        let external_ref = ExternalMediaRef::parse("not-a-valid-ref");
        assert!(external_ref.is_none());
    }

    #[test]
    fn map_provider_error_rate_limited_preserves_retry_after() {
        let err = cameo::ProviderError::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(5)),
        };
        match map_provider_error(err) {
            EnrichmentError::RateLimited { retry_after } => {
                assert_eq!(retry_after, Some(std::time::Duration::from_secs(5)));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn map_provider_error_not_found() {
        assert!(matches!(
            map_provider_error(cameo::ProviderError::NotFound),
            EnrichmentError::NotFound
        ));
    }

    #[test]
    fn map_provider_error_transport() {
        match map_provider_error(cameo::ProviderError::Transport(
            "connection reset".to_string(),
        )) {
            EnrichmentError::Transport(msg) => assert_eq!(msg, "connection reset"),
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[test]
    fn map_client_error_not_configured() {
        assert!(matches!(
            map_client_error(cameo::CameoClientError::NotConfigured),
            EnrichmentError::NotConfigured
        ));
    }

    #[test]
    fn build_client_returns_none_when_nothing_configured() {
        let config = CameoWiringConfig {
            tmdb_api_token: None,
            anilist_enabled: false,
        };
        let client = build_client(config).unwrap();
        assert!(client.is_none());
    }

    #[test]
    fn build_client_returns_some_when_anilist_enabled() {
        let config = CameoWiringConfig {
            tmdb_api_token: None,
            anilist_enabled: true,
        };
        let client = build_client(config).unwrap();
        assert!(client.is_some());
    }
}
