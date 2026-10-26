//! TheTVDB-backed [`EnrichmentProvider`] adapter (issue #267), over
//! TheTVDB's v4 API.
//!
//! Written against the API directly, not through `cameo`: cameo 0.2.0 -- the
//! latest release -- has no TheTVDB client (justin13888/cameo#60).
//! When a cameo release carries one, this adapter is replaced by registering
//! it with the cameo facade in [`super::cameo::build_client`], and the
//! composite in [`super::composite`] has one provider again.
//!
//! As in [`super::artwork`], the decisions are pure functions with no socket
//! in them -- [`classify`] for a response's status, and the `map_*`
//! functions for its body -- so most of the policy is tested without a
//! server; the request flow itself (login, the bearer token, one re-login on
//! a `401`, paging) is tested against a local listener.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::{Client, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use beam_domain::models::access::normalize_certification;
use beam_domain::providers::enrichment::{
    EnrichmentError, EnrichmentProvider, EpisodeEnrichment, ExternalMediaRef, MediaQuery,
    MovieEnrichment, MovieSearchHit, SeasonEnrichment, ShowEnrichment, ShowSearchHit,
};
use beam_domain::services::Clock;

/// The provider name: a TheTVDB match is stored as `tvdb:<id>`, as a
/// TheTVDB pin is.
pub const PROVIDER: &str = "tvdb";

/// TheTVDB's v4 API. The trailing slash makes every path relative to it.
pub const DEFAULT_BASE_URL: &str = "https://api4.thetvdb.com/v4/";

/// The language metadata is read in, as TheTVDB spells it (ISO 639-2/T).
/// `BEAM_METADATA_LANGUAGE` applies to TMDB alone, as it always has; a
/// record with no English translation falls back to its own language.
const LANGUAGE: &str = "eng";

/// The country whose certification a title is rated by, as TheTVDB spells
/// it -- the one [`normalize_certification`] recognises.
const CERTIFICATION_COUNTRY: &str = "usa";

/// How long one request may take, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The most pages of a series' episodes read: TheTVDB pages by 500, so this
/// is past the longest series it lists.
const MAX_EPISODE_PAGES: u32 = 20;

/// How long a series' episode list answers its seasons. One enrichment of a
/// show reads every season it has, and each would otherwise list the whole
/// series again.
const EPISODE_CACHE_TTL: Duration = Duration::from_secs(600);

/// How many series' episode lists are kept at once.
const EPISODE_CACHE_SERIES: usize = 16;

/// TheTVDB's artwork types: the backdrops of a series and of a movie.
const SERIES_BACKGROUND: u32 = 3;
const MOVIE_BACKGROUND: u32 = 15;

/// Configuration for the TheTVDB client. An absent or blank API key is not
/// configured, and [`build_client`] returns `None`.
#[derive(Clone)]
pub struct TvdbWiringConfig {
    pub api_key: Option<String>,
    /// The subscriber PIN a user-supported API key is issued with; a
    /// licensed project key has none.
    pub pin: Option<String>,
}

impl std::fmt::Debug for TvdbWiringConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { api_key, pin } = self;
        f.debug_struct("TvdbWiringConfig")
            .field("api_key", &api_key.as_ref().map(|_| "<redacted>"))
            .field("pin", &pin.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Build the TheTVDB provider, or `None` when no API key is configured.
/// Offline: the first lookup logs in.
pub fn build_client(
    config: TvdbWiringConfig,
    clock: Arc<dyn Clock>,
) -> Result<Option<TvdbEnrichmentProvider>, reqwest::Error> {
    let TvdbWiringConfig { api_key, pin } = config;
    let Some(api_key) = api_key
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
    else {
        return Ok(None);
    };
    let pin = pin
        .map(|pin| pin.trim().to_string())
        .filter(|pin| !pin.is_empty());
    let base = Url::parse(DEFAULT_BASE_URL).expect("the default base URL parses");
    TvdbEnrichmentProvider::new(base, api_key, pin, clock).map(Some)
}

/// [`EnrichmentProvider`] over TheTVDB's v4 API: shows, their seasons and
/// specials, their absolute numbering, and movies.
pub struct TvdbEnrichmentProvider {
    client: Client,
    base: Url,
    api_key: String,
    pin: Option<String>,
    /// The bearer token the last login answered; `None` before the first.
    token: tokio::sync::Mutex<Option<String>>,
    clock: Arc<dyn Clock>,
    episodes: parking_lot::Mutex<VecDeque<CachedEpisodes>>,
}

impl std::fmt::Debug for TvdbEnrichmentProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TvdbEnrichmentProvider")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

/// A series' episodes, as listed when.
struct CachedEpisodes {
    series: u32,
    fetched_at: Instant,
    episodes: Arc<Vec<WireEpisode>>,
}

impl TvdbEnrichmentProvider {
    fn new(
        base: Url,
        api_key: String,
        pin: Option<String>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, reqwest::Error> {
        // No cookie store (the feature is off), and no redirect: an API
        // answer is never a redirect, and a token must not follow one.
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            base,
            api_key,
            pin,
            token: tokio::sync::Mutex::new(None),
            clock,
            episodes: parking_lot::Mutex::new(VecDeque::new()),
        })
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, EnrichmentError> {
        let mut url = self.base.join(path).map_err(|err| {
            EnrichmentError::Provider(format!("invalid TheTVDB path {path}: {err}"))
        })?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    /// The bearer token, logging in when there is none or `fresh` asks for
    /// a new one.
    async fn token(&self, fresh: bool) -> Result<String, EnrichmentError> {
        let mut token = self.token.lock().await;
        if !fresh && let Some(token) = token.as_ref() {
            return Ok(token.clone());
        }
        let mut body = serde_json::json!({ "apikey": self.api_key });
        if let Some(pin) = &self.pin {
            body["pin"] = serde_json::Value::String(pin.clone());
        }
        let response = self
            .client
            .post(self.url("login", &[])?)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(transport)?;
        let status = response.status().as_u16();
        if status == 401 {
            return Err(EnrichmentError::Provider(
                "auth error: TheTVDB refused the API key".to_string(),
            ));
        }
        classify(status, retry_after(&response))?;
        let login: Envelope<LoginData> = read(response).await?;
        let fresh_token = login
            .data
            .map(|data| data.token)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| EnrichmentError::Provider("TheTVDB's login answered no token".into()))?;
        *token = Some(fresh_token.clone());
        Ok(fresh_token)
    }

    /// `GET` `url` with the bearer token, logging in again once when the
    /// token is refused (it lasts a month, and a restart outlives none).
    async fn get<T: DeserializeOwned>(&self, url: Url) -> Result<Envelope<T>, EnrichmentError> {
        let mut fresh = false;
        loop {
            let token = self.token(fresh).await?;
            let response = self
                .client
                .get(url.clone())
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(transport)?;
            let status = response.status().as_u16();
            if status == 401 && !fresh {
                fresh = true;
                continue;
            }
            classify(status, retry_after(&response))?;
            return read(response).await;
        }
    }

    /// Every episode of `series` in its default (aired) order, specials as
    /// season 0, from the cache while it is fresh.
    async fn episodes(&self, series: u32) -> Result<Arc<Vec<WireEpisode>>, EnrichmentError> {
        let now = self.clock.monotonic();
        {
            let mut cache = self.episodes.lock();
            cache.retain(|entry| now.duration_since(entry.fetched_at) < EPISODE_CACHE_TTL);
            if let Some(entry) = cache.iter().find(|entry| entry.series == series) {
                return Ok(entry.episodes.clone());
            }
        }
        let mut episodes = Vec::new();
        for page in 0..MAX_EPISODE_PAGES {
            let url = self.url(
                &format!("series/{series}/episodes/default/{LANGUAGE}"),
                &[("page", &page.to_string())],
            )?;
            let listed: Envelope<EpisodePage> = self.get(url).await?;
            let more = listed.links.and_then(|links| links.next).is_some();
            episodes.extend(listed.data.map(|page| page.episodes).unwrap_or_default());
            if !more {
                break;
            }
        }
        let episodes = Arc::new(episodes);
        let mut cache = self.episodes.lock();
        cache.retain(|entry| entry.series != series);
        if cache.len() >= EPISODE_CACHE_SERIES {
            cache.pop_front();
        }
        cache.push_back(CachedEpisodes {
            series,
            fetched_at: now,
            episodes: episodes.clone(),
        });
        Ok(episodes)
    }

    async fn search(
        &self,
        query: &MediaQuery,
        kind: &str,
    ) -> Result<Vec<SearchHit>, EnrichmentError> {
        let url = self.url(
            "search",
            &[
                ("query", query.title.as_str()),
                ("type", kind),
                ("limit", "20"),
            ],
        )?;
        let found: Envelope<Vec<WireSearchResult>> = self.get(url).await?;
        Ok(found
            .data
            .unwrap_or_default()
            .iter()
            .filter_map(map_search_result)
            .collect())
    }
}

/// The TheTVDB id `id` names, or why it names none.
fn native_id(id: &ExternalMediaRef) -> Result<u32, EnrichmentError> {
    if id.provider() != PROVIDER {
        return Err(EnrichmentError::NotConfigured);
    }
    id.native()
        .parse()
        .map_err(|_| EnrichmentError::Provider(format!("not a TheTVDB id: {id}")))
}

#[async_trait::async_trait]
impl EnrichmentProvider for TvdbEnrichmentProvider {
    fn available_providers(&self) -> Vec<String> {
        vec![PROVIDER.to_string()]
    }

    async fn search_movies(
        &self,
        query: &MediaQuery,
    ) -> Result<Vec<MovieSearchHit>, EnrichmentError> {
        Ok(self
            .search(query, "movie")
            .await?
            .into_iter()
            .map(|hit| MovieSearchHit {
                external_ref: hit.external_ref,
                title: hit.title,
                original_title: hit.original_title,
                year: hit.year,
                popularity: None,
                vote_average: None,
            })
            .collect())
    }

    async fn search_shows(
        &self,
        query: &MediaQuery,
    ) -> Result<Vec<ShowSearchHit>, EnrichmentError> {
        Ok(self
            .search(query, "series")
            .await?
            .into_iter()
            .map(|hit| ShowSearchHit {
                external_ref: hit.external_ref,
                title: hit.title,
                original_title: hit.original_title,
                year: hit.year,
                popularity: None,
                vote_average: None,
            })
            .collect())
    }

    async fn movie_enrichment(
        &self,
        id: &ExternalMediaRef,
    ) -> Result<MovieEnrichment, EnrichmentError> {
        let native = native_id(id)?;
        let url = self.url(
            &format!("movies/{native}/extended"),
            &[("meta", "translations")],
        )?;
        let found: Envelope<WireRecord> = self.get(url).await?;
        found
            .data
            .map(|record| map_movie(&record))
            .ok_or(EnrichmentError::NotFound)
    }

    async fn show_enrichment(
        &self,
        id: &ExternalMediaRef,
    ) -> Result<ShowEnrichment, EnrichmentError> {
        let native = native_id(id)?;
        let url = self.url(
            &format!("series/{native}/extended"),
            &[("meta", "translations")],
        )?;
        let found: Envelope<WireRecord> = self.get(url).await?;
        found
            .data
            .map(|record| map_show(&record))
            .ok_or(EnrichmentError::NotFound)
    }

    async fn season_enrichment(
        &self,
        show: &ExternalMediaRef,
        season_number: u32,
    ) -> Result<SeasonEnrichment, EnrichmentError> {
        let native = native_id(show)?;
        let episodes = self.episodes(native).await?;
        map_season(&episodes, season_number).ok_or(EnrichmentError::NotFound)
    }

    async fn invalidate(&self, id: &ExternalMediaRef) {
        if let Ok(series) = native_id(id) {
            self.episodes.lock().retain(|entry| entry.series != series);
        }
    }
}

fn transport(err: reqwest::Error) -> EnrichmentError {
    EnrichmentError::Transport(err.to_string())
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

async fn read<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, EnrichmentError> {
    let body = response.bytes().await.map_err(transport)?;
    serde_json::from_slice(&body)
        .map_err(|err| EnrichmentError::Provider(format!("deserialization error: {err}")))
}

/// What a response's status decides, before its body is read.
pub(crate) fn classify(status: u16, retry_after: Option<Duration>) -> Result<(), EnrichmentError> {
    match status {
        200..=299 => Ok(()),
        404 => Err(EnrichmentError::NotFound),
        429 => Err(EnrichmentError::RateLimited { retry_after }),
        401 | 403 => Err(EnrichmentError::Provider(format!(
            "auth error: TheTVDB answered HTTP {status}"
        ))),
        other => Err(EnrichmentError::Provider(format!(
            "api error (HTTP {other}) from TheTVDB"
        ))),
    }
}

// ── Wire types: only the fields Beam reads, each optional ──────────────────

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: Option<T>,
    #[serde(default)]
    links: Option<Links>,
}

#[derive(Debug, Deserialize)]
struct Links {
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LoginData {
    token: String,
}

#[derive(Debug, Deserialize)]
struct WireSearchResult {
    tvdb_id: Option<String>,
    name: Option<String>,
    year: Option<String>,
    #[serde(default)]
    translations: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WireRecord {
    id: u32,
    name: Option<String>,
    overview: Option<String>,
    image: Option<String>,
    year: Option<String>,
    first_aired: Option<String>,
    runtime: Option<u32>,
    /// Spelled in snake case by TheTVDB, unlike its neighbours.
    #[serde(rename = "first_release")]
    first_release: Option<WireRelease>,
    genres: Vec<WireGenre>,
    remote_ids: Vec<WireRemoteId>,
    content_ratings: Vec<WireContentRating>,
    artworks: Vec<WireArtwork>,
    translations: WireTranslations,
}

#[derive(Debug, Deserialize)]
struct WireRelease {
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireGenre {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRemoteId {
    id: String,
    source_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireContentRating {
    name: String,
    country: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireArtwork {
    image: Option<String>,
    #[serde(rename = "type")]
    kind: Option<u32>,
    score: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WireTranslations {
    name_translations: Vec<WireTranslation>,
    overview_translations: Vec<WireTranslation>,
}

#[derive(Debug, Deserialize)]
struct WireTranslation {
    language: String,
    name: Option<String>,
    overview: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EpisodePage {
    #[serde(default)]
    episodes: Vec<WireEpisode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEpisode {
    season_number: Option<u32>,
    number: Option<u32>,
    absolute_number: Option<u32>,
    name: Option<String>,
    overview: Option<String>,
    aired: Option<String>,
    runtime: Option<u32>,
    image: Option<String>,
}

// ── Mapping ─────────────────────────────────────────────────────────────────

/// A search result, for a movie or a show alike.
#[derive(Debug, PartialEq)]
struct SearchHit {
    external_ref: ExternalMediaRef,
    title: String,
    original_title: Option<String>,
    year: Option<u32>,
}

fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The year a `YYYY` or `YYYY-MM-DD` value starts with.
fn year_of(value: Option<&str>) -> Option<u32> {
    value
        .and_then(|value| value.get(..4))
        .and_then(|year| year.parse().ok())
        .filter(|year| *year > 0)
}

fn date_of(value: Option<&str>) -> Option<chrono::NaiveDate> {
    value.and_then(|value| chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok())
}

/// The English title, and the record's own name beside it when that is
/// another: the matcher scores a query against both.
fn titles(english: Option<String>, own: Option<String>) -> Option<(String, Option<String>)> {
    match (english, own) {
        (Some(english), Some(own)) if english != own => Some((english, Some(own))),
        (Some(title), _) | (None, Some(title)) => Some((title, None)),
        (None, None) => None,
    }
}

/// A result with no numeric id or no name is not one Beam can match.
fn map_search_result(result: &WireSearchResult) -> Option<SearchHit> {
    let id: u32 = result.tvdb_id.as_deref()?.trim().parse().ok()?;
    let (title, original_title) = titles(
        non_blank(result.translations.get(LANGUAGE).map(String::as_str)),
        non_blank(result.name.as_deref()),
    )?;
    Some(SearchHit {
        external_ref: ExternalMediaRef::new(PROVIDER, &id.to_string()),
        title,
        original_title,
        year: year_of(result.year.as_deref()),
    })
}

/// The ids a record names on other providers, as `(tmdb, imdb)`.
fn remote_ids(record: &WireRecord) -> (Option<u32>, Option<String>) {
    let mut tmdb = None;
    let mut imdb = None;
    for remote in &record.remote_ids {
        match remote.source_name.as_deref() {
            Some("TheMovieDB.com") if tmdb.is_none() => tmdb = remote.id.trim().parse().ok(),
            Some("IMDB") if imdb.is_none() && remote.id.starts_with("tt") => {
                imdb = Some(remote.id.trim().to_string());
            }
            _ => {}
        }
    }
    (tmdb, imdb)
}

fn english_name(record: &WireRecord) -> Option<String> {
    record
        .translations
        .name_translations
        .iter()
        .find(|translation| translation.language == LANGUAGE)
        .and_then(|translation| non_blank(translation.name.as_deref()))
}

fn english_overview(record: &WireRecord) -> Option<String> {
    record
        .translations
        .overview_translations
        .iter()
        .find(|translation| translation.language == LANGUAGE)
        .and_then(|translation| non_blank(translation.overview.as_deref()))
        .or_else(|| non_blank(record.overview.as_deref()))
}

/// The highest-scored artwork of `kind`.
fn best_artwork(record: &WireRecord, kind: u32) -> Option<String> {
    record
        .artworks
        .iter()
        .filter(|artwork| artwork.kind == Some(kind))
        .filter_map(|artwork| Some((non_blank(artwork.image.as_deref())?, artwork.score)))
        .max_by(|(_, a), (_, b)| a.unwrap_or(0.0).total_cmp(&b.unwrap_or(0.0)))
        .map(|(image, _)| image)
}

fn certification(record: &WireRecord) -> Option<String> {
    record
        .content_ratings
        .iter()
        .find(|rating| rating.country.as_deref() == Some(CERTIFICATION_COUNTRY))
        .and_then(|rating| normalize_certification(&rating.name))
}

fn genres(record: &WireRecord) -> Vec<String> {
    record
        .genres
        .iter()
        .filter_map(|genre| non_blank(Some(&genre.name)))
        .collect()
}

fn map_show(record: &WireRecord) -> ShowEnrichment {
    let (tmdb_id, imdb_id) = remote_ids(record);
    let (title, original_title) =
        titles(english_name(record), non_blank(record.name.as_deref())).unwrap_or_default();
    ShowEnrichment {
        tmdb_id,
        imdb_id,
        anilist_id: None,
        tvdb_id: Some(record.id),
        title,
        original_title,
        description: english_overview(record),
        year: year_of(record.year.as_deref()).or_else(|| year_of(record.first_aired.as_deref())),
        poster_url: non_blank(record.image.as_deref()),
        backdrop_url: best_artwork(record, SERIES_BACKGROUND),
        // TheTVDB's `score` counts its users' favourites, not a 0-10 rating.
        rating: None,
        content_rating: certification(record),
        genres: genres(record),
        // TheTVDB's characters are not read: the stored credits are kept.
        credits: None,
    }
}

fn map_movie(record: &WireRecord) -> MovieEnrichment {
    let (tmdb_id, imdb_id) = remote_ids(record);
    let (title, original_title) =
        titles(english_name(record), non_blank(record.name.as_deref())).unwrap_or_default();
    let release_date = date_of(
        record
            .first_release
            .as_ref()
            .and_then(|release| release.date.as_deref()),
    );
    MovieEnrichment {
        tmdb_id,
        imdb_id,
        anilist_id: None,
        title,
        original_title,
        description: english_overview(record),
        year: year_of(record.year.as_deref())
            .or_else(|| release_date.map(|date| chrono::Datelike::year(&date) as u32)),
        release_date,
        runtime_mins: record.runtime.filter(|minutes| *minutes > 0),
        poster_url: non_blank(record.image.as_deref()),
        backdrop_url: best_artwork(record, MOVIE_BACKGROUND),
        rating: None,
        content_rating: certification(record),
        genres: genres(record),
        credits: None,
        // TheTVDB's lists are not collections Beam reads.
        collection: None,
    }
}

/// Season `season_number` of a series' episodes, or `None` when it has no
/// episode in it. Its first aired date is its earliest episode's.
fn map_season(episodes: &[WireEpisode], season_number: u32) -> Option<SeasonEnrichment> {
    let episodes: Vec<EpisodeEnrichment> = episodes
        .iter()
        .filter(|episode| episode.season_number == Some(season_number))
        .filter_map(|episode| {
            Some(EpisodeEnrichment {
                episode_number: episode.number.filter(|number| *number > 0)?,
                absolute_number: episode.absolute_number.filter(|number| *number > 0),
                title: non_blank(episode.name.as_deref()),
                description: non_blank(episode.overview.as_deref()),
                air_date: date_of(episode.aired.as_deref()),
                runtime_mins: episode.runtime.filter(|minutes| *minutes > 0),
                thumbnail_url: non_blank(episode.image.as_deref()),
            })
        })
        .collect();
    if episodes.is_empty() {
        return None;
    }
    Some(SeasonEnrichment {
        season_number,
        // A season's poster is a lookup of its own, which is not made.
        poster_url: None,
        air_date: episodes.iter().filter_map(|episode| episode.air_date).min(),
        episodes,
    })
}

#[cfg(test)]
#[path = "tvdb_tests.rs"]
mod tests;
