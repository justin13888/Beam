//! One [`EnrichmentProvider`] over several (issue #267): the cameo-backed
//! TMDB and AniList adapter, and TheTVDB's own, which cameo does not carry
//! (see [`super::tvdb`]).
//!
//! Searches ask every provider, in order, and answer every hit, so the
//! matcher ranks them together -- and an earlier provider wins a tie. A
//! lookup by id goes to the one provider whose name the id carries.

use std::sync::Arc;

use tracing::warn;

use beam_domain::providers::enrichment::{
    EnrichmentError, EnrichmentProvider, ExternalMediaRef, MediaQuery, MovieEnrichment,
    MovieSearchHit, SeasonEnrichment, ShowEnrichment, ShowSearchHit,
};

/// [`EnrichmentProvider`] dispatching to several, in priority order.
#[derive(Debug)]
pub struct CompositeEnrichmentProvider {
    providers: Vec<Arc<dyn EnrichmentProvider>>,
}

impl CompositeEnrichmentProvider {
    /// `providers` in priority order: the first is asked first, and wins a
    /// tie between equally good matches.
    pub fn new(providers: Vec<Arc<dyn EnrichmentProvider>>) -> Self {
        Self { providers }
    }

    /// The provider that resolves `id`, or `NotConfigured` when none does:
    /// a match made through a provider since unconfigured stays pending
    /// rather than being searched away.
    fn owner(&self, id: &ExternalMediaRef) -> Result<&dyn EnrichmentProvider, EnrichmentError> {
        self.providers
            .iter()
            .find(|provider| {
                provider
                    .available_providers()
                    .iter()
                    .any(|name| name == id.provider())
            })
            .map(|provider| &**provider)
            .ok_or(EnrichmentError::NotConfigured)
    }
}

/// One provider's search in flight, as `async_trait` returns it.
type Search<'a, T> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<T>, EnrichmentError>> + Send + 'a>,
>;

/// Every provider's hits, in provider order. A rate limit stops the search
/// at once -- the sweep backs off rather than ask anyone else meanwhile --
/// and another failure only fails the search when no provider found a hit.
async fn search_all<'a, T, F>(
    providers: &'a [Arc<dyn EnrichmentProvider>],
    search: F,
) -> Result<Vec<T>, EnrichmentError>
where
    F: Fn(&'a dyn EnrichmentProvider) -> Search<'a, T>,
{
    let mut hits = Vec::new();
    let mut last_err = None;
    for provider in providers {
        match search(&**provider).await {
            Ok(found) => hits.extend(found),
            Err(err @ EnrichmentError::RateLimited { .. }) => return Err(err),
            Err(err) => {
                warn!(providers = ?provider.available_providers(), error = %err, "a provider's search failed; asking the next");
                last_err = Some(err);
            }
        }
    }
    match last_err {
        Some(err) if hits.is_empty() => Err(err),
        _ => Ok(hits),
    }
}

#[async_trait::async_trait]
impl EnrichmentProvider for CompositeEnrichmentProvider {
    fn available_providers(&self) -> Vec<String> {
        self.providers
            .iter()
            .flat_map(|provider| provider.available_providers())
            .collect()
    }

    async fn search_movies(
        &self,
        query: &MediaQuery,
    ) -> Result<Vec<MovieSearchHit>, EnrichmentError> {
        search_all(&self.providers, |provider| provider.search_movies(query)).await
    }

    async fn search_shows(
        &self,
        query: &MediaQuery,
    ) -> Result<Vec<ShowSearchHit>, EnrichmentError> {
        search_all(&self.providers, |provider| provider.search_shows(query)).await
    }

    async fn movie_enrichment(
        &self,
        id: &ExternalMediaRef,
    ) -> Result<MovieEnrichment, EnrichmentError> {
        self.owner(id)?.movie_enrichment(id).await
    }

    async fn show_enrichment(
        &self,
        id: &ExternalMediaRef,
    ) -> Result<ShowEnrichment, EnrichmentError> {
        self.owner(id)?.show_enrichment(id).await
    }

    async fn season_enrichment(
        &self,
        show: &ExternalMediaRef,
        season_number: u32,
    ) -> Result<SeasonEnrichment, EnrichmentError> {
        self.owner(show)?
            .season_enrichment(show, season_number)
            .await
    }

    async fn invalidate(&self, id: &ExternalMediaRef) {
        if let Ok(provider) = self.owner(id) {
            provider.invalidate(id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider;

    fn show_hit(provider: &str, id: &str, title: &str) -> ShowSearchHit {
        ShowSearchHit {
            external_ref: ExternalMediaRef::new(provider, id),
            title: title.to_string(),
            original_title: None,
            year: None,
            popularity: None,
            vote_average: None,
        }
    }

    fn query(title: &str) -> MediaQuery {
        MediaQuery {
            title: title.to_string(),
            year: None,
        }
    }

    fn composite(providers: Vec<InMemoryEnrichmentProvider>) -> CompositeEnrichmentProvider {
        CompositeEnrichmentProvider::new(
            providers
                .into_iter()
                .map(|provider| Arc::new(provider) as Arc<dyn EnrichmentProvider>)
                .collect(),
        )
    }

    #[tokio::test]
    async fn a_search_answers_every_providers_hits_in_priority_order() {
        let provider = composite(vec![
            InMemoryEnrichmentProvider::new(&["tmdb", "anilist"])
                .with_show_search("Frieren", vec![show_hit("tmdb", "209867", "Frieren")]),
            InMemoryEnrichmentProvider::new(&["tvdb"])
                .with_show_search("Frieren", vec![show_hit("tvdb", "424536", "Frieren")]),
        ]);

        assert_eq!(provider.available_providers(), ["tmdb", "anilist", "tvdb"]);
        let hits = provider.search_shows(&query("Frieren")).await.unwrap();
        let refs: Vec<&str> = hits.iter().map(|hit| hit.external_ref.as_str()).collect();
        assert_eq!(refs, ["tmdb:209867", "tvdb:424536"]);
    }

    #[tokio::test]
    async fn a_failing_provider_fails_the_search_only_when_nothing_was_found() {
        let failing = || {
            InMemoryEnrichmentProvider::new(&["tmdb"])
                .with_search_error(|| EnrichmentError::Transport("connection reset".into()))
        };
        let found = composite(vec![
            failing(),
            InMemoryEnrichmentProvider::new(&["tvdb"])
                .with_show_search("Frieren", vec![show_hit("tvdb", "424536", "Frieren")]),
        ]);
        assert_eq!(
            found.search_shows(&query("Frieren")).await.unwrap().len(),
            1
        );

        // Nothing found anywhere, and one provider failed: the failure is
        // the answer, so the title is retried rather than left unmatched.
        let nothing = composite(vec![failing(), InMemoryEnrichmentProvider::new(&["tvdb"])]);
        assert!(matches!(
            nothing.search_movies(&query("Frieren")).await,
            Err(EnrichmentError::Transport(_))
        ));
    }

    #[tokio::test]
    async fn a_rate_limit_stops_the_search_whatever_was_found() {
        let provider = composite(vec![
            InMemoryEnrichmentProvider::new(&["tmdb"])
                .with_show_search("Frieren", vec![show_hit("tmdb", "209867", "Frieren")]),
            InMemoryEnrichmentProvider::new(&["tvdb"]).with_search_error(|| {
                EnrichmentError::RateLimited {
                    retry_after: Some(std::time::Duration::from_secs(5)),
                }
            }),
        ]);
        assert!(matches!(
            provider.search_shows(&query("Frieren")).await,
            Err(EnrichmentError::RateLimited { retry_after: Some(after) })
                if after.as_secs() == 5
        ));
    }

    #[tokio::test]
    async fn a_lookup_goes_to_the_provider_the_id_names() {
        let tmdb_ref = ExternalMediaRef::new("tmdb", "1399");
        let tvdb_ref = ExternalMediaRef::new("tvdb", "121361");
        let tvdb = Arc::new(
            InMemoryEnrichmentProvider::new(&["tvdb"]).with_show_enrichment(
                tvdb_ref.clone(),
                ShowEnrichment {
                    tvdb_id: Some(121361),
                    title: "Game of Thrones".to_string(),
                    ..Default::default()
                },
            ),
        );
        let provider = CompositeEnrichmentProvider::new(vec![
            Arc::new(InMemoryEnrichmentProvider::new(&["tmdb"])),
            tvdb.clone(),
        ]);

        let found = provider.show_enrichment(&tvdb_ref).await.unwrap();
        assert_eq!(found.tvdb_id, Some(121361));
        assert!(matches!(
            provider.show_enrichment(&tmdb_ref).await,
            Err(EnrichmentError::NotFound)
        ));
        assert!(matches!(
            provider.season_enrichment(&tvdb_ref, 1).await,
            Err(EnrichmentError::NotFound)
        ));

        provider.invalidate(&tvdb_ref).await;
        provider
            .invalidate(&ExternalMediaRef::new("imdb", "tt0944947"))
            .await;
        assert_eq!(tvdb.invalidated_refs(), ["tvdb:121361"]);
    }

    #[tokio::test]
    async fn an_id_no_provider_resolves_is_not_configured() {
        let provider = composite(vec![InMemoryEnrichmentProvider::new(&["tmdb"])]);
        let anilist = ExternalMediaRef::new("anilist", "5114");
        assert!(matches!(
            provider.movie_enrichment(&anilist).await,
            Err(EnrichmentError::NotConfigured)
        ));
        assert!(matches!(
            provider.show_enrichment(&anilist).await,
            Err(EnrichmentError::NotConfigured)
        ));
    }
}
