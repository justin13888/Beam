//! Browse and search tests for DbMetadataService, split out of
//! `metadata_tests.rs`. Like those, they use in-memory repository fakes and
//! need no external infrastructure.

/// Sources over empty stores, for a service whose test reads none.
#[cfg(test)]
fn empty_sources() -> std::sync::Arc<crate::services::sources::SourceCatalog> {
    use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository;
    use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
    use std::sync::Arc;

    Arc::new(crate::services::sources::SourceCatalog::new(crate::services::sources::SourceRepositories {movies: Arc::new(InMemoryMovieRepository::default()), files: Arc::new(InMemoryFileRepository::default()), streams: Arc::new(InMemoryMediaStreamRepository::default()), sidecars: Arc::new(InMemorySidecarSubtitleRepository::default()), chapters: Arc::new(beam_domain::repositories::chapter::in_memory::InMemoryChapterRepository::default()), trickplay: Arc::new(beam_domain::repositories::trickplay::in_memory::InMemoryTrickplayRepository::default()),}))
}

/// Browse and search through the service (issue #187): the page request rules,
/// cursors, hydration of genres, ratings and counts, and that a failing store
/// is an error rather than an empty page or a miss.
#[cfg(test)]
mod browse {
    use std::sync::Arc;

    use beam_domain::models::catalog::{CatalogPosition, SortKey, TitleKind};
    use beam_domain::models::{CreateEpisode, CreateMovie, CreateShow};
    use beam_domain::providers::enrichment::ShowEnrichment;
    use beam_domain::repositories::catalog::MockCatalogRepository;
    use beam_domain::repositories::catalog::in_memory::InMemoryCatalogRepository;
    use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
    use beam_domain::repositories::movie::MockMovieRepository;
    use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
    use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
    use beam_domain::repositories::{
        CatalogRepository, GenreRepository, MovieRepository, ShowRepository,
    };
    use proptest::prelude::*;
    use sea_orm::DbErr;
    use uuid::Uuid;

    use crate::models::MediaMetadata;
    use crate::models::search::{MediaConnection, MediaSortField, SortOrder};
    use crate::services::cursor::browse;
    use crate::services::metadata::{
        BrowseRequest, DEFAULT_PAGE_SIZE, DbMetadataService, MAX_PAGE_SIZE, MediaSearchFilters,
        MetadataError, MetadataRepositories, MetadataService,
    };
    use crate::services::paging::{PageDirection, PageRequest};

    /// A browse page request, at browse's bounds.
    fn browse_page(
        first: Option<u32>,
        after: Option<String>,
        last: Option<u32>,
        before: Option<String>,
    ) -> Result<PageRequest, String> {
        PageRequest::from_relay(
            first,
            after,
            last,
            before,
            DEFAULT_PAGE_SIZE,
            MAX_PAGE_SIZE,
            "items",
        )
    }

    /// Every double, with the catalogue and genre store reading the title
    /// doubles, so a test writes through one trait and reads through another.
    struct Library {
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
        genres: Arc<InMemoryGenreRepository>,
        people: Arc<beam_domain::repositories::person::in_memory::InMemoryPersonRepository>,
    }

    impl Library {
        fn new() -> Self {
            Self {
                movies: Arc::default(),
                shows: Arc::default(),
                genres: Arc::default(),
                people: Arc::default(),
            }
        }

        fn service(&self) -> DbMetadataService {
            self.service_over(Arc::new(InMemoryCatalogRepository::new(
                self.movies.clone(),
                self.shows.clone(),
                self.genres.clone(),
            )))
        }

        fn service_over(&self, catalog: Arc<dyn CatalogRepository>) -> DbMetadataService {
            DbMetadataService::new(MetadataRepositories {
                movies: self.movies.clone(),
                shows: self.shows.clone(),
                sources: super::empty_sources(),
                catalog,
                genres: self.genres.clone(),
                people: self.people.clone(),
                collections: Arc::new(beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository::default()),
            })
        }

        async fn movie(&self, title: &str) -> Uuid {
            self.movies
                .find_or_create_by_identity(CreateMovie::new(title, None, None))
                .await
                .unwrap()
                .id
        }

        /// A rated, identified show with genres, two seasons and three
        /// episodes.
        async fn full_show(&self, title: &str) -> Uuid {
            let show = self
                .shows
                .find_or_create_by_identity(CreateShow::new(title, Some(2022)))
                .await
                .unwrap();
            self.shows
                .apply_enrichment(
                    show.id,
                    &ShowEnrichment {
                        title: title.to_string(),
                        year: Some(2022),
                        tmdb_id: Some(95396),
                        imdb_id: Some("tt11280740".to_string()),
                        rating: Some(8.4),
                        ..Default::default()
                    },
                    &beam_domain::models::enrichment::FieldLocks::none(),
                )
                .await
                .unwrap();
            for (season_number, episodes) in [(1, 2), (2, 1)] {
                let season = self
                    .shows
                    .find_or_create_season(show.id, season_number)
                    .await
                    .unwrap();
                for episode_number in 1..=episodes {
                    self.shows
                        .find_or_create_episode(CreateEpisode {
                            season_id: season.id,
                            episode_number,
                            title: format!("Episode {episode_number}"),
                            runtime: None,
                            air_date: None,
                        })
                        .await
                        .unwrap();
                }
            }
            self.genres
                .set_show_genres(show.id, &["Thriller".to_string(), "drama".to_string()])
                .await
                .unwrap();
            show.id
        }
    }

    fn request(first: Option<u32>, after: Option<String>) -> BrowseRequest {
        BrowseRequest {
            first,
            after,
            last: None,
            before: None,
            sort_by: MediaSortField::Title,
            sort_order: SortOrder::Asc,
            filters: MediaSearchFilters::default(),
            access: beam_domain::models::access::AccessPolicy::unrestricted(),
        }
    }

    fn backward(last: Option<u32>, before: Option<String>) -> BrowseRequest {
        BrowseRequest {
            first: None,
            after: None,
            last,
            before,
            ..request(None, None)
        }
    }

    fn titles(connection: &MediaConnection) -> Vec<String> {
        connection
            .items
            .iter()
            .map(|item| item.title().original.clone())
            .collect()
    }

    #[test]
    fn page_requests_page_one_way_within_the_size_bounds() {
        let c = || Some("c".to_string());
        let ok = |direction, size: u32, cursor: Option<String>| {
            Ok::<_, ()>(PageRequest {
                direction,
                size: std::num::NonZeroU32::new(size).unwrap(),
                cursor,
            })
        };
        /// `first`, `after`, `last`, `before`, and the page they ask for.
        type Case = (
            Option<u32>,
            Option<String>,
            Option<u32>,
            Option<String>,
            Result<PageRequest, ()>,
        );
        let cases: Vec<Case> = vec![
            (
                None,
                None,
                None,
                None,
                ok(PageDirection::Forward, DEFAULT_PAGE_SIZE, None),
            ),
            (Some(5), c(), None, None, ok(PageDirection::Forward, 5, c())),
            (
                None,
                c(),
                None,
                None,
                ok(PageDirection::Forward, DEFAULT_PAGE_SIZE, c()),
            ),
            (
                None,
                None,
                Some(3),
                None,
                ok(PageDirection::Backward, 3, None),
            ),
            (
                None,
                None,
                None,
                c(),
                ok(PageDirection::Backward, DEFAULT_PAGE_SIZE, c()),
            ),
            (
                None,
                None,
                Some(1),
                c(),
                ok(PageDirection::Backward, 1, c()),
            ),
            (
                Some(MAX_PAGE_SIZE),
                None,
                None,
                None,
                ok(PageDirection::Forward, MAX_PAGE_SIZE, None),
            ),
            (Some(MAX_PAGE_SIZE + 1), None, None, None, Err(())),
            (None, None, Some(MAX_PAGE_SIZE + 1), None, Err(())),
            (Some(0), None, None, None, Err(())),
            (None, None, Some(0), None, Err(())),
            (Some(5), None, Some(5), None, Err(())),
            (None, c(), None, c(), Err(())),
            (Some(5), None, None, c(), Err(())),
            (None, c(), Some(5), None, Err(())),
        ];
        for (first, after, last, before, expected) in cases {
            let got = browse_page(first, after.clone(), last, before.clone());
            match (&got, &expected) {
                (Ok(got), Ok(expected)) => assert_eq!(got, expected),
                (Err(_), Err(())) => {}
                _ => panic!(
                    "{first:?} {after:?} {last:?} {before:?}: got {got:?}, want {expected:?}"
                ),
            }
        }
    }

    proptest! {
        /// Any accepted request pages the way its parameters name, at a size
        /// the server bounds; nothing panics.
        #[test]
        fn an_accepted_page_request_is_bounded_and_one_directional(
            first in proptest::option::of(any::<u32>()),
            after in proptest::option::of(".{0,4}"),
            last in proptest::option::of(any::<u32>()),
            before in proptest::option::of(".{0,4}"),
        ) {
            if let Ok(page) = browse_page(first, after.clone(), last, before.clone()) {
                prop_assert!(page.size.get() <= MAX_PAGE_SIZE);
                match page.direction {
                    PageDirection::Forward => {
                        prop_assert!(last.is_none() && before.is_none());
                        prop_assert_eq!(page.cursor, after);
                    }
                    PageDirection::Backward => {
                        prop_assert!(first.is_none() && after.is_none());
                        prop_assert_eq!(page.cursor, before);
                    }
                }
            }
        }
    }

    /// A detail lists the title's cast in billing order and its crew by
    /// department (issue #261); a browse result, which reads no credits,
    /// carries neither.
    #[tokio::test]
    async fn a_detail_lists_cast_and_crew_and_a_browse_result_does_not() {
        use beam_domain::models::person::CreditRole;
        use beam_domain::providers::enrichment::{CreditEnrichment, ExternalMediaRef};
        use beam_domain::repositories::PersonRepository;

        let credit = |native: &str, name: &str, role: CreditRole| CreditEnrichment {
            person_ref: ExternalMediaRef::new("tmdb", native),
            name: name.to_string(),
            profile_url: None,
            role,
        };
        let cast = |billing_order: u32, character: &str| CreditRole::Cast {
            character: Some(character.to_string()),
            billing_order,
        };
        let library = Library::new();
        let movie = library.movie("Heat").await;
        let show = library.full_show("Severance").await;
        library
            .people
            .set_movie_credits(
                movie,
                &[
                    credit(
                        "1",
                        "Michael Mann",
                        CreditRole::Crew {
                            job: "Director".to_string(),
                            department: Some("Directing".to_string()),
                        },
                    ),
                    credit("2", "Robert De Niro", cast(1, "Neil McCauley")),
                    credit("3", "Al Pacino", cast(0, "Vincent Hanna")),
                ],
            )
            .await
            .unwrap();
        library
            .people
            .set_show_credits(show, &[credit("4", "Adam Scott", cast(0, "Mark Scout"))])
            .await
            .unwrap();
        let service = library.service();

        let Some(MediaMetadata::Movie(detail)) = service.get_media_metadata(movie).await.unwrap()
        else {
            panic!("the movie resolves");
        };
        let cast_list = detail.cast.expect("a detail carries its cast");
        let names: Vec<&str> = cast_list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Al Pacino", "Robert De Niro"]);
        assert_eq!(cast_list[0].character.as_deref(), Some("Vincent Hanna"));
        let crew = detail.crew.expect("a detail carries its crew");
        assert_eq!(crew.len(), 1);
        assert_eq!(crew[0].name, "Michael Mann");
        assert_eq!(crew[0].job, "Director");

        let Some(MediaMetadata::Show(detail)) = service.get_media_metadata(show).await.unwrap()
        else {
            panic!("the show resolves");
        };
        let show_cast = detail.cast.expect("a show's detail carries its cast");
        assert_eq!(show_cast.len(), 1);
        assert_eq!(show_cast[0].name, "Adam Scott");
        assert_eq!(detail.crew.map(|crew| crew.len()), Some(0));

        let page = service.search_media(request(None, None)).await.unwrap();
        assert_eq!(page.items.len(), 2);
        for item in &page.items {
            let (cast, crew) = match item {
                MediaMetadata::Movie(m) => (&m.cast, &m.crew),
                MediaMetadata::Show(s) => (&s.cast, &s.crew),
            };
            assert!(cast.is_none() && crew.is_none(), "browse reads no credits");
        }
    }

    #[tokio::test]
    async fn pages_forward_and_back_carry_their_neighbours_and_their_cursors() {
        let library = Library::new();
        for title in ["Alpha", "Bravo", "Charlie", "Delta", "Echo"] {
            library.movie(title).await;
        }
        let service = library.service();

        let one = service.search_media(request(Some(2), None)).await.unwrap();
        assert_eq!(titles(&one), ["Alpha", "Bravo"]);
        assert!(one.page_info.has_next_page && !one.page_info.has_previous_page);

        let two = service
            .search_media(request(Some(2), one.page_info.end_cursor.clone()))
            .await
            .unwrap();
        assert_eq!(titles(&two), ["Charlie", "Delta"]);
        assert!(two.page_info.has_next_page && two.page_info.has_previous_page);

        let three = service
            .search_media(request(Some(2), two.page_info.end_cursor.clone()))
            .await
            .unwrap();
        assert_eq!(titles(&three), ["Echo"]);
        assert!(!three.page_info.has_next_page && three.page_info.has_previous_page);

        let back = service
            .search_media(backward(Some(2), three.page_info.start_cursor.clone()))
            .await
            .unwrap();
        assert_eq!(
            titles(&back),
            ["Charlie", "Delta"],
            "a backward page is in display order"
        );
        assert!(back.page_info.has_next_page && back.page_info.has_previous_page);
        let front = service
            .search_media(backward(Some(2), back.page_info.start_cursor.clone()))
            .await
            .unwrap();
        assert_eq!(titles(&front), ["Alpha", "Bravo"]);
        assert!(front.page_info.has_next_page && !front.page_info.has_previous_page);

        let last_two = service.search_media(backward(Some(2), None)).await.unwrap();
        assert_eq!(titles(&last_two), ["Delta", "Echo"]);
        assert!(!last_two.page_info.has_next_page && last_two.page_info.has_previous_page);
    }

    #[tokio::test]
    async fn an_empty_listing_is_an_empty_page_with_no_cursors() {
        let page = Library::new()
            .service()
            .search_media(request(None, None))
            .await
            .unwrap();
        assert!(page.items.is_empty());
        assert!(!page.page_info.has_next_page && !page.page_info.has_previous_page);
        assert_eq!(page.page_info.start_cursor, None);
        assert_eq!(page.page_info.end_cursor, None);
    }

    #[tokio::test]
    async fn a_cursor_not_issued_for_this_sort_is_an_invalid_cursor() {
        let library = Library::new();
        library.movie("Alpha").await;
        library.movie("Bravo").await;
        let service = library.service();
        let page = service.search_media(request(Some(1), None)).await.unwrap();
        let cursor = page.page_info.end_cursor.expect("a cursor");

        let garbage = service
            .search_media(request(Some(1), Some("not-a-cursor".to_string())))
            .await;
        assert!(
            matches!(garbage, Err(MetadataError::InvalidCursor(_))),
            "{garbage:?}"
        );

        let resorted = service
            .search_media(BrowseRequest {
                sort_by: MediaSortField::Year,
                ..request(Some(1), Some(cursor))
            })
            .await;
        assert!(
            matches!(resorted, Err(MetadataError::InvalidCursor(_))),
            "{resorted:?}"
        );
    }

    #[tokio::test]
    async fn a_page_request_the_server_does_not_answer_is_invalid_pagination() {
        let service = Library::new().service();
        let mixed = service
            .search_media(BrowseRequest {
                last: Some(2),
                ..request(Some(2), None)
            })
            .await;
        assert!(
            matches!(mixed, Err(MetadataError::InvalidPagination(_))),
            "{mixed:?}"
        );
        let oversized = service
            .search_media(request(Some(MAX_PAGE_SIZE + 1), None))
            .await;
        assert!(
            matches!(oversized, Err(MetadataError::InvalidPagination(_))),
            "{oversized:?}"
        );
    }

    /// A NUL cannot be bound into a Postgres text parameter, so a search for
    /// one failed the statement and answered caller input with a 500. It is
    /// refused before the store is asked; any other text still searches.
    #[tokio::test]
    async fn a_search_text_holding_nul_is_refused_before_the_store() {
        for text in ["\0", "a\0b"] {
            let mut catalog = MockCatalogRepository::new();
            catalog.expect_browse().never();
            let result = Library::new()
                .service_over(Arc::new(catalog))
                .search_media(BrowseRequest {
                    filters: MediaSearchFilters {
                        query: Some(text.to_string()),
                        ..Default::default()
                    },
                    ..request(None, None)
                })
                .await;
            assert!(
                matches!(result, Err(MetadataError::InvalidSearchQuery(_))),
                "{text:?}: {result:?}"
            );
        }

        let library = Library::new();
        library.movie("Alpha").await;
        let page = library
            .service()
            .search_media(BrowseRequest {
                filters: MediaSearchFilters {
                    query: Some("alph".to_string()),
                    ..Default::default()
                },
                ..request(None, None)
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1, "{page:?}");
    }

    /// NFR-205: a database failure while browsing is an error the route turns
    /// into a 500 -- it used to become an empty page, which says the library
    /// has nothing.
    #[tokio::test]
    async fn a_failing_catalogue_is_an_internal_error_not_an_empty_page() {
        let mut catalog = MockCatalogRepository::new();
        catalog
            .expect_browse()
            .returning(|_| Err(DbErr::Custom("connection reset".to_string())));
        let result = Library::new()
            .service_over(Arc::new(catalog))
            .search_media(request(None, None))
            .await;
        assert!(
            matches!(result, Err(MetadataError::InternalError(_))),
            "{result:?}"
        );
    }

    /// NFR-205: a failing title read is an error, never "no such title" --
    /// that is what turned a database failure on the detail route into a 404.
    #[tokio::test]
    async fn a_failing_title_read_is_an_internal_error_not_a_miss() {
        let failing = || {
            let mut movies = MockMovieRepository::new();
            movies
                .expect_find_by_id()
                .returning(|_| Err(DbErr::Custom("connection reset".to_string())));
            movies
        };
        let service = |movies: MockMovieRepository| {
            let library = Library::new();
            DbMetadataService::new(MetadataRepositories {
                movies: Arc::new(movies),
                shows: library.shows.clone(),
                sources: super::empty_sources(),
                catalog: Arc::new(MockCatalogRepository::new()),
                genres: library.genres.clone(),
                people: library.people.clone(),
                collections: Arc::new(beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository::default()),
            })
        };
        let id = Uuid::new_v4();

        let detail = service(failing()).get_media_metadata(id).await;
        assert!(
            matches!(detail, Err(MetadataError::InternalError(_))),
            "{detail:?}"
        );
        let sources = service(failing()).get_media_sources(id).await;
        assert!(
            matches!(sources, Err(MetadataError::InternalError(_))),
            "{sources:?}"
        );
    }

    /// NFR-205: every read that hydrates a page is load-bearing. A failure
    /// in any one of them -- the movies, the shows, either kind's genres, the
    /// shows' counts -- fails the page as an internal error (a 500), never a
    /// page with those titles, genres or counts silently missing.
    #[tokio::test]
    async fn a_failing_hydration_read_fails_the_page_rather_than_thinning_it() {
        use beam_domain::repositories::genre::MockGenreRepository;
        use beam_domain::repositories::show::MockShowRepository;

        fn reset() -> DbErr {
            DbErr::Custom("connection reset".to_string())
        }

        #[derive(Debug, Clone, Copy)]
        enum Failing {
            Movies,
            Shows,
            MovieGenres,
            ShowGenres,
            ChildCounts,
        }

        for failing in [
            Failing::Movies,
            Failing::Shows,
            Failing::MovieGenres,
            Failing::ShowGenres,
            Failing::ChildCounts,
        ] {
            let library = Library::new();
            let movie = library.movie("Arrival").await;
            let show = library.full_show("Severance").await;
            let page = vec![
                CatalogPosition {
                    kind: TitleKind::Movie,
                    id: movie,
                    key: SortKey::Title("arrival".to_string()),
                },
                CatalogPosition {
                    kind: TitleKind::Show,
                    id: show,
                    key: SortKey::Title("severance".to_string()),
                },
            ];
            let mut catalog = MockCatalogRepository::new();
            catalog.expect_browse().returning(move |_| Ok(page.clone()));

            // Each read before the failing one answers from the doubles the
            // titles were written through; the failing one errs; nothing
            // after it may be asked (mockall refuses an unexpected call).
            let mut movies = MockMovieRepository::new();
            let mut shows = MockShowRepository::new();
            let mut genres = MockGenreRepository::new();
            let (movie_store, show_store, genre_store) = (
                library.movies.clone(),
                library.shows.clone(),
                library.genres.clone(),
            );
            match failing {
                Failing::Movies => {
                    movies.expect_find_by_ids().returning(|_| Err(reset()));
                }
                Failing::Shows
                | Failing::MovieGenres
                | Failing::ShowGenres
                | Failing::ChildCounts => {
                    let found = movie_store.find_by_ids(&[movie]).await.unwrap();
                    movies
                        .expect_find_by_ids()
                        .returning(move |_| Ok(found.clone()));
                }
            }
            match failing {
                Failing::Movies => {}
                Failing::Shows => {
                    shows.expect_find_by_ids().returning(|_| Err(reset()));
                }
                Failing::MovieGenres | Failing::ShowGenres | Failing::ChildCounts => {
                    let found = show_store.find_by_ids(&[show]).await.unwrap();
                    shows
                        .expect_find_by_ids()
                        .returning(move |_| Ok(found.clone()));
                }
            }
            match failing {
                Failing::Movies | Failing::Shows => {}
                Failing::MovieGenres => {
                    genres
                        .expect_movie_genre_names()
                        .returning(|_| Err(reset()));
                }
                Failing::ShowGenres | Failing::ChildCounts => {
                    let found = genre_store.movie_genre_names(&[movie]).await.unwrap();
                    genres
                        .expect_movie_genre_names()
                        .returning(move |_| Ok(found.clone()));
                }
            }
            match failing {
                Failing::Movies | Failing::Shows | Failing::MovieGenres => {}
                Failing::ShowGenres => {
                    genres.expect_show_genre_names().returning(|_| Err(reset()));
                }
                Failing::ChildCounts => {
                    let found = genre_store.show_genre_names(&[show]).await.unwrap();
                    genres
                        .expect_show_genre_names()
                        .returning(move |_| Ok(found.clone()));
                    shows.expect_child_counts().returning(|_| Err(reset()));
                }
            }

            let result = DbMetadataService::new(MetadataRepositories {
                movies: Arc::new(movies),
                shows: Arc::new(shows),
                sources: super::empty_sources(),
                catalog: Arc::new(catalog),
                genres: Arc::new(genres),
                people: Arc::new(
                    beam_domain::repositories::person::in_memory::InMemoryPersonRepository::default(
                    ),
                ),
                collections: Arc::new(beam_domain::repositories::collection::in_memory::InMemoryCollectionRepository::default()),
            })
            .search_media(request(None, None))
            .await;
            assert!(
                matches!(result, Err(MetadataError::InternalError(_))),
                "{failing:?} failing: {result:?}"
            );
        }
    }

    /// A title the page named but that has gone by the time it is read is
    /// left out; the page's cursors still mark where the page ended, so the
    /// next page starts after it.
    #[tokio::test]
    async fn a_title_gone_between_the_page_and_its_read_is_skipped() {
        let library = Library::new();
        let kept = library.movie("Kept").await;
        let gone = Uuid::new_v4();
        let positions = vec![
            CatalogPosition {
                kind: TitleKind::Movie,
                id: kept,
                key: SortKey::Title("kept".to_string()),
            },
            CatalogPosition {
                kind: TitleKind::Movie,
                id: gone,
                key: SortKey::Title("zz".to_string()),
            },
        ];
        let mut catalog = MockCatalogRepository::new();
        let returned = positions.clone();
        catalog
            .expect_browse()
            .returning(move |_| Ok(returned.clone()));

        let page = library
            .service_over(Arc::new(catalog))
            .search_media(request(None, None))
            .await
            .unwrap();

        assert_eq!(titles(&page), ["Kept"]);
        assert_eq!(
            page.page_info.end_cursor,
            Some(browse::encode(
                MediaSortField::Title,
                SortOrder::Asc,
                &positions[1]
            ))
        );
    }

    /// Issue #187: a browsed show carried no genres, ratings or identifiers,
    /// and zero seasons and episodes. It carries all of them now, and agrees
    /// with its own detail.
    #[tokio::test]
    async fn a_browsed_show_carries_genres_ratings_identifiers_and_counts() {
        let library = Library::new();
        let id = library.full_show("Severance").await;
        let service = library.service();

        let page = service
            .search_media(BrowseRequest {
                filters: MediaSearchFilters {
                    media_type: Some(crate::models::TitleKind::Show),
                    ..Default::default()
                },
                ..request(None, None)
            })
            .await
            .unwrap();
        let [MediaMetadata::Show(browsed)] = page.items.as_slice() else {
            panic!("one show: {page:?}");
        };
        let Some(MediaMetadata::Show(detail)) = service.get_media_metadata(id).await.unwrap()
        else {
            panic!("the show resolves");
        };

        for (view, show) in [("browse", browsed), ("detail", &detail)] {
            assert_eq!(show.genres, ["drama", "Thriller"], "{view}");
            assert_eq!(
                show.ratings.as_ref().and_then(|r| r.tmdb),
                Some(8.4),
                "{view}"
            );
            let identifiers = show.identifiers.as_ref().expect("identified");
            assert_eq!(identifiers.tmdb_id, Some(95396), "{view}");
            assert_eq!(identifiers.imdb_id.as_deref(), Some("tt11280740"), "{view}");
            assert_eq!((show.season_count, show.episode_count), (2, 3), "{view}");
        }
        assert!(
            browsed.seasons.is_empty(),
            "browse carries counts, not seasons"
        );
        assert_eq!(detail.seasons.len(), 2);
    }

    #[tokio::test]
    async fn a_movie_carries_its_genres_and_the_genre_filter_takes_a_name_or_a_slug() {
        let library = Library::new();
        let tagged = library.movie("Arrival").await;
        library.movie("Clue").await;
        library
            .genres
            .set_movie_genres(
                tagged,
                &["Science Fiction".to_string(), "Drama".to_string()],
            )
            .await
            .unwrap();
        let service = library.service();

        for genre in ["Science Fiction", "science fiction", "science-fiction"] {
            let page = service
                .search_media(BrowseRequest {
                    filters: MediaSearchFilters {
                        genre: Some(genre.to_string()),
                        ..Default::default()
                    },
                    ..request(None, None)
                })
                .await
                .unwrap();
            assert_eq!(titles(&page), ["Arrival"], "genre={genre}");
            let MediaMetadata::Movie(movie) = &page.items[0] else {
                panic!("a movie");
            };
            assert_eq!(movie.genres, ["Drama", "Science Fiction"]);
        }
        let Some(MediaMetadata::Movie(detail)) = service.get_media_metadata(tagged).await.unwrap()
        else {
            panic!("the movie resolves");
        };
        assert_eq!(detail.genres, ["Drama", "Science Fiction"]);
    }

    /// Every sort key reaches the store: sorting by rating orders by rating,
    /// by runtime by runtime -- not silently by title, as three of the five
    /// keys used to.
    #[tokio::test]
    async fn every_sort_key_orders_the_page_by_that_key() {
        use beam_domain::providers::enrichment::MovieEnrichment;

        let library = Library::new();
        for (title, year, runtime_mins, rating) in [
            ("Alpha", 2010, 100, 5.0),
            ("Bravo", 1990, 80, 9.0),
            ("Charlie", 2000, 120, 7.0),
        ] {
            let id = library.movie(title).await;
            library
                .movies
                .apply_enrichment(
                    id,
                    &MovieEnrichment {
                        title: title.to_string(),
                        year: Some(year),
                        runtime_mins: Some(runtime_mins),
                        rating: Some(rating),
                        ..Default::default()
                    },
                    &beam_domain::models::enrichment::FieldLocks::none(),
                )
                .await
                .unwrap();
        }
        let service = library.service();

        for (sort_by, ascending) in [
            (MediaSortField::Title, ["Alpha", "Bravo", "Charlie"]),
            (MediaSortField::Year, ["Bravo", "Charlie", "Alpha"]),
            (MediaSortField::Rating, ["Alpha", "Charlie", "Bravo"]),
            (MediaSortField::DateAdded, ["Alpha", "Bravo", "Charlie"]),
            (MediaSortField::Runtime, ["Bravo", "Alpha", "Charlie"]),
        ] {
            let asc = service
                .search_media(BrowseRequest {
                    sort_by,
                    ..request(None, None)
                })
                .await
                .unwrap();
            assert_eq!(titles(&asc), ascending, "{sort_by} asc");
            let desc = service
                .search_media(BrowseRequest {
                    sort_by,
                    sort_order: SortOrder::Desc,
                    ..request(None, None)
                })
                .await
                .unwrap();
            let mut descending = ascending;
            descending.reverse();
            assert_eq!(titles(&desc), descending, "{sort_by} desc");
        }
    }
}
