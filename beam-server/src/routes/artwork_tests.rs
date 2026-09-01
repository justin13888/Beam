//! Subcutaneous tests for `/v1/artwork/{kind}/{id}/{variant}`.
//!
//! Driven through Kynos's in-process `TestClient` over a real router: real
//! handler, real `Served` conditional-request engine, a real cache writing real
//! files into a `TempDir`, and a fake only at the one boundary that is a
//! network -- the provider fetch. No Postgres, no Docker, no listener.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use beam_auth::utils::{models::CreateUser, session_store::SessionData};
    use beam_domain::models::{CreateMovie, CreateShow};
    use beam_domain::providers::artwork::test_utils::InMemoryArtworkFetcher;
    use beam_domain::providers::artwork::{ArtworkFetchError, ImageFormat};
    use beam_domain::providers::enrichment::MovieEnrichment;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use beam_domain::repositories::{MovieRepository, ShowRepository};
    use beam_domain::services::RealClock;
    use kynos::http::StatusCode;
    use kynos::prelude::*;
    use kynos::test::TestClient;
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::routes::artwork::{get_artwork, head_artwork};
    use crate::routes::test_support::make_app_state;
    use crate::services::artwork::{ArtworkCache, ArtworkCacheConfig};
    use crate::state::{AppServices, AppState};

    const POSTER_URL: &str = "https://image.tmdb.org/t/p/w500/poster.jpg";
    const POSTER_BYTES: &[u8] = b"\xff\xd8\xff-not-really-a-jpeg";

    struct Fixture {
        state: AppState,
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
        fetcher: Arc<InMemoryArtworkFetcher>,
        _root: TempDir,
    }

    fn fixture(fetcher: InMemoryArtworkFetcher) -> Fixture {
        let base = make_app_state();
        let root = TempDir::new().expect("temp dir");
        let fetcher = Arc::new(fetcher);
        let movies = Arc::new(InMemoryMovieRepository::default());
        let shows = Arc::new(InMemoryShowRepository::default());

        let artwork = Arc::new(ArtworkCache::new(
            ArtworkCacheConfig {
                root: root.path().join("artwork"),
                max_bytes: 1_000_000,
                negative_ttl: std::time::Duration::from_secs(300),
            },
            fetcher.clone(),
            Arc::new(RealClock),
        ));

        let services = AppServices {
            hash: base.services.hash.clone(),
            library: base.services.library.clone(),
            metadata: base.services.metadata.clone(),
            notification: base.services.notification.clone(),
            admin_log: base.services.admin_log.clone(),
            user_repo: base.services.user_repo.clone(),
            playback: base.services.playback.clone(),
            genre_repo: base.services.genre_repo.clone(),
            library_repo: base.services.library_repo.clone(),
            file_repo: base.services.file_repo.clone(),
            enrichment_repo: base.services.enrichment_repo.clone(),
            movie_repo: movies.clone(),
            show_repo: shows.clone(),
            artwork,
            session_store: base.services.session_store.clone(),
            oidc_client: base.services.oidc_client.clone(),
            pending_auth_store: base.services.pending_auth_store.clone(),
            oidc_config: base.services.oidc_config.clone(),
        };

        Fixture {
            state: AppState::new(base.config.clone(), services, base.probe.clone(), None),
            movies,
            shows,
            fetcher,
            _root: root,
        }
    }

    fn client(fixture: &Fixture) -> TestClient<AppState> {
        let service = Router::new()
            .nest(
                "/v1",
                Router::new().mount(kynos::routes![get_artwork, head_artwork]),
            )
            .build(fixture.state.clone())
            .expect("the artwork router describes itself");

        TestClient::new(service)
    }

    async fn signed_in(fixture: &Fixture) -> String {
        let user = fixture
            .state
            .services
            .user_repo
            .create(CreateUser {
                oidc_issuer: "https://test.example".to_string(),
                oidc_subject: "subj-1".to_string(),
                email: Some("viewer@example.com".to_string()),
                display_name: "Viewer".to_string(),
                avatar_url: None,
                is_admin: false,
            })
            .await
            .expect("user is created");

        fixture
            .state
            .services
            .session_store
            .create(
                &SessionData {
                    user_id: user.id.to_string(),
                    device_hash: "test-device".to_string(),
                    ip: "127.0.0.1".to_string(),
                    created_at: chrono::Utc::now().timestamp(),
                    last_active: chrono::Utc::now().timestamp(),
                },
                86400,
                86400,
            )
            .await
            .expect("session is created")
    }

    /// A movie whose enrichment left `poster_url` pointing at the provider.
    async fn movie_with_poster(fixture: &Fixture, poster_url: Option<&str>) -> Uuid {
        let movie = fixture
            .movies
            .create(CreateMovie {
                title: "Arrival".to_string(),
                year: Some(2016),
                runtime: None,
            })
            .await
            .expect("movie is created");

        fixture
            .movies
            .apply_enrichment(
                movie.id,
                &MovieEnrichment {
                    title: "Arrival".to_string(),
                    poster_url: poster_url.map(str::to_string),
                    ..Default::default()
                },
            )
            .await
            .expect("enrichment applies");

        movie.id
    }

    #[tokio::test]
    async fn a_poster_is_served_by_beam_rather_than_the_provider() {
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            POSTER_URL,
            ImageFormat::Jpeg,
            POSTER_BYTES,
        ));
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        let response = client
            .get(&format!("/v1/artwork/movie/{id}/poster"))
            .cookie("beam_session", &token)
            .send()
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.header("content-type"), Some("image/jpeg"));
        assert_eq!(response.bytes().as_ref(), POSTER_BYTES);
        assert!(
            response.header("etag").is_some(),
            "artwork must carry a validator so clients can revalidate",
        );

        // A second request is the whole point: the provider is asked once
        // however many viewers render the grid.
        let again = client
            .get(&format!("/v1/artwork/movie/{id}/poster"))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(fixture.fetcher.call_count(), 1);
    }

    #[tokio::test]
    async fn a_client_holding_the_current_etag_gets_a_304() {
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            POSTER_URL,
            ImageFormat::Jpeg,
            POSTER_BYTES,
        ));
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;
        let path = format!("/v1/artwork/movie/{id}/poster");

        let first = client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        let etag = first.header("etag").expect("a validator").to_string();

        let revalidated = client
            .get(&path)
            .cookie("beam_session", &token)
            .header("if-none-match", &etag)
            .send()
            .await;

        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
        assert!(
            revalidated.bytes().is_empty(),
            "a 304 carries no representation",
        );
    }

    /// Re-enrichment pointing the title elsewhere must change the validator,
    /// or a client holding the old one would never see the new art.
    #[tokio::test]
    async fn the_validator_changes_when_the_title_points_at_new_art() {
        const REFRESHED: &str = "https://image.tmdb.org/t/p/w500/refreshed.jpg";
        let fixture = fixture(
            InMemoryArtworkFetcher::new()
                .with_image(POSTER_URL, ImageFormat::Jpeg, POSTER_BYTES)
                .with_image(REFRESHED, ImageFormat::Png, b"the-new-art"),
        );
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;
        let path = format!("/v1/artwork/movie/{id}/poster");

        let before = client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        let stale_etag = before.header("etag").expect("a validator").to_string();

        // What an enrichment sweep does.
        fixture
            .movies
            .apply_enrichment(
                id,
                &MovieEnrichment {
                    title: "Arrival".to_string(),
                    poster_url: Some(REFRESHED.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("enrichment applies");

        let after = client
            .get(&path)
            .cookie("beam_session", &token)
            .header("if-none-match", &stale_etag)
            .send()
            .await;

        assert_eq!(
            after.status(),
            StatusCode::OK,
            "the old validator must not satisfy the new artwork",
        );
        assert_eq!(after.bytes().as_ref(), b"the-new-art");
        assert_eq!(after.header("content-type"), Some("image/png"));
        assert_ne!(after.header("etag"), Some(stale_etag.as_str()));
    }

    #[tokio::test]
    async fn an_episode_thumbnail_is_served_from_its_own_row() {
        const STILL: &str = "https://image.tmdb.org/t/p/w300/still.jpg";
        let fixture =
            fixture(InMemoryArtworkFetcher::new().with_image(STILL, ImageFormat::Jpeg, b"still"));

        let show = fixture
            .shows
            .create(CreateShow {
                title: "Severance".to_string(),
                year: Some(2022),
            })
            .await
            .expect("show is created");
        let season = fixture
            .shows
            .find_or_create_season(show.id, 1)
            .await
            .expect("season is created");
        let episode = fixture
            .shows
            .create_episode(beam_domain::models::CreateEpisode {
                season_id: season.id,
                episode_number: 1,
                title: "Good News About Hell".to_string(),
                runtime: None,
            })
            .await
            .expect("episode is created");
        fixture
            .shows
            .apply_season_enrichment(
                show.id,
                &beam_domain::providers::enrichment::SeasonEnrichment {
                    season_number: 1,
                    episodes: vec![beam_domain::providers::enrichment::EpisodeEnrichment {
                        episode_number: 1,
                        thumbnail_url: Some(STILL.to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
            .await
            .expect("season enrichment applies");

        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        let response = client
            .get(&format!("/v1/artwork/episode/{}/thumbnail", episode.id))
            .cookie("beam_session", &token)
            .send()
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().as_ref(), b"still");
    }

    /// Every "there is no image here" case is one answer, because every client
    /// already renders a placeholder for it.
    #[tokio::test]
    async fn everything_without_an_image_is_a_404() {
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            POSTER_URL,
            ImageFormat::Jpeg,
            POSTER_BYTES,
        ));
        let un_enriched = movie_with_poster(&fixture, None).await;
        let with_art = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        for (case, path) in [
            (
                "a title that has no artwork yet",
                format!("/v1/artwork/movie/{un_enriched}/poster"),
            ),
            (
                "an id that does not exist",
                format!("/v1/artwork/movie/{}/poster", Uuid::new_v4()),
            ),
            (
                "an id that is not an id at all",
                "/v1/artwork/movie/not-a-uuid/poster".to_string(),
            ),
            (
                "a variant this kind does not have",
                format!("/v1/artwork/movie/{with_art}/thumbnail"),
            ),
            (
                "a season backdrop, which Beam does not store",
                format!("/v1/artwork/season/{}/backdrop", Uuid::new_v4()),
            ),
        ] {
            let response = client
                .get(&path)
                .cookie("beam_session", &token)
                .send()
                .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{case}");
        }
    }

    /// A provider that has dropped the image degrades to the same placeholder
    /// as a title with no art, rather than to a 500.
    #[tokio::test]
    async fn art_the_provider_no_longer_has_is_a_404() {
        let fixture = fixture(
            InMemoryArtworkFetcher::new().with_error(POSTER_URL, ArtworkFetchError::NotFound),
        );
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        let response = client
            .get(&format!("/v1/artwork/movie/{id}/poster"))
            .cookie("beam_session", &token)
            .send()
            .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn artwork_requires_a_session() {
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            POSTER_URL,
            ImageFormat::Jpeg,
            POSTER_BYTES,
        ));
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;

        let response = client(&fixture)
            .get(&format!("/v1/artwork/movie/{id}/poster"))
            .send()
            .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            fixture.fetcher.call_count(),
            0,
            "an unauthenticated request must not reach the provider",
        );
    }

    #[tokio::test]
    async fn head_describes_the_image_without_sending_it() {
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            POSTER_URL,
            ImageFormat::Jpeg,
            POSTER_BYTES,
        ));
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        let response = client
            .head(&format!("/v1/artwork/movie/{id}/poster"))
            .cookie("beam_session", &token)
            .send()
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.header("content-type"), Some("image/jpeg"));
        assert!(response.bytes().is_empty(), "a HEAD carries no body");
    }
}
