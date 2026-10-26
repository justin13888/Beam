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
    use beam_domain::models::person::CreditRole;
    use beam_domain::models::{CreateMovie, CreateShow};
    use beam_domain::providers::artwork::test_utils::InMemoryArtworkFetcher;
    use beam_domain::providers::artwork::{ArtworkFetchError, ImageFormat};
    use beam_domain::providers::enrichment::{
        CollectionEnrichment, CreditEnrichment, ExternalMediaRef, MovieEnrichment,
    };
    use beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::person::in_memory::InMemoryPersonRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use beam_domain::repositories::{
        CollectionRepository, MovieRepository, PersonRepository, ShowRepository,
    };
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
        people: Arc<InMemoryPersonRepository>,
        collections: Arc<InMemoryCollectionRepository>,
        fetcher: Arc<InMemoryArtworkFetcher>,
        _root: TempDir,
    }

    fn fixture(fetcher: InMemoryArtworkFetcher) -> Fixture {
        let base = make_app_state();
        let root = TempDir::new().expect("temp dir");
        let fetcher = Arc::new(fetcher);
        let movies = Arc::new(InMemoryMovieRepository::default());
        let shows = Arc::new(InMemoryShowRepository::default());
        let people = Arc::new(InMemoryPersonRepository::default());
        let collections = Arc::new(InMemoryCollectionRepository::default());

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
            access: base.services.access.clone(),
            library: base.services.library.clone(),
            metadata: base.services.metadata.clone(),
            subtitles: base.services.subtitles.clone(),
            notification: base.services.notification.clone(),
            admin_log: base.services.admin_log.clone(),
            user_repo: base.services.user_repo.clone(),
            language_preferences: base.services.language_preferences.clone(),
            playback: base.services.playback.clone(),
            genre_repo: base.services.genre_repo.clone(),
            library_repo: base.services.library_repo.clone(),
            file_repo: base.services.file_repo.clone(),
            enrichment_repo: base.services.enrichment_repo.clone(),
            enrichment_control: base.services.enrichment_control.clone(),
            people: Arc::new(crate::services::people::PeopleService::new(
                people.clone(),
                movies.clone(),
                shows.clone(),
            )),
            lists: base.services.lists.clone(),
            collections: Arc::new(crate::services::collections::CollectionService::new(
                collections.clone(),
                base.services.metadata.clone(),
                base.services.playback.clone(),
            )),
            movie_repo: movies.clone(),
            show_repo: shows.clone(),
            artwork,
            session_store: base.services.session_store.clone(),
            oidc_client: base.services.oidc_client.clone(),
            pending_auth_store: base.services.pending_auth_store.clone(),
            device_auth_store: base.services.device_auth_store.clone(),
            oidc_config: base.services.oidc_config.clone(),
            watch_status: Arc::new(beam_index::services::watch_status::WatchStatus::new()),
            telemetry: crate::routes::test_support::idle_library_report(),
            playback_telemetry: crate::routes::test_support::idle_playback_telemetry(),
            trickplay: crate::routes::test_support::idle_trickplay(),
        };

        Fixture {
            state: AppState::new(base.config.clone(), services, base.probe.clone(), None),
            movies,
            shows,
            people,
            collections,
            fetcher,
            _root: root,
        }
    }

    /// A person credited on a movie, with `portrait` as their provider
    /// portrait, by their id.
    async fn credited_person(fixture: &Fixture, native: &str, portrait: Option<&str>) -> Uuid {
        let movie = movie_with_poster(fixture, None).await;
        fixture
            .people
            .set_movie_credits(
                movie,
                &[CreditEnrichment {
                    person_ref: ExternalMediaRef::new("tmdb", native),
                    name: format!("Person {native}"),
                    profile_url: portrait.map(str::to_string),
                    role: CreditRole::Cast {
                        character: None,
                        billing_order: 0,
                    },
                }],
            )
            .await
            .expect("credits are stored");
        fixture
            .people
            .movie_credits(movie)
            .await
            .expect("credits are read")
            .into_iter()
            .find(|credit| credit.person.external_ref == format!("tmdb:{native}"))
            .expect("the person is credited")
            .person
            .id
    }

    #[tokio::test]
    async fn a_persons_portrait_is_served_by_beam_and_nothing_else_of_theirs_is() {
        const PORTRAIT_URL: &str = "https://image.tmdb.org/t/p/h632/portrait.jpg";
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            PORTRAIT_URL,
            ImageFormat::Jpeg,
            POSTER_BYTES,
        ));
        let pictured = credited_person(&fixture, "1", Some(PORTRAIT_URL)).await;
        let unpictured = credited_person(&fixture, "2", None).await;
        let movie = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        // Pictured, then credited on nothing: their row stays, their
        // portrait is no longer served.
        let uncredited = credited_person(&fixture, "3", Some(PORTRAIT_URL)).await;
        let last_title = fixture
            .people
            .filmography(uncredited)
            .await
            .expect("the filmography is read")[0]
            .title_id;
        fixture
            .people
            .set_movie_credits(last_title, &[])
            .await
            .expect("credits are cleared");
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        let response = client
            .get(&format!("/v1/artwork/person/{pictured}/profile"))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().as_ref(), POSTER_BYTES);

        for (case, path) in [
            (
                "a person with no portrait",
                format!("/v1/artwork/person/{unpictured}/profile"),
            ),
            (
                "a person no title credits any more",
                format!("/v1/artwork/person/{uncredited}/profile"),
            ),
            (
                "a person who does not exist",
                format!("/v1/artwork/person/{}/profile", Uuid::new_v4()),
            ),
            (
                "a person's poster, which Beam does not store",
                format!("/v1/artwork/person/{pictured}/poster"),
            ),
            (
                "a movie's profile, which no title has",
                format!("/v1/artwork/movie/{movie}/profile"),
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

    /// A collection holding one movie, with `poster` as its provider
    /// poster, by its id.
    async fn collection_of_one(fixture: &Fixture, native: &str, poster: Option<&str>) -> Uuid {
        let movie = movie_with_poster(fixture, None).await;
        fixture
            .collections
            .set_movie_collection(
                movie,
                Some(&CollectionEnrichment {
                    collection_ref: ExternalMediaRef::new("tmdb", native),
                    name: format!("Collection {native}"),
                    poster_url: poster.map(str::to_string),
                    backdrop_url: None,
                }),
            )
            .await
            .expect("the collection is stored");
        fixture
            .collections
            .movie_collection(movie)
            .await
            .expect("the collection is read")
            .expect("the movie is in it")
            .id
    }

    #[tokio::test]
    async fn a_collections_poster_is_served_by_beam_while_a_movie_is_in_it() {
        const COLLECTION_POSTER_URL: &str = "https://image.tmdb.org/t/p/w500/collection.jpg";
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            COLLECTION_POSTER_URL,
            ImageFormat::Jpeg,
            POSTER_BYTES,
        ));
        let pictured = collection_of_one(&fixture, "1", Some(COLLECTION_POSTER_URL)).await;
        let unpictured = collection_of_one(&fixture, "2", None).await;
        // Pictured, then left by its only movie: no longer served.
        let abandoned = collection_of_one(&fixture, "3", Some(COLLECTION_POSTER_URL)).await;
        let leaver = fixture.collections.movie_ids(abandoned).await.unwrap()[0];
        fixture
            .collections
            .set_movie_collection(leaver, None)
            .await
            .expect("the movie leaves");
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        let response = client
            .get(&format!("/v1/artwork/collection/{pictured}/poster"))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().as_ref(), POSTER_BYTES);

        for (case, path) in [
            (
                "a collection with no poster",
                format!("/v1/artwork/collection/{unpictured}/poster"),
            ),
            (
                "a collection with no backdrop",
                format!("/v1/artwork/collection/{pictured}/backdrop"),
            ),
            (
                "a collection no movie is in any more",
                format!("/v1/artwork/collection/{abandoned}/poster"),
            ),
            (
                "a collection that does not exist",
                format!("/v1/artwork/collection/{}/poster", Uuid::new_v4()),
            ),
            (
                "a collection's thumbnail, which Beam does not store",
                format!("/v1/artwork/collection/{pictured}/thumbnail"),
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

    /// A movie of its own whose enrichment left `poster_url` pointing at the
    /// provider. Each call parses a different title, so each is a new movie.
    async fn movie_with_poster(fixture: &Fixture, poster_url: Option<&str>) -> Uuid {
        let movie = fixture
            .movies
            .find_or_create_by_identity(CreateMovie::new(
                format!("Arrival {}", Uuid::new_v4()),
                Some(2016),
                None,
            ))
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
                &beam_domain::models::enrichment::FieldLocks::none(),
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
                &beam_domain::models::enrichment::FieldLocks::none(),
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
            .find_or_create_by_identity(CreateShow::new("Severance".to_string(), Some(2022)))
            .await
            .expect("show is created");
        let season = fixture
            .shows
            .find_or_create_season(show.id, 1)
            .await
            .expect("season is created");
        let episode = fixture
            .shows
            .find_or_create_episode(beam_domain::models::CreateEpisode {
                season_id: season.id,
                episode_number: 1,
                title: "Good News About Hell".to_string(),
                runtime: None,
                air_date: None,
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

    /// The artwork paths `ShowMetadata` publishes are ones this route
    /// actually serves: a client following them gets the show's own poster
    /// and backdrop, not a 404.
    #[tokio::test]
    async fn a_shows_published_artwork_urls_resolve_to_its_own_art() {
        use crate::models::MediaMetadata;
        use crate::services::metadata::{DbMetadataService, MetadataService};
        use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
        use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;

        const SHOW_POSTER: &str = "https://image.tmdb.org/t/p/w500/show-poster.jpg";
        const SHOW_BACKDROP: &str = "https://image.tmdb.org/t/p/w1280/show-backdrop.jpg";
        let fixture = fixture(
            InMemoryArtworkFetcher::new()
                .with_image(SHOW_POSTER, ImageFormat::Jpeg, b"show-poster")
                .with_image(SHOW_BACKDROP, ImageFormat::Jpeg, b"show-backdrop"),
        );
        let show = fixture
            .shows
            .find_or_create_by_identity(CreateShow::new("Severance".to_string(), Some(2022)))
            .await
            .expect("show is created");
        fixture
            .shows
            .apply_enrichment(
                show.id,
                &beam_domain::providers::enrichment::ShowEnrichment {
                    title: "Severance".to_string(),
                    poster_url: Some(SHOW_POSTER.to_string()),
                    backdrop_url: Some(SHOW_BACKDROP.to_string()),
                    ..Default::default()
                },
                &beam_domain::models::enrichment::FieldLocks::none(),
            )
            .await
            .expect("enrichment applies");

        let genres = Arc::new(
            beam_domain::repositories::genre::in_memory::InMemoryGenreRepository::default(),
        );
        let metadata = DbMetadataService::new(crate::services::metadata::MetadataRepositories {
            movies: fixture.movies.clone(),
            shows: fixture.shows.clone(),
            sources: Arc::new(crate::services::sources::SourceCatalog::new(crate::services::sources::SourceRepositories {movies: fixture.movies.clone(), files: Arc::new(InMemoryFileRepository::default()), streams: Arc::new(InMemoryMediaStreamRepository::default()), sidecars: Arc::new(
                    beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository::default(),
                ), chapters: Arc::new(beam_domain::repositories::chapter::in_memory::InMemoryChapterRepository::default()), trickplay: Arc::new(beam_domain::repositories::trickplay::in_memory::InMemoryTrickplayRepository::default()),})),
            catalog: Arc::new(
                beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository::new(
                    fixture.movies.clone(),
                    fixture.shows.clone(),
                    genres.clone(),
                ),
            ),
            genres,
            people: Arc::new(
                beam_domain::repositories::person::in_memory::InMemoryPersonRepository::default(),
            ),
            collections: Arc::new(InMemoryCollectionRepository::default()),
        });
        let Some(MediaMetadata::Show(published)) = metadata
            .get_media_metadata(show.id)
            .await
            .expect("the lookup succeeds")
        else {
            panic!("the show resolves");
        };
        let poster_path = published.poster_url.expect("a poster path is published");
        let backdrop_path = published
            .backdrop_url
            .expect("a backdrop path is published");

        let client = client(&fixture);
        let token = signed_in(&fixture).await;
        for (path, expected) in [
            (poster_path, b"show-poster".as_slice()),
            (backdrop_path, b"show-backdrop".as_slice()),
        ] {
            let response = client
                .get(&path)
                .cookie("beam_session", &token)
                .send()
                .await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.bytes().as_ref(), expected, "{path}");
        }
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

    /// An id that is not a UUID is the caller's mistake, and it gets the same
    /// answer here as from the detail route, `/sources` and the admin refresh.
    /// This route used to fold it into the 404 above, so four routes over one
    /// identifier gave two answers to one condition.
    #[tokio::test]
    async fn a_malformed_id_is_a_400_like_every_other_media_id_route() {
        let fixture = fixture(InMemoryArtworkFetcher::new());
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        client
            .get("/v1/artwork/movie/not-a-uuid/poster")
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(
            fixture.fetcher.call_count(),
            0,
            "a request that names nothing must not reach the provider",
        );
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

    /// The provider answered and Beam could not use the answer. That is the
    /// provider's fault, and it is reported as one: a 502, not the `internal`
    /// that would send an operator looking for a bug in Beam.
    #[tokio::test]
    async fn a_provider_that_answers_unusably_is_a_502() {
        let fixture = fixture(
            InMemoryArtworkFetcher::new()
                .with_error(POSTER_URL, ArtworkFetchError::Upstream { status: 503 }),
        );
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        client
            .get(&format!("/v1/artwork/movie/{id}/poster"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_GATEWAY)
            .assert_problem_type(
                "https://beam.justinchung.net/reference/errors/#artwork-upstream-failed",
            );
    }

    /// A stored URL the fetcher refuses never produced a request, so nothing
    /// upstream can have failed: enrichment wrote bad data onto a row, and
    /// that is Beam's fault. Reporting it as a provider failure would tell an
    /// operator to wait for a CDN that was never asked anything.
    #[tokio::test]
    async fn a_stored_url_beam_refuses_to_fetch_is_beams_own_fault() {
        const PLAIN_HTTP: &str = "http://image.tmdb.org/t/p/w500/poster.jpg";
        let fixture = fixture(InMemoryArtworkFetcher::new().with_error(
            PLAIN_HTTP,
            ArtworkFetchError::Refused {
                url: PLAIN_HTTP.to_string(),
                reason: "not https".to_string(),
            },
        ));
        let id = movie_with_poster(&fixture, Some(PLAIN_HTTP)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        client
            .get(&format!("/v1/artwork/movie/{id}/poster"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
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

    /// A real JPEG of `width` by `height`, so a resize has pixels to work on.
    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let pixels = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode_image(&pixels)
            .expect("the fixture encodes");
        encoded
    }

    fn dimensions(bytes: &[u8]) -> (u32, u32) {
        let decoded = image::load_from_memory(bytes).expect("a decodable image");
        (decoded.width(), decoded.height())
    }

    #[tokio::test]
    async fn a_sized_poster_is_narrower_and_revalidates_under_its_own_etag() {
        let original = jpeg(1000, 1500);
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            POSTER_URL,
            ImageFormat::Jpeg,
            &original,
        ));
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;
        let full_path = format!("/v1/artwork/movie/{id}/poster");
        let sized_path = format!("/v1/artwork/movie/{id}/poster?size=w185");

        let full = client
            .get(&full_path)
            .cookie("beam_session", &token)
            .send()
            .await;
        let sized = client
            .get(&sized_path)
            .cookie("beam_session", &token)
            .send()
            .await;

        assert_eq!(full.status(), StatusCode::OK);
        assert_eq!(full.bytes().as_ref(), original.as_slice());
        assert_eq!(sized.status(), StatusCode::OK);
        assert_eq!(sized.header("content-type"), Some("image/jpeg"));
        assert_eq!(dimensions(sized.bytes().as_ref()), (185, 278));
        let full_etag = full.header("etag").expect("a validator").to_string();
        let sized_etag = sized.header("etag").expect("a validator").to_string();
        assert_ne!(
            full_etag, sized_etag,
            "each size is its own representation and needs its own validator",
        );
        assert_eq!(
            fixture.fetcher.call_count(),
            1,
            "a size is made from the cached original, not fetched again",
        );

        let revalidated = client
            .get(&sized_path)
            .cookie("beam_session", &token)
            .header("if-none-match", &sized_etag)
            .send()
            .await;
        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);

        // The original's validator does not stand for the rendition.
        let mismatched = client
            .get(&sized_path)
            .cookie("beam_session", &token)
            .header("if-none-match", &full_etag)
            .send()
            .await;
        assert_eq!(mismatched.status(), StatusCode::OK);
        assert_eq!(mismatched.bytes(), sized.bytes());
    }

    #[tokio::test]
    async fn a_size_that_is_not_on_the_ladder_is_refused_before_the_provider_is_asked() {
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            POSTER_URL,
            ImageFormat::Jpeg,
            &jpeg(1000, 1500),
        ));
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;

        for size in ["w200", "185", "original", ""] {
            let response = client
                .get(&format!("/v1/artwork/movie/{id}/poster?size={size}"))
                .cookie("beam_session", &token)
                .send()
                .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "size={size}");
        }
        assert_eq!(fixture.fetcher.call_count(), 0);
    }

    #[tokio::test]
    async fn a_sized_head_describes_the_rendition() {
        let fixture = fixture(InMemoryArtworkFetcher::new().with_image(
            POSTER_URL,
            ImageFormat::Jpeg,
            &jpeg(1000, 1500),
        ));
        let id = movie_with_poster(&fixture, Some(POSTER_URL)).await;
        let client = client(&fixture);
        let token = signed_in(&fixture).await;
        let path = format!("/v1/artwork/movie/{id}/poster?size=w342");

        let got = client
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        let head = client
            .head(&path)
            .cookie("beam_session", &token)
            .send()
            .await;

        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.header("etag"), got.header("etag"));
        assert_eq!(
            head.header("content-length"),
            Some(got.bytes().len().to_string().as_str()),
        );
    }
}
