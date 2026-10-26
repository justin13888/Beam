//! `cameo`-backed [`EnrichmentProvider`] adapter. This is the only module in
//! the workspace that depends on the `cameo` crate directly -- every cameo
//! type is mapped to beam-domain's provider-agnostic DTOs here and nowhere
//! else, mirroring how ffmpeg is confined to this crate's `probe` module.

use tracing::warn;

use beam_domain::providers::enrichment::{
    CollectionEnrichment, CreditEnrichment, CreditRole, EnrichmentError, EnrichmentProvider,
    ExternalMediaRef, MediaQuery, MovieEnrichment, MovieSearchHit, SeasonEnrichment,
    ShowEnrichment, ShowSearchHit,
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
    /// Preferred metadata language as a BCP-47 tag (e.g. `"en-US"`). Applied
    /// to the TMDB client only -- cameo's language support is client-level
    /// configuration (`TmdbConfig::with_language`), not a per-search
    /// parameter, and AniList has no language concept. `None` (or an
    /// empty/whitespace value, filtered out defensively) keeps the provider
    /// default. cameo validates the tag at client construction; an invalid
    /// tag fails `build_client` with a message naming the tag, which
    /// beam-server escalates to a fatal startup error when this knob was
    /// explicitly set (an explicit knob is never silently ignored).
    pub metadata_language: Option<String>,
}

/// Build the `cameo`-backed provider from config, or `None` if no provider
/// is configured (absent TMDB token and AniList disabled).
///
/// The TMDB client is built here rather than by the facade's `with_tmdb`, and
/// registered with the facade for every capability it has, so the credits
/// lookups -- which cameo 0.2.0 offers on `TmdbClient` only, not on the
/// facade (justin13888/cameo#58) -- share one connection pool and one
/// rate-limit budget with every other TMDB request.
pub fn build_client(
    config: CameoWiringConfig,
) -> Result<Option<CameoEnrichmentProvider>, cameo::CameoClientError> {
    let mut builder = cameo::CameoClient::builder();
    let mut tmdb = None;
    let mut any = false;

    if let Some(token) = config.tmdb_api_token.filter(|t| !t.is_empty()) {
        let mut tmdb_config = cameo::TmdbConfig::new(token);
        if let Some(language) = config
            .metadata_language
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            tmdb_config = tmdb_config.with_language(language);
        }
        let client = cameo::TmdbClient::new(tmdb_config).map_err(cameo::ProviderError::from)?;
        let shared = std::sync::Arc::new(client.clone());
        builder = builder
            .register_search_provider(shared.clone())
            .register_detail_provider(shared.clone())
            .register_discovery_provider(shared.clone())
            .register_recommendation_provider(shared.clone())
            .register_season_provider(shared.clone())
            .register_watch_provider(shared);
        tmdb = Some(client);
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
    Ok(Some(CameoEnrichmentProvider {
        client: builder.build()?,
        tmdb,
    }))
}

/// [`EnrichmentProvider`] backed by a configured `cameo::CameoClient`.
#[derive(Debug)]
pub struct CameoEnrichmentProvider {
    client: cameo::CameoClient,
    /// The TMDB client the facade dispatches to, kept for the credits the
    /// facade cannot ask for; `None` when no TMDB token is configured.
    tmdb: Option<cameo::TmdbClient>,
}

impl CameoEnrichmentProvider {
    /// The ids of the providers this dispatches to, in priority order.
    pub fn provider_ids(&self) -> Vec<&str> {
        self.client.provider_ids()
    }

    /// A title's credits, when the provider that matched it reports them:
    /// `None` -- leave the stored credits alone -- for a title whose provider
    /// has no credits lookup, and when the lookup fails. A credits failure
    /// never fails the enrichment it belongs to: the title's own metadata is
    /// still worth applying.
    ///
    /// AniList titles get none: cameo 0.2.0 exposes no AniList characters or
    /// staff (justin13888/cameo#58).
    async fn credits(
        &self,
        id: &ExternalMediaRef,
        kind: CreditsOf,
    ) -> Option<Vec<CreditEnrichment>> {
        let tmdb = self.tmdb.as_ref().filter(|_| id.provider() == "tmdb")?;
        let Ok(native) = id.native().parse::<i32>() else {
            warn!(id = %id, "not a numeric TMDB id; its credits are not fetched");
            return None;
        };
        let fetched = match kind {
            CreditsOf::Movie => tmdb.movie_credits(native).await,
            CreditsOf::Show => tmdb.tv_series_credits(native).await,
        };
        match fetched {
            Ok(credits) => Some(map_credits(&credits)),
            Err(err) => {
                warn!(id = %id, error = %err, "fetching a title's credits failed; its stored credits are kept");
                None
            }
        }
    }
}

/// Which TMDB credits endpoint a title's credits are read from.
#[derive(Debug, Clone, Copy)]
enum CreditsOf {
    Movie,
    Show,
}

/// The most cast credits stored per title: the top of the billing, which is
/// what a detail page shows, rather than every extra a provider lists.
pub const MAX_CAST: usize = 50;

/// The crew jobs stored: those a title is known by. TMDB lists hundreds of
/// crew on a feature; the rest are not what anyone browses by.
pub const KEY_CREW_JOBS: [&str; 11] = [
    "Director",
    "Screenplay",
    "Writer",
    "Story",
    "Novel",
    "Characters",
    "Producer",
    "Executive Producer",
    "Original Music Composer",
    "Director of Photography",
    "Editor",
];

/// A title's credits as Beam stores them: the first [`MAX_CAST`] cast by
/// billing order, and the crew whose job is one of [`KEY_CREW_JOBS`]. A
/// credit with no person id or no name names no one and is dropped.
fn map_credits(credits: &cameo::providers::tmdb::TmdbCredits) -> Vec<CreditEnrichment> {
    let person = |id: u64| ExternalMediaRef::new("tmdb", &id.to_string());
    let mut cast: Vec<&cameo::providers::tmdb::CastMember> = credits
        .cast
        .iter()
        .filter(|member| member.id != 0 && !member.name.trim().is_empty())
        .collect();
    cast.sort_by_key(|member| member.order);
    let cast = cast
        .into_iter()
        .take(MAX_CAST)
        .map(|member| CreditEnrichment {
            person_ref: person(member.id),
            name: member.name.clone(),
            profile_url: member.profile_url.clone(),
            role: CreditRole::Cast {
                character: member
                    .character
                    .as_deref()
                    .map(str::trim)
                    .filter(|character| !character.is_empty())
                    .map(str::to_string),
                billing_order: member.order,
            },
        });
    let crew = credits
        .crew
        .iter()
        .filter(|member| member.id != 0 && !member.name.trim().is_empty())
        .filter_map(|member| {
            let job = member.job.as_deref()?;
            KEY_CREW_JOBS.contains(&job).then(|| CreditEnrichment {
                person_ref: person(member.id),
                name: member.name.clone(),
                profile_url: member.profile_url.clone(),
                role: CreditRole::Crew {
                    job: job.to_string(),
                    department: member.department.clone(),
                },
            })
        });
    cast.chain(crew).collect()
}

#[async_trait::async_trait]
impl EnrichmentProvider for CameoEnrichmentProvider {
    fn available_providers(&self) -> Vec<String> {
        self.provider_ids()
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
        let mut enrichment = map_movie_enrichment(&details);
        enrichment.credits = self.credits(id, CreditsOf::Movie).await;
        Ok(enrichment)
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
        let mut enrichment = map_show_enrichment(&details);
        enrichment.credits = self.credits(id, CreditsOf::Show).await;
        Ok(enrichment)
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
        // Read by a lookup of their own: `CameoEnrichmentProvider::credits`.
        credits: None,
        collection: details
            .belongs_to_collection
            .as_ref()
            .and_then(|collection| map_collection(movie.provider_id.provider(), collection)),
    }
}

/// A movie's collection as Beam stores it, keyed by the matching provider's
/// id for it. One with no id, or no name, is not one Beam can key or show,
/// and is dropped.
fn map_collection(provider: &str, collection: &cameo::Collection) -> Option<CollectionEnrichment> {
    let id = collection.id?;
    let name = collection.name.trim();
    if name.is_empty() {
        return None;
    }
    Some(CollectionEnrichment {
        collection_ref: ExternalMediaRef::new(provider, &id.to_string()),
        name: name.to_string(),
        poster_url: collection.poster_url.clone(),
        backdrop_url: collection.backdrop_url.clone(),
    })
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
        rating: show.vote_average.map(|v| v as f32),
        genres: show.genres.iter().map(|g| g.name().to_string()).collect(),
        // Read by a lookup of their own: `CameoEnrichmentProvider::credits`.
        credits: None,
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
        assert_eq!(enrichment.collection, None, "a movie in no collection");
    }

    #[test]
    fn maps_a_movies_collection_keyed_by_the_providers_id_for_it() {
        let movie = UnifiedMovie::new(MediaId::tmdb(603), "The Matrix");
        let details = UnifiedMovieDetails::new(movie).with_collection(
            cameo::Collection::new("The Matrix Collection")
                .with_id(2344)
                .with_poster_url("https://image.tmdb.org/collection.jpg"),
        );

        assert_eq!(
            map_movie_enrichment(&details).collection,
            Some(CollectionEnrichment {
                collection_ref: ExternalMediaRef::new("tmdb", "2344"),
                name: "The Matrix Collection".to_string(),
                poster_url: Some("https://image.tmdb.org/collection.jpg".to_string()),
                backdrop_url: None,
            })
        );
    }

    #[test]
    fn a_collection_with_no_id_or_no_name_is_dropped() {
        for collection in [
            cameo::Collection::new("Unkeyed Collection"),
            cameo::Collection::new("  ").with_id(1),
        ] {
            let movie = UnifiedMovie::new(MediaId::tmdb(603), "The Matrix");
            let details = UnifiedMovieDetails::new(movie).with_collection(collection.clone());
            assert_eq!(
                map_movie_enrichment(&details).collection,
                None,
                "{collection:?}"
            );
        }
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
        assert_eq!(enrichment.rating, None, "an unrated show stays unrated");
    }

    /// A show's rating is carried on the provider's 0-10 scale, as a movie's
    /// is, so the two sort and filter by rating together (issue #187).
    #[test]
    fn maps_show_enrichment_rating() {
        let show =
            UnifiedTvShow::new(MediaId::tmdb(1399), "Game of Thrones").with_vote_average(8.4);
        let details = UnifiedTvShowDetails::new(show);

        assert_eq!(map_show_enrichment(&details).rating, Some(8.4_f32));
    }

    /// TMDB's credits as its credits endpoint answers them. Built through
    /// serde because cameo marks the credit types `#[non_exhaustive]`.
    fn tmdb_credits(
        cast: serde_json::Value,
        crew: serde_json::Value,
    ) -> cameo::providers::tmdb::TmdbCredits {
        serde_json::from_value(serde_json::json!({ "cast": cast, "crew": crew }))
            .expect("a credits document")
    }

    #[test]
    fn maps_the_billed_cast_and_the_key_crew() {
        let credits = tmdb_credits(
            serde_json::json!([
                {"id": 6384, "name": "Keanu Reeves", "character": "Neo", "order": 0,
                 "profile_url": "https://image.tmdb.org/t/p/h632/neo.jpg"},
                {"id": 2975, "name": "Laurence Fishburne", "character": " ", "order": 1},
                {"id": 0, "name": "No One", "character": "Ghost", "order": 2},
                {"id": 530, "name": "  ", "character": "Blank", "order": 3}
            ]),
            serde_json::json!([
                {"id": 9339, "name": "Lana Wachowski", "job": "Director", "department": "Directing"},
                {"id": 1091, "name": "Key Grip", "job": "Key Grip", "department": "Crew"},
                {"id": 9340, "name": "Lilly Wachowski", "job": "Screenplay", "department": "Writing"},
                {"id": 1092, "name": "No Job"}
            ]),
        );

        let mapped = map_credits(&credits);
        let summary: Vec<(&str, &str, &CreditRole)> = mapped
            .iter()
            .map(|c| (c.person_ref.as_str(), c.name.as_str(), &c.role))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "tmdb:6384",
                    "Keanu Reeves",
                    &CreditRole::Cast {
                        character: Some("Neo".to_string()),
                        billing_order: 0,
                    }
                ),
                (
                    "tmdb:2975",
                    "Laurence Fishburne",
                    // A blank character is no character.
                    &CreditRole::Cast {
                        character: None,
                        billing_order: 1,
                    }
                ),
                (
                    "tmdb:9339",
                    "Lana Wachowski",
                    &CreditRole::Crew {
                        job: "Director".to_string(),
                        department: Some("Directing".to_string()),
                    }
                ),
                (
                    "tmdb:9340",
                    "Lilly Wachowski",
                    &CreditRole::Crew {
                        job: "Screenplay".to_string(),
                        department: Some("Writing".to_string()),
                    }
                ),
            ],
            "nameless and id-less credits are dropped, and only the key crew jobs kept"
        );
        assert_eq!(
            mapped[0].profile_url.as_deref(),
            Some("https://image.tmdb.org/t/p/h632/neo.jpg")
        );
    }

    #[test]
    fn keeps_only_the_top_of_the_billing() {
        let cast: Vec<serde_json::Value> = (0..(MAX_CAST as u32 + 10))
            .rev()
            .map(|order| {
                serde_json::json!({"id": 1000 + order, "name": format!("Actor {order}"), "order": order})
            })
            .collect();
        let mapped = map_credits(&tmdb_credits(
            serde_json::Value::from(cast),
            serde_json::json!([]),
        ));
        assert_eq!(mapped.len(), MAX_CAST);
        let orders: Vec<u32> = mapped
            .iter()
            .map(|c| match &c.role {
                CreditRole::Cast { billing_order, .. } => *billing_order,
                CreditRole::Crew { .. } => panic!("no crew was listed"),
            })
            .collect();
        let expected: Vec<u32> = (0..MAX_CAST as u32).collect();
        assert_eq!(orders, expected, "the lowest billing orders, in order");
    }

    #[tokio::test]
    async fn an_anilist_title_has_no_credits_to_fetch() {
        let provider = build_client(CameoWiringConfig {
            tmdb_api_token: Some("test-token".to_string()),
            anilist_enabled: true,
            metadata_language: None,
        })
        .unwrap()
        .expect("both providers configured");
        // `None` leaves stored credits alone; no request is made for it.
        let reference = ExternalMediaRef::new("anilist", "5114");
        assert_eq!(provider.credits(&reference, CreditsOf::Show).await, None);
        // Nor for a TMDB reference that is not a TMDB id.
        let malformed = ExternalMediaRef::new("tmdb", "not-a-number");
        assert_eq!(provider.credits(&malformed, CreditsOf::Movie).await, None);
    }

    #[test]
    fn tmdb_keeps_priority_over_anilist() {
        let provider = build_client(CameoWiringConfig {
            tmdb_api_token: Some("test-token".to_string()),
            anilist_enabled: true,
            metadata_language: None,
        })
        .unwrap()
        .expect("both providers configured");
        assert_eq!(provider.provider_ids(), ["tmdb", "anilist"]);
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
            metadata_language: None,
        };
        let client = build_client(config).unwrap();
        assert!(client.is_none());
    }

    #[test]
    fn build_client_returns_some_when_anilist_enabled() {
        let config = CameoWiringConfig {
            tmdb_api_token: None,
            anilist_enabled: true,
            metadata_language: None,
        };
        let client = build_client(config).unwrap();
        assert!(client.is_some());
    }

    #[test]
    fn build_client_accepts_valid_metadata_language_for_tmdb() {
        // Client construction is offline (no discovery request); cameo only
        // validates the config, so a valid BCP-47 tag must build fine.
        let config = CameoWiringConfig {
            tmdb_api_token: Some("test-token".to_string()),
            anilist_enabled: false,
            metadata_language: Some("en-US".to_string()),
        };
        let client = build_client(config).unwrap();
        assert!(client.is_some());
    }

    #[test]
    fn build_client_rejects_invalid_metadata_language_with_clear_error() {
        // cameo validates BCP-47 at construction; the startup fallback path
        // logs this error before disabling enrichment, so it must name the
        // language problem rather than something opaque.
        let config = CameoWiringConfig {
            tmdb_api_token: Some("test-token".to_string()),
            anilist_enabled: false,
            metadata_language: Some("english".to_string()),
        };
        let err = build_client(config).expect_err("an invalid language tag must fail the build");
        let message = err.to_string();
        assert!(
            message.contains("language") && message.contains("english"),
            "error must name the bad language tag, got: {message}"
        );
    }

    #[test]
    fn build_client_ignores_whitespace_only_metadata_language() {
        // Defense in depth: beam-server normalizes empty -> None, but the
        // wiring config is a public seam, so a stray whitespace value must
        // not reach cameo's validator and kill enrichment.
        let config = CameoWiringConfig {
            tmdb_api_token: Some("test-token".to_string()),
            anilist_enabled: false,
            metadata_language: Some("   ".to_string()),
        };
        let client = build_client(config).unwrap();
        assert!(client.is_some());
    }
}
