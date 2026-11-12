/// Behavioural contract for [`crate::repositories::CatalogRepository`]: one
/// listing of movies and shows together, filtered, ordered by every sort field
/// in both directions with missing values last, paged from a position either
/// way without a title repeated or skipped -- and never listing a title with
/// no present file (issues #179, #183, #187).
///
/// Titles are single ASCII words: the SQL catalogue orders by `lower(title)`
/// under the database's collation, which agrees with the in-memory byte order
/// only there.
///
/// `$setup` names an `async fn() -> impl CatalogRepositoryFixture`.
#[macro_export]
macro_rules! catalog_repository_contract {
    ($setup:path) => {
        use ::std::collections::HashMap;
        use ::std::num::NonZeroU32;
        use ::std::time::Duration;
        use ::uuid::Uuid;
        use $crate::models::catalog::{
            CatalogFilters, CatalogPosition, CatalogQuery, CatalogSort, CatalogSortField, Seek,
            SortDirection, SortKey, TitleKind,
        };
        use $crate::models::file::{CreateMediaFile, FileStatus, MediaFileContent};
        use $crate::models::movie::{CreateMovie, CreateMovieEntry};
        use $crate::models::show::{CreateEpisode, CreateShow};
        use $crate::providers::enrichment::{MovieEnrichment, ShowEnrichment};
        use $crate::repositories::contract::fixture::CatalogRepositoryFixture;

        /// A title the contract made: its id and its file's, when it has one.
        #[derive(Debug, Clone, Copy)]
        struct Made {
            id: Uuid,
            file: Option<Uuid>,
        }

        /// A present file in the library `library_id`.
        async fn present_file(
            fixture: &impl CatalogRepositoryFixture,
            library_id: Uuid,
            content: MediaFileContent,
        ) -> Uuid {
            let unique = Uuid::new_v4();
            fixture
                .files()
                .create(CreateMediaFile {
                    library_id,
                    path: ::std::path::PathBuf::from(format!("/videos/{library_id}/{unique}.mkv")),
                    hash: (unique.as_u128() as u64) >> 1,
                    size_bytes: 1024,
                    mtime: None,
                    identity: None,
                    mime_type: Some("video/x-matroska".to_string()),
                    duration: None,
                    container_format: Some("matroska".to_string()),
                    content: Some(content),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
                })
                .await
                .expect("create a file")
                .id
        }

        /// A movie as the indexer and enrichment leave it, with a present file
        /// when `with_file`.
        async fn movie(
            fixture: &impl CatalogRepositoryFixture,
            name: &str,
            year: Option<u32>,
            runtime_mins: Option<u32>,
            rating: Option<f32>,
            with_file: bool,
        ) -> Made {
            let movies = fixture.movies();
            let movie = movies
                .find_or_create_by_identity(CreateMovie::new(
                    name,
                    year,
                    runtime_mins.map(|mins| Duration::from_secs(u64::from(mins) * 60)),
                ))
                .await
                .expect("create a movie");
            if rating.is_some() {
                movies
                    .apply_enrichment(
                        movie.id,
                        &MovieEnrichment {
                            title: name.to_string(),
                            year,
                            runtime_mins,
                            rating,
                            ..Default::default()
                        },
                        &$crate::models::enrichment::FieldLocks::none(),
                    )
                    .await
                    .expect("rate the movie");
            }
            let library_id = fixture.new_library().await;
            let entry = movies
                .find_or_create_entry(CreateMovieEntry {
                    library_id,
                    movie_id: movie.id,
                    edition: None,
                })
                .await
                .expect("create an entry");
            let file = if with_file {
                Some(
                    present_file(
                        fixture,
                        library_id,
                        MediaFileContent::movie(entry.id),
                    )
                    .await,
                )
            } else {
                None
            };
            Made { id: movie.id, file }
        }

        /// A show with one season of `episodes` episodes, each with a present
        /// file when `with_files`. Returns the show and its episodes' files.
        async fn show(
            fixture: &impl CatalogRepositoryFixture,
            name: &str,
            year: Option<u32>,
            rating: Option<f32>,
            episodes: u32,
            with_files: bool,
        ) -> (Uuid, Vec<Uuid>) {
            let shows = fixture.shows();
            let show = shows
                .find_or_create_by_identity(CreateShow::new(name, year))
                .await
                .expect("create a show");
            if rating.is_some() {
                shows
                    .apply_enrichment(
                        show.id,
                        &ShowEnrichment {
                            title: name.to_string(),
                            year,
                            rating,
                            ..Default::default()
                        },
                        &$crate::models::enrichment::FieldLocks::none(),
                    )
                    .await
                    .expect("rate the show");
            }
            let season = shows
                .find_or_create_season(show.id, 1)
                .await
                .expect("create a season");
            let library_id = fixture.new_library().await;
            let mut files = Vec::new();
            for number in 1..=episodes {
                let episode = shows
                    .find_or_create_episode(CreateEpisode {
                        season_id: season.id,
                        episode_number: number,
                        title: format!("Episode {number}"),
                        runtime: None,
                        air_date: None,
                    })
                    .await
                    .expect("create an episode");
                if with_files {
                    files.push(
                        present_file(fixture, library_id, MediaFileContent::episode(episode.id))
                            .await,
                    );
                }
            }
            (show.id, files)
        }

        /// Seven live titles covering every sort field's interesting cases:
        /// both kinds, mixed-case titles, tied and missing years and ratings,
        /// and runtimes only films have. Returns each title's id and name, in
        /// the order they were created.
        async fn seed(fixture: &impl CatalogRepositoryFixture) -> Vec<(Uuid, &'static str)> {
            vec![
                (movie(fixture, "alpha", Some(2001), Some(100), Some(8.0), true).await.id, "alpha"),
                (movie(fixture, "Bravo", None, None, None, true).await.id, "Bravo"),
                (movie(fixture, "charlie", Some(1999), Some(90), None, true).await.id, "charlie"),
                (show(fixture, "delta", Some(2010), Some(9.0), 1, true).await.0, "delta"),
                (movie(fixture, "Echo", Some(2001), None, Some(6.0), true).await.id, "Echo"),
                (show(fixture, "Foxtrot", None, None, 1, true).await.0, "Foxtrot"),
                (show(fixture, "golf", Some(1999), Some(6.0), 1, true).await.0, "golf"),
            ]
        }

        fn every_sort() -> Vec<CatalogSort> {
            let mut sorts = Vec::new();
            for field in [
                CatalogSortField::Title,
                CatalogSortField::Year,
                CatalogSortField::Rating,
                CatalogSortField::DateAdded,
                CatalogSortField::Runtime,
            ] {
                for direction in [SortDirection::Asc, SortDirection::Desc] {
                    sorts.push(CatalogSort { field, direction });
                }
            }
            sorts
        }

        fn sorted(field: CatalogSortField, direction: SortDirection) -> CatalogSort {
            CatalogSort { field, direction }
        }

        fn by_title() -> CatalogSort {
            sorted(CatalogSortField::Title, SortDirection::Asc)
        }

        async fn browse(
            fixture: &impl CatalogRepositoryFixture,
            filters: CatalogFilters,
            sort: CatalogSort,
            seek: Seek,
            limit: u32,
        ) -> Vec<CatalogPosition> {
            fixture
                .repo()
                .browse(&CatalogQuery {
                    filters,
                    access: $crate::models::access::AccessPolicy::unrestricted(),
                    sort,
                    seek,
                    limit: NonZeroU32::new(limit).expect("a positive limit"),
                })
                .await
                .expect("browse")
        }

        /// A live title in `library_id`, associated with it as the indexer
        /// associates one, certified `certification`. Returns its id.
        async fn certified(
            fixture: &impl CatalogRepositoryFixture,
            kind: TitleKind,
            library_id: Uuid,
            certification: Option<&str>,
        ) -> Uuid {
            let name = format!("certified {}", Uuid::new_v4());
            match kind {
                TitleKind::Movie => {
                    let movies = fixture.movies();
                    let movie = movies
                        .find_or_create_by_identity(CreateMovie::new(name.clone(), None, None))
                        .await
                        .expect("create a movie");
                    movies
                        .apply_enrichment(
                            movie.id,
                            &MovieEnrichment {
                                title: name,
                                content_rating: certification.map(str::to_string),
                                ..Default::default()
                            },
                            &$crate::models::enrichment::FieldLocks::none(),
                        )
                        .await
                        .expect("certify the movie");
                    let entry = movies
                        .find_or_create_entry(CreateMovieEntry {
                            library_id,
                            movie_id: movie.id,
                            edition: None,
                        })
                        .await
                        .expect("create an entry");
                    movies
                        .ensure_library_association(library_id, movie.id)
                        .await
                        .expect("associate the movie");
                    present_file(fixture, library_id, MediaFileContent::movie(entry.id)).await;
                    movie.id
                }
                TitleKind::Show => {
                    let shows = fixture.shows();
                    let show = shows
                        .find_or_create_by_identity(CreateShow::new(name.clone(), None))
                        .await
                        .expect("create a show");
                    shows
                        .apply_enrichment(
                            show.id,
                            &ShowEnrichment {
                                title: name,
                                content_rating: certification.map(str::to_string),
                                ..Default::default()
                            },
                            &$crate::models::enrichment::FieldLocks::none(),
                        )
                        .await
                        .expect("certify the show");
                    let season = shows
                        .find_or_create_season(show.id, 1)
                        .await
                        .expect("create a season");
                    let episode = shows
                        .find_or_create_episode(CreateEpisode {
                            season_id: season.id,
                            episode_number: 1,
                            title: "Pilot".to_string(),
                            runtime: None,
                            air_date: None,
                        })
                        .await
                        .expect("create an episode");
                    shows
                        .ensure_library_association(library_id, show.id)
                        .await
                        .expect("associate the show");
                    present_file(fixture, library_id, MediaFileContent::episode(episode.id))
                        .await;
                    show.id
                }
            }
        }

        /// Only the titles a user's policy admits are listed: those in a
        /// granted library, certified within the ceiling (issue #263).
        #[tokio::test]
        async fn a_listing_holds_only_the_titles_the_access_policy_admits() {
            use ::std::collections::BTreeSet;
            use $crate::models::access::{AccessPolicy, ContentRating, LibraryAccess};

            let fixture = $setup().await;
            let (kids, grown) = (fixture.new_library().await, fixture.new_library().await);
            let pg_movie = certified(&fixture, TitleKind::Movie, kids, Some("PG")).await;
            let unrated_movie = certified(&fixture, TitleKind::Movie, kids, None).await;
            let not_recognised = certified(&fixture, TitleKind::Movie, kids, Some("NR")).await;
            let tv_pg_show = certified(&fixture, TitleKind::Show, kids, Some("TV-PG")).await;
            let r_movie = certified(&fixture, TitleKind::Movie, grown, Some("R")).await;
            let tv_ma_show = certified(&fixture, TitleKind::Show, grown, Some("TV-MA")).await;

            let listed = |access: AccessPolicy| {
                let fixture = &fixture;
                async move {
                    fixture
                        .repo()
                        .browse(&CatalogQuery {
                            filters: CatalogFilters::default(),
                            access,
                            sort: by_title(),
                            seek: Seek::Forward(None),
                            limit: NonZeroU32::new(1000).expect("a positive limit"),
                        })
                        .await
                        .expect("browse")
                        .into_iter()
                        .map(|row| row.id)
                        .collect::<BTreeSet<Uuid>>()
                }
            };
            let set = |ids: &[Uuid]| ids.iter().copied().collect::<BTreeSet<Uuid>>();
            let only = |ids: &[Uuid]| LibraryAccess::Only(ids.iter().copied().collect());

            assert_eq!(
                listed(AccessPolicy::unrestricted()).await,
                set(&[pg_movie, unrated_movie, not_recognised, tv_pg_show, r_movie, tv_ma_show])
            );
            assert_eq!(
                listed(AccessPolicy {
                    libraries: only(&[kids]),
                    max_rating: None,
                })
                .await,
                set(&[pg_movie, unrated_movie, not_recognised, tv_pg_show]),
                "a grant lists its library's titles, rated or not"
            );
            assert_eq!(
                listed(AccessPolicy {
                    libraries: LibraryAccess::All,
                    max_rating: Some(ContentRating::ParentalGuidance),
                })
                .await,
                set(&[pg_movie, tv_pg_show]),
                "a ceiling hides what is above it and what it cannot place"
            );
            assert_eq!(
                listed(AccessPolicy {
                    libraries: only(&[grown]),
                    max_rating: Some(ContentRating::Mature),
                })
                .await,
                set(&[r_movie, tv_ma_show])
            );
            assert_eq!(
                listed(AccessPolicy {
                    libraries: only(&[grown]),
                    max_rating: Some(ContentRating::Teen),
                })
                .await,
                set(&[]),
                "both restrictions hold at once"
            );
            assert_eq!(
                listed(AccessPolicy {
                    libraries: only(&[]),
                    max_rating: None,
                })
                .await,
                set(&[]),
                "no grant lists nothing"
            );
        }

        /// The whole listing in one page.
        async fn everything(
            fixture: &impl CatalogRepositoryFixture,
            filters: CatalogFilters,
            sort: CatalogSort,
        ) -> Vec<CatalogPosition> {
            browse(fixture, filters, sort, Seek::Forward(None), 1000).await
        }

        fn names(rows: &[CatalogPosition], made: &[(Uuid, &'static str)]) -> Vec<&'static str> {
            let by_id: HashMap<Uuid, &'static str> = made.iter().copied().collect();
            rows.iter()
                .map(|row| *by_id.get(&row.id).expect("a title the contract made"))
                .collect()
        }

        /// `rows` lists `groups` in order; within a group, in any order.
        #[track_caller]
        fn assert_grouped(rows: &[&'static str], groups: &[&[&'static str]]) {
            let mut at = 0;
            for group in groups {
                let mut got: Vec<&str> = rows[at..at + group.len()].to_vec();
                let mut want: Vec<&str> = group.to_vec();
                got.sort_unstable();
                want.sort_unstable();
                assert_eq!(got, want, "at {at} of {rows:?}");
                at += group.len();
            }
            assert_eq!(at, rows.len(), "{rows:?} has more than {groups:?}");
        }

        /// The key a title really has for `field`, read back through its own
        /// repository.
        async fn stored_key(
            fixture: &impl CatalogRepositoryFixture,
            row: &CatalogPosition,
            field: CatalogSortField,
        ) -> SortKey {
            match row.kind {
                TitleKind::Movie => {
                    let movie = fixture.movies().find_by_id(row.id).await.unwrap().unwrap();
                    match field {
                        CatalogSortField::Title => SortKey::Title(movie.title.to_lowercase()),
                        CatalogSortField::Year => SortKey::Year(movie.year.map(|y| y as i32)),
                        CatalogSortField::Rating => SortKey::Rating(movie.rating_tmdb),
                        CatalogSortField::DateAdded => SortKey::DateAdded(movie.created_at),
                        CatalogSortField::Runtime => {
                            SortKey::Runtime(movie.runtime.map(|d| (d.as_secs() / 60) as i32))
                        }
                        CatalogSortField::Relevance => {
                            unreachable!("a relevance key is the store's, not the title's")
                        }
                    }
                }
                TitleKind::Show => {
                    let show = fixture.shows().find_by_id(row.id).await.unwrap().unwrap();
                    match field {
                        CatalogSortField::Title => SortKey::Title(show.title.to_lowercase()),
                        CatalogSortField::Year => SortKey::Year(show.year.map(|y| y as i32)),
                        CatalogSortField::Rating => SortKey::Rating(show.rating_tmdb),
                        CatalogSortField::DateAdded => SortKey::DateAdded(show.created_at),
                        CatalogSortField::Runtime => SortKey::Runtime(None),
                        CatalogSortField::Relevance => {
                            unreachable!("a relevance key is the store's, not the title's")
                        }
                    }
                }
            }
        }

        async fn order_of(
            fixture: &impl CatalogRepositoryFixture,
            made: &[(Uuid, &'static str)],
            field: CatalogSortField,
            direction: SortDirection,
        ) -> Vec<&'static str> {
            names(
                &everything(fixture, CatalogFilters::default(), sorted(field, direction)).await,
                made,
            )
        }

        async fn listed_names(
            fixture: &impl CatalogRepositoryFixture,
            made: &[(Uuid, &'static str)],
            filters: CatalogFilters,
        ) -> Vec<&'static str> {
            names(&everything(fixture, filters, by_title()).await, made)
        }

        #[tokio::test]
        async fn titles_of_both_kinds_interleave_by_title_ignoring_case() {
            let fixture = $setup().await;
            let made = seed(&fixture).await;

            let asc = everything(&fixture, CatalogFilters::default(), by_title()).await;
            assert_eq!(
                names(&asc, &made),
                ["alpha", "Bravo", "charlie", "delta", "Echo", "Foxtrot", "golf"]
            );
            assert_eq!(asc[3].kind, TitleKind::Show);
            assert_eq!(asc[3].key, SortKey::Title("delta".to_string()));

            assert_eq!(
                order_of(&fixture, &made, CatalogSortField::Title, SortDirection::Desc).await,
                ["golf", "Foxtrot", "Echo", "delta", "charlie", "Bravo", "alpha"]
            );
        }

        #[tokio::test]
        async fn every_field_sorts_both_ways_with_missing_values_last() {
            use CatalogSortField::{DateAdded, Rating, Runtime, Year};
            use SortDirection::{Asc, Desc};
            let fixture = $setup().await;
            let made = seed(&fixture).await;

            // A group is one key: its titles tie, and the id -- not the kind
            // -- orders them (strict order is the next test's to check).
            assert_grouped(
                &order_of(&fixture, &made, Year, Asc).await,
                &[&["charlie", "golf"], &["alpha", "Echo"], &["delta"], &["Bravo", "Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Year, Desc).await,
                &[&["delta"], &["alpha", "Echo"], &["charlie", "golf"], &["Bravo", "Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Rating, Asc).await,
                &[&["Echo", "golf"], &["alpha"], &["delta"], &["Bravo", "charlie", "Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Rating, Desc).await,
                &[&["delta"], &["alpha"], &["Echo", "golf"], &["Bravo", "charlie", "Foxtrot"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Runtime, Asc).await,
                &[&["charlie"], &["alpha"], &["Bravo", "delta", "Echo", "Foxtrot", "golf"]],
            );
            assert_grouped(
                &order_of(&fixture, &made, Runtime, Desc).await,
                &[&["alpha"], &["charlie"], &["Bravo", "delta", "Echo", "Foxtrot", "golf"]],
            );
            let created: Vec<&str> = made.iter().map(|(_, name)| *name).collect();
            assert_eq!(order_of(&fixture, &made, DateAdded, Asc).await, created);
            let mut newest_first = created.clone();
            newest_first.reverse();
            assert_eq!(order_of(&fixture, &made, DateAdded, Desc).await, newest_first);
        }

        /// Whatever the sort, every row carries the key its title really has,
        /// and each row sorts strictly after the one before it -- ties
        /// included, which only the `(id, kind)` tie-break can separate.
        #[tokio::test]
        async fn every_row_carries_its_own_key_in_strict_display_order() {
            let fixture = $setup().await;
            seed(&fixture).await;

            for sort in every_sort() {
                let rows = everything(&fixture, CatalogFilters::default(), sort).await;
                assert_eq!(rows.len(), 7, "{sort:?}");
                for row in &rows {
                    assert_eq!(row.key, stored_key(&fixture, row, sort.field).await, "{sort:?}");
                }
                for pair in rows.windows(2) {
                    assert!(
                        pair[0].display_cmp(&pair[1], sort.direction).is_lt(),
                        "{sort:?}: {:?} before {:?}",
                        pair[0],
                        pair[1]
                    );
                }
            }
        }

        #[tokio::test]
        async fn paging_forward_in_any_page_size_visits_every_title_once() {
            let fixture = $setup().await;
            seed(&fixture).await;

            for sort in every_sort() {
                let whole = everything(&fixture, CatalogFilters::default(), sort).await;
                for size in 1..=3_u32 {
                    let mut walked = Vec::new();
                    let mut after = None;
                    loop {
                        let page = browse(
                            &fixture,
                            CatalogFilters::default(),
                            sort,
                            Seek::Forward(after.clone()),
                            size,
                        )
                        .await;
                        assert!(page.len() <= size as usize);
                        let short = page.len() < size as usize;
                        after = page.last().cloned();
                        walked.extend(page);
                        if short {
                            break;
                        }
                    }
                    assert_eq!(walked, whole, "{sort:?} in pages of {size}");
                }
            }
        }

        #[tokio::test]
        async fn paging_backward_from_the_end_mirrors_paging_forward() {
            let fixture = $setup().await;
            seed(&fixture).await;

            for sort in every_sort() {
                let whole = everything(&fixture, CatalogFilters::default(), sort).await;
                for size in 1..=3_u32 {
                    let mut walked: Vec<CatalogPosition> = Vec::new();
                    let mut before = None;
                    loop {
                        let page = browse(
                            &fixture,
                            CatalogFilters::default(),
                            sort,
                            Seek::Backward(before.clone()),
                            size,
                        )
                        .await;
                        assert!(page.len() <= size as usize);
                        let short = page.len() < size as usize;
                        before = page.first().cloned();
                        let mut joined = page;
                        joined.extend(walked);
                        walked = joined;
                        if short {
                            break;
                        }
                    }
                    assert_eq!(walked, whole, "{sort:?} in pages of {size}, backwards");
                }
            }
        }

        /// A position is a value, not an offset: a page from a title that has
        /// since left the listing starts where that title was.
        #[tokio::test]
        async fn a_page_from_a_title_that_left_the_listing_continues_where_it_was() {
            let fixture = $setup().await;
            let made = seed(&fixture).await;
            let whole = everything(&fixture, CatalogFilters::default(), by_title()).await;
            let gone = whole[2].clone();
            assert_eq!(names(std::slice::from_ref(&gone), &made), ["charlie"]);
            // charlie is a movie: its only file is behind its only entry.
            let entries = fixture.movies().find_entries_by_movie_id(gone.id).await.unwrap();
            let files = fixture.files().find_by_movie_entry_id(entries[0].id).await.unwrap();
            fixture
                .files()
                .mark_missing(vec![files[0].id], ::chrono::Utc::now())
                .await
                .unwrap();

            let after = browse(
                &fixture,
                CatalogFilters::default(),
                by_title(),
                Seek::Forward(Some(gone.clone())),
                10,
            )
            .await;
            assert_eq!(names(&after, &made), ["delta", "Echo", "Foxtrot", "golf"]);
            let before = browse(
                &fixture,
                CatalogFilters::default(),
                by_title(),
                Seek::Backward(Some(gone)),
                10,
            )
            .await;
            assert_eq!(names(&before, &made), ["alpha", "Bravo"]);
        }

        #[tokio::test]
        async fn a_backward_page_is_the_last_rows_before_the_position_in_display_order() {
            let fixture = $setup().await;
            let made = seed(&fixture).await;
            let sort = sorted(CatalogSortField::Title, SortDirection::Desc);

            let last_two =
                browse(&fixture, CatalogFilters::default(), sort, Seek::Backward(None), 2).await;
            assert_eq!(names(&last_two, &made), ["Bravo", "alpha"]);

            let whole = everything(&fixture, CatalogFilters::default(), sort).await;
            let before_echo = browse(
                &fixture,
                CatalogFilters::default(),
                sort,
                Seek::Backward(Some(whole[2].clone())),
                1,
            )
            .await;
            assert_eq!(names(&before_echo, &made), ["Foxtrot"]);
        }

        #[tokio::test]
        async fn every_filter_narrows_both_kinds() {
            let fixture = $setup().await;
            let made = seed(&fixture).await;
            let id_of = |name: &str| made.iter().find(|(_, n)| *n == name).unwrap().0;
            let genres = fixture.genres();
            genres
                .set_movie_genres(
                    id_of("alpha"),
                    &["Science Fiction".to_string(), "Drama".to_string()],
                )
                .await
                .unwrap();
            genres
                .set_show_genres(id_of("delta"), &["Science Fiction".to_string()])
                .await
                .unwrap();
            genres
                .set_movie_genres(id_of("charlie"), &["Comedy".to_string()])
                .await
                .unwrap();
            let listed = |filters| listed_names(&fixture, &made, filters);

            assert_eq!(
                listed(CatalogFilters { kind: Some(TitleKind::Show), ..Default::default() }).await,
                ["delta", "Foxtrot", "golf"]
            );
            assert_eq!(
                listed(CatalogFilters { kind: Some(TitleKind::Movie), ..Default::default() }).await,
                ["alpha", "Bravo", "charlie", "Echo"]
            );
            assert_eq!(
                listed(CatalogFilters { query: Some("ALPH".to_string()), ..Default::default() })
                    .await,
                ["alpha"],
                "the title search ignores case"
            );
            assert_eq!(
                listed(CatalogFilters {
                    genre_slug: Some("science-fiction".to_string()),
                    ..Default::default()
                })
                .await,
                ["alpha", "delta"]
            );
            assert_eq!(
                listed(CatalogFilters { genre_slug: Some("drama".to_string()), ..Default::default() })
                    .await,
                ["alpha"]
            );
            assert_eq!(
                listed(CatalogFilters {
                    genre_slug: Some("science-fiction".to_string()),
                    kind: Some(TitleKind::Movie),
                    ..Default::default()
                })
                .await,
                ["alpha"]
            );
            assert_eq!(
                listed(CatalogFilters { year: Some(2001), ..Default::default() }).await,
                ["alpha", "Echo"]
            );
            assert_eq!(
                listed(CatalogFilters { year_from: Some(2000), ..Default::default() }).await,
                ["alpha", "delta", "Echo"]
            );
            assert_eq!(
                listed(CatalogFilters { year_to: Some(2000), ..Default::default() }).await,
                ["charlie", "golf"]
            );
            assert_eq!(
                listed(CatalogFilters {
                    year_from: Some(2000),
                    year_to: Some(2005),
                    ..Default::default()
                })
                .await,
                ["alpha", "Echo"]
            );
            assert_eq!(
                listed(CatalogFilters { min_rating: Some(7.0), ..Default::default() }).await,
                ["alpha", "delta"],
                "a show's rating counts as a movie's does"
            );
            assert_eq!(
                listed(CatalogFilters { min_rating: Some(6.0), ..Default::default() }).await,
                ["alpha", "delta", "Echo", "golf"],
                "a rating exactly at the minimum is kept; an unrated title counts as 0"
            );
        }

        /// The minimum rating compares in the precision a rating is stored
        /// in: a stored 7.2 meets a minimum of 7.2, not 7.19999..., for either kind.
        #[tokio::test]
        async fn a_rating_exactly_at_a_fractional_minimum_is_kept() {
            let fixture = $setup().await;
            let made = vec![
                (movie(&fixture, "lima", None, None, Some(7.2), true).await.id, "lima"),
                (show(&fixture, "mike", None, Some(7.2), 1, true).await.0, "mike"),
                (movie(&fixture, "november", None, None, Some(7.1), true).await.id, "november"),
            ];
            assert_eq!(
                listed_names(
                    &fixture,
                    &made,
                    CatalogFilters { min_rating: Some(7.2), ..Default::default() },
                )
                .await,
                ["lima", "mike"]
            );
        }

        /// Only titles with a present file are browsable or searchable
        /// (issues #179, #183): a title that never had a file, or whose files
        /// are all missing, is on no page and matches no filter -- and comes
        /// back the moment one of its files does.
        #[tokio::test]
        async fn only_titles_with_a_present_file_are_listed() {
            let fixture = $setup().await;
            movie(&fixture, "hotel", None, None, None, false).await;
            show(&fixture, "india", None, None, 1, false).await;
            let kept_movie = movie(&fixture, "juliet", None, None, None, true).await;
            let (kept_show, show_files) = show(&fixture, "kilo", None, None, 2, true).await;
            let movie_file = kept_movie.file.expect("juliet has a file");
            let listed = |filters| {
                let fixture = &fixture;
                async move {
                    everything(fixture, filters, by_title())
                        .await
                        .into_iter()
                        .map(|row| row.id)
                        .collect::<Vec<Uuid>>()
                }
            };

            assert_eq!(
                listed(CatalogFilters::default()).await,
                [kept_movie.id, kept_show],
                "titles with no file are not listed"
            );
            assert!(
                listed(CatalogFilters { query: Some("hotel".to_string()), ..Default::default() })
                    .await
                    .is_empty(),
                "nor found by searching for them"
            );

            let files = fixture.files();
            files
                .mark_missing(vec![movie_file, show_files[0]], ::chrono::Utc::now())
                .await
                .unwrap();
            assert_eq!(
                listed(CatalogFilters::default()).await,
                [kept_show],
                "a movie whose only file is missing leaves; a show with another present episode stays"
            );

            files.mark_missing(vec![show_files[1]], ::chrono::Utc::now()).await.unwrap();
            assert!(
                listed(CatalogFilters::default()).await.is_empty(),
                "a show with every episode file missing leaves"
            );
            assert!(
                listed(CatalogFilters { query: Some("juliet".to_string()), ..Default::default() })
                    .await
                    .is_empty(),
                "and is not found by searching for it"
            );
            assert!(
                fixture.movies().find_by_id(kept_movie.id).await.unwrap().is_some(),
                "a hidden title still resolves by id"
            );

            files.restore(movie_file).await.unwrap();
            assert_eq!(
                listed(CatalogFilters::default()).await,
                [kept_movie.id],
                "listed again when its file returns"
            );
        }

        #[tokio::test]
        async fn a_position_keyed_for_another_sort_is_refused() {
            let fixture = $setup().await;
            let query = CatalogQuery {
                filters: CatalogFilters::default(),
                access: $crate::models::access::AccessPolicy::unrestricted(),
                sort: by_title(),
                seek: Seek::Forward(Some(CatalogPosition {
                    kind: TitleKind::Movie,
                    id: Uuid::new_v4(),
                    key: SortKey::Year(Some(2001)),
                })),
                limit: NonZeroU32::MIN,
            };
            let refused = fixture.repo().browse(&query).await.unwrap_err();
            assert_eq!(
                refused.to_string(),
                $crate::repositories::catalog::mismatched_position(&query)
                    .expect("a year position does not seek a title sort")
                    .to_string()
            );
        }

        /// The titles `relevance_ranks_the_closest_title_first` searches,
        /// in the order a search for `matrix` ranks them: the same text as
        /// the query (twice, one of each kind), the query and a suffix, the
        /// query behind a prefix. `zulu` matches nothing and is never listed.
        async fn seed_matches(
            fixture: &impl CatalogRepositoryFixture,
        ) -> Vec<(Uuid, &'static str)> {
            vec![
                (movie(fixture, "Matrix", None, None, None, true).await.id, "Matrix"),
                (show(fixture, "matrix", None, None, 1, true).await.0, "matrix"),
                (show(fixture, "Matrixes", None, None, 1, true).await.0, "Matrixes"),
                (movie(fixture, "Thematrix", None, None, None, true).await.id, "Thematrix"),
                (movie(fixture, "zulu", None, None, None, true).await.id, "zulu"),
            ]
        }

        fn matching(query: &str) -> CatalogFilters {
            CatalogFilters { query: Some(query.to_string()), ..Default::default() }
        }

        /// A relevance sort puts the title closest to the query first
        /// ascending and last descending, ties broken by `(id, kind)` as in
        /// every sort, and pages from a position either way without a title
        /// repeated or skipped (issue #247).
        #[tokio::test]
        async fn relevance_ranks_the_closest_title_first() {
            use SortDirection::{Asc, Desc};
            let fixture = $setup().await;
            let made = seed_matches(&fixture).await;

            let by_relevance = |direction| sorted(CatalogSortField::Relevance, direction);

            let asc = everything(&fixture, matching("matrix"), by_relevance(Asc)).await;
            assert_grouped(
                &names(&asc, &made),
                &[&["Matrix", "matrix"], &["Matrixes"], &["Thematrix"]],
            );
            assert_eq!(
                asc[0].key,
                SortKey::Relevance(0.0),
                "the same text is no distance away"
            );
            for pair in asc.windows(2) {
                assert!(
                    pair[0].display_cmp(&pair[1], Asc).is_lt(),
                    "{:?} before {:?}",
                    pair[0],
                    pair[1]
                );
            }
            let desc = everything(&fixture, matching("matrix"), by_relevance(Desc)).await;
            let mut reversed = asc.clone();
            reversed.reverse();
            assert_eq!(desc, reversed, "descending is ascending reversed, ties included");

            for direction in [Asc, Desc] {
                let sort = by_relevance(direction);
                let whole = everything(&fixture, matching("matrix"), sort).await;
                let mut forward = Vec::new();
                let mut after = None;
                loop {
                    let seek = Seek::Forward(after);
                    let page = browse(&fixture, matching("matrix"), sort, seek, 1).await;
                    let Some(last) = page.last().cloned() else {
                        break;
                    };
                    forward.extend(page);
                    after = Some(last);
                }
                assert_eq!(forward, whole, "{direction:?} forwards");
                let mut backward = Vec::new();
                let mut before = None;
                loop {
                    let seek = Seek::Backward(before);
                    let page = browse(&fixture, matching("matrix"), sort, seek, 1).await;
                    let Some(first) = page.first().cloned() else {
                        break;
                    };
                    backward.splice(0..0, page);
                    before = Some(first);
                }
                assert_eq!(backward, whole, "{direction:?} backwards");
            }
        }

        /// Relevance is distance from a query, so a listing with none has
        /// nothing to rank by and is refused rather than ordered arbitrarily.
        #[tokio::test]
        async fn relevance_without_a_query_is_refused() {
            let fixture = $setup().await;
            seed_matches(&fixture).await;
            let query = CatalogQuery {
                filters: CatalogFilters::default(),
                access: $crate::models::access::AccessPolicy::unrestricted(),
                sort: sorted(CatalogSortField::Relevance, SortDirection::Asc),
                seek: Seek::Forward(None),
                limit: NonZeroU32::MIN,
            };
            let refused = fixture.repo().browse(&query).await.unwrap_err();
            assert_eq!(
                refused.to_string(),
                $crate::repositories::catalog::unranked_relevance(&query)
                    .expect("relevance with no query is unranked")
                    .to_string()
            );
        }
    };
}
