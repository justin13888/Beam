//! The shared behavioural contract for [`EnrichmentProvider`], bound by the
//! in-memory double, TheTVDB's adapter (against a local stand-in) and the
//! composite, so the double the rest of the suite runs on answers as the
//! real providers do.
//!
//! What every provider owes its callers, as the trait documents it:
//!
//! * it names the provider its ids carry;
//! * a title it knows resolves by its id -- a movie, a show and a season;
//! * an id of its own that it does not know is
//!   [`EnrichmentError::NotFound`], and so is a season it does not have;
//! * an id of a provider it is not is [`EnrichmentError::NotConfigured`],
//!   which the sweep reads as "leave the match pending" rather than "search
//!   it away";
//! * a search finds a title it knows by that title, and a search for
//!   nothing it knows is an empty answer, not an error;
//! * invalidating an id -- its own or not -- never loses a title.

use std::sync::Arc;

use beam_domain::providers::enrichment::{EnrichmentProvider, ExternalMediaRef, MediaQuery};

/// A provider under the contract, and what it was set up to know.
pub(crate) struct ContractSubject {
    pub(crate) provider: Arc<dyn EnrichmentProvider>,
    /// The name the provider's ids carry.
    pub(crate) name: &'static str,
    /// A movie it knows, and its title.
    pub(crate) movie: ExternalMediaRef,
    pub(crate) movie_title: &'static str,
    /// A show it knows, and its title.
    pub(crate) show: ExternalMediaRef,
    pub(crate) show_title: &'static str,
    /// A season of [`Self::show`] it knows.
    pub(crate) season: u32,
    /// An id of the provider's own that names nothing it knows.
    pub(crate) unknown: ExternalMediaRef,
}

/// An id of a provider no subject is.
pub(crate) fn foreign_ref() -> ExternalMediaRef {
    ExternalMediaRef::new("contract-foreign", "1")
}

/// A search for `title`.
pub(crate) fn query(title: &str) -> MediaQuery {
    MediaQuery {
        title: title.to_string(),
        year: None,
    }
}

/// A title no subject knows.
pub(crate) const UNKNOWN_TITLE: &str = "No Such Title Anywhere";

/// The contract's tests, over the subject `$setup` -- an
/// `async fn() -> ContractSubject` -- builds.
macro_rules! enrichment_provider_contract {
    ($setup:path) => {
        use ::beam_domain::providers::enrichment::EnrichmentError;
        use $crate::providers::contract::{UNKNOWN_TITLE, foreign_ref, query};

        #[tokio::test]
        async fn it_names_the_provider_its_ids_carry() {
            let subject: $crate::providers::contract::ContractSubject = $setup().await;
            assert!(
                subject
                    .provider
                    .available_providers()
                    .iter()
                    .any(|name| name == subject.name),
                "{:?}",
                subject.provider.available_providers()
            );
            assert_eq!(subject.movie.provider(), subject.name);
            assert_eq!(subject.show.provider(), subject.name);
            assert_eq!(subject.unknown.provider(), subject.name);
        }

        #[tokio::test]
        async fn a_title_it_knows_resolves_by_its_id() {
            let subject: $crate::providers::contract::ContractSubject = $setup().await;
            let movie = subject.provider.movie_enrichment(&subject.movie).await;
            assert_eq!(movie.unwrap().title, subject.movie_title);
            let show = subject.provider.show_enrichment(&subject.show).await;
            assert_eq!(show.unwrap().title, subject.show_title);
            let season = subject
                .provider
                .season_enrichment(&subject.show, subject.season)
                .await;
            assert_eq!(season.unwrap().season_number, subject.season);
        }

        #[tokio::test]
        async fn an_id_of_its_own_it_does_not_know_is_not_found() {
            let subject: $crate::providers::contract::ContractSubject = $setup().await;
            let provider = &subject.provider;
            let movie = provider.movie_enrichment(&subject.unknown).await;
            assert!(matches!(movie, Err(EnrichmentError::NotFound)), "{movie:?}");
            let show = provider.show_enrichment(&subject.unknown).await;
            assert!(matches!(show, Err(EnrichmentError::NotFound)), "{show:?}");
            let season = provider.season_enrichment(&subject.unknown, 1).await;
            assert!(
                matches!(season, Err(EnrichmentError::NotFound)),
                "{season:?}"
            );
            let missing_season = provider
                .season_enrichment(&subject.show, subject.season + 40)
                .await;
            assert!(
                matches!(missing_season, Err(EnrichmentError::NotFound)),
                "{missing_season:?}"
            );
        }

        #[tokio::test]
        async fn an_id_of_another_provider_is_not_configured() {
            let subject: $crate::providers::contract::ContractSubject = $setup().await;
            let provider = &subject.provider;
            let foreign = foreign_ref();
            let movie = provider.movie_enrichment(&foreign).await;
            assert!(
                matches!(movie, Err(EnrichmentError::NotConfigured)),
                "{movie:?}"
            );
            let show = provider.show_enrichment(&foreign).await;
            assert!(
                matches!(show, Err(EnrichmentError::NotConfigured)),
                "{show:?}"
            );
            let season = provider.season_enrichment(&foreign, 1).await;
            assert!(
                matches!(season, Err(EnrichmentError::NotConfigured)),
                "{season:?}"
            );
        }

        #[tokio::test]
        async fn a_search_finds_a_title_it_knows_and_nothing_else() {
            let subject: $crate::providers::contract::ContractSubject = $setup().await;
            let provider = &subject.provider;
            let movies = provider
                .search_movies(&query(subject.movie_title))
                .await
                .unwrap();
            assert!(
                movies.iter().any(
                    |hit| hit.external_ref == subject.movie && hit.title == subject.movie_title
                ),
                "{movies:?}"
            );
            let shows = provider
                .search_shows(&query(subject.show_title))
                .await
                .unwrap();
            assert!(
                shows
                    .iter()
                    .any(|hit| hit.external_ref == subject.show && hit.title == subject.show_title),
                "{shows:?}"
            );
            assert!(
                provider
                    .search_movies(&query(UNKNOWN_TITLE))
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                provider
                    .search_shows(&query(UNKNOWN_TITLE))
                    .await
                    .unwrap()
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn invalidating_an_id_never_loses_a_title() {
            let subject: $crate::providers::contract::ContractSubject = $setup().await;
            let provider = &subject.provider;
            provider.invalidate(&subject.show).await;
            provider.invalidate(&subject.movie).await;
            provider.invalidate(&foreign_ref()).await;
            assert_eq!(
                provider
                    .movie_enrichment(&subject.movie)
                    .await
                    .unwrap()
                    .title,
                subject.movie_title
            );
            assert_eq!(
                provider
                    .season_enrichment(&subject.show, subject.season)
                    .await
                    .unwrap()
                    .season_number,
                subject.season
            );
        }
    };
}

pub(crate) use enrichment_provider_contract;

/// The in-memory double the rest of the suite runs on.
mod in_memory {
    use super::*;
    use beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider;
    use beam_domain::providers::enrichment::{
        MovieEnrichment, MovieSearchHit, SeasonEnrichment, ShowEnrichment, ShowSearchHit,
    };

    /// A double named `name` knowing one movie, one show and its first
    /// season.
    pub(crate) fn knowing(name: &'static str) -> InMemoryEnrichmentProvider {
        let movie = ExternalMediaRef::new(name, "11");
        let show = ExternalMediaRef::new(name, "22");
        InMemoryEnrichmentProvider::new(&[name])
            .with_movie_search(
                "Contract Movie",
                vec![MovieSearchHit {
                    external_ref: movie.clone(),
                    title: "Contract Movie".to_string(),
                    original_title: None,
                    year: None,
                    popularity: None,
                    vote_average: None,
                }],
            )
            .with_show_search(
                "Contract Show",
                vec![ShowSearchHit {
                    external_ref: show.clone(),
                    title: "Contract Show".to_string(),
                    original_title: None,
                    year: None,
                    popularity: None,
                    vote_average: None,
                }],
            )
            .with_movie_enrichment_at(
                movie,
                MovieEnrichment {
                    title: "Contract Movie".to_string(),
                    ..Default::default()
                },
            )
            .with_show_enrichment(
                show.clone(),
                ShowEnrichment {
                    title: "Contract Show".to_string(),
                    ..Default::default()
                },
            )
            .with_season_enrichment(
                show,
                SeasonEnrichment {
                    season_number: 1,
                    ..Default::default()
                },
            )
    }

    /// The subject [`knowing`] makes, under `name`.
    pub(crate) fn subject(
        provider: Arc<dyn EnrichmentProvider>,
        name: &'static str,
    ) -> ContractSubject {
        ContractSubject {
            provider,
            name,
            movie: ExternalMediaRef::new(name, "11"),
            movie_title: "Contract Movie",
            show: ExternalMediaRef::new(name, "22"),
            show_title: "Contract Show",
            season: 1,
            unknown: ExternalMediaRef::new(name, "99"),
        }
    }

    async fn setup() -> ContractSubject {
        subject(Arc::new(knowing("tmdb")), "tmdb")
    }

    enrichment_provider_contract!(setup);
}

pub(crate) use in_memory::{knowing, subject};
