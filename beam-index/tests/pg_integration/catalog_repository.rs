//! The shared `CatalogRepository` contract, run against real SQL: the
//! `UNION ALL` keyset statement over real rows, `pg_trgm` similarity, the
//! genre junctions and the liveness joins (issue #187).
//!
//! The catalogue is global, so each contract test runs in a migrated schema of
//! its own.

use async_trait::async_trait;
use beam_domain::repositories::{
    CatalogRepository, FileRepository, GenreRepository, MovieRepository, ShowRepository,
};
use beam_index::repositories::{
    SqlCatalogRepository, SqlFileRepository, SqlGenreRepository, SqlMovieRepository,
    SqlShowRepository,
};
use beam_test_support::postgres::ScopedSchema;
use beam_test_support::seed;

struct PgFixture {
    schema: ScopedSchema,
    repo: SqlCatalogRepository,
    movies: SqlMovieRepository,
    shows: SqlShowRepository,
    genres: SqlGenreRepository,
    files: SqlFileRepository,
}

#[async_trait]
impl beam_domain::repositories::contract::fixture::CatalogRepositoryFixture for PgFixture {
    fn repo(&self) -> &dyn CatalogRepository {
        &self.repo
    }

    fn movies(&self) -> &dyn MovieRepository {
        &self.movies
    }

    fn shows(&self) -> &dyn ShowRepository {
        &self.shows
    }

    fn genres(&self) -> &dyn GenreRepository {
        &self.genres
    }

    fn files(&self) -> &dyn FileRepository {
        &self.files
    }

    async fn new_library(&self) -> Uuid {
        seed::library(self.schema.db().as_ref())
            .await
            .expect("seed a library")
    }
}

async fn setup() -> PgFixture {
    // Left behind on purpose, as for the other contracts: the next run's
    // `migrate_once` sweeps every `beam_test_*` schema.
    let schema = ScopedSchema::create_migrated("catalog_contract")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    PgFixture {
        repo: SqlCatalogRepository::new(db.clone()),
        movies: SqlMovieRepository::new(db.clone()),
        shows: SqlShowRepository::new(db.clone()),
        genres: SqlGenreRepository::new(db.clone()),
        files: SqlFileRepository::new(db),
        schema,
    }
}

beam_domain::catalog_repository_contract!(setup);

/// The default order is `lower(title)` under the database's collation, not
/// byte order: titles differing only in case sort together, whichever kind
/// they are, and the page after one of them starts at the next.
#[tokio::test]
async fn titles_differing_only_in_case_sort_together_and_page_exactly() {
    use beam_domain::models::catalog::{CatalogFilters, CatalogQuery, CatalogSort, Seek};
    use beam_domain::models::catalog::{CatalogSortField, SortDirection};
    use beam_domain::models::{CreateMovie, CreateShow};

    let fixture = setup().await;
    for title in ["zulu", "ALPHA", "Mike"] {
        fixture
            .movies
            .find_or_create_by_identity(CreateMovie::new(title, None, None))
            .await
            .unwrap();
    }
    fixture
        .shows
        .find_or_create_by_identity(CreateShow::new("alpha", Some(1999)))
        .await
        .unwrap();
    // Unfiltered liveness would hide all four: give each a file.
    let library = fixture.new_library().await;
    fixture.give_every_title_a_file(library).await;

    let sort = CatalogSort {
        field: CatalogSortField::Title,
        direction: SortDirection::Asc,
    };
    let first = fixture
        .repo
        .browse(&CatalogQuery {
            filters: CatalogFilters::default(),
            sort,
            seek: Seek::Forward(None),
            limit: std::num::NonZeroU32::new(2).unwrap(),
        })
        .await
        .unwrap();
    let keys: Vec<_> = first.iter().map(|p| p.key.clone()).collect();
    assert_eq!(
        keys,
        [
            beam_domain::models::catalog::SortKey::Title("alpha".to_string()),
            beam_domain::models::catalog::SortKey::Title("alpha".to_string()),
        ],
        "a movie and a show titled alike sort together, before `Mike`"
    );
    let rest = fixture
        .repo
        .browse(&CatalogQuery {
            filters: CatalogFilters::default(),
            sort,
            seek: Seek::Forward(first.last().cloned()),
            limit: std::num::NonZeroU32::new(10).unwrap(),
        })
        .await
        .unwrap();
    let rest: Vec<_> = rest.into_iter().map(|p| p.key).collect();
    assert_eq!(
        rest,
        [
            beam_domain::models::catalog::SortKey::Title("mike".to_string()),
            beam_domain::models::catalog::SortKey::Title("zulu".to_string()),
        ]
    );
}

/// A search's `%` runs at a similarity threshold of 0.2, not pg_trgm's 0.3:
/// `matirx` shares 3 of 11 trigrams with `Matrix` (0.27), so a one-letter
/// typo still finds a short title -- and only inside the browse's own
/// transaction, so the pooled connection goes back at the default.
#[tokio::test]
async fn a_search_matches_at_a_similarity_of_two_tenths_and_leaves_the_session_as_it_was() {
    use beam_domain::models::CreateMovie;
    use beam_domain::models::catalog::{CatalogFilters, CatalogQuery, CatalogSort, Seek};
    use beam_domain::models::catalog::{CatalogSortField, SortDirection};
    use sea_orm::{ConnectionTrait, Statement};

    let fixture = setup().await;
    let movie = fixture
        .movies
        .find_or_create_by_identity(CreateMovie::new("Matrix", None, None))
        .await
        .unwrap();
    let library = fixture.new_library().await;
    fixture.give_every_title_a_file(library).await;

    let db = fixture.schema.db();
    let similarity: f32 = db
        .query_one_raw(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT similarity('Matrix', 'matirx') AS s",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "s")
        .unwrap();
    assert!(
        (0.2..0.3).contains(&similarity),
        "the case sits between the two thresholds: {similarity}"
    );

    for sort in [CatalogSortField::Title, CatalogSortField::Relevance] {
        let found = fixture
            .repo
            .browse(&CatalogQuery {
                filters: CatalogFilters {
                    query: Some("matirx".to_string()),
                    ..Default::default()
                },
                sort: CatalogSort {
                    field: sort,
                    direction: SortDirection::Asc,
                },
                seek: Seek::Forward(None),
                limit: std::num::NonZeroU32::new(10).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(
            found.iter().map(|p| p.id).collect::<Vec<_>>(),
            [movie.id],
            "{sort:?}"
        );
    }

    let threshold: String = db
        .query_one_raw(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            // Unknown to a connection that has not loaded pg_trgm yet, which
            // is the default too.
            "SELECT COALESCE(current_setting('pg_trgm.similarity_threshold', true), '0.3') AS t",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "t")
        .unwrap();
    assert_eq!(threshold, "0.3", "the setting ended with the browse");
}

impl PgFixture {
    /// A present file for every movie (through an entry) and every show
    /// (through a season and an episode) in the schema.
    async fn give_every_title_a_file(&self, library_id: Uuid) {
        use beam_domain::models::{
            CreateEpisode, CreateMediaFile, CreateMovieEntry, FileStatus, MediaFileContent,
        };

        let mut contents = Vec::new();
        for movie in self.movies.find_all().await.unwrap() {
            let entry = self
                .movies
                .find_or_create_entry(CreateMovieEntry {
                    library_id,
                    movie_id: movie.id,
                    edition: None,
                })
                .await
                .unwrap();
            contents.push(MediaFileContent::movie(entry.id));
        }
        for show in self.shows.find_all().await.unwrap() {
            let season = self.shows.find_or_create_season(show.id, 1).await.unwrap();
            let episode = self
                .shows
                .find_or_create_episode(CreateEpisode {
                    season_id: season.id,
                    episode_number: 1,
                    title: "Pilot".to_string(),
                    runtime: None,
                    air_date: None,
                })
                .await
                .unwrap();
            contents.push(MediaFileContent::episode(episode.id));
        }
        for content in contents {
            let unique = Uuid::new_v4();
            self.files
                .create(CreateMediaFile {
                    library_id,
                    path: std::path::PathBuf::from(format!("/videos/{unique}.mkv")),
                    hash: (unique.as_u128() as u64) >> 1,
                    size_bytes: 1024,
                    mtime: None,
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(content),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
                })
                .await
                .unwrap();
        }
    }
}

/// NFR-301 on a catalogue big enough for the planner to have a choice: a page
/// sorted by an indexed key is an ordered index scan of each kind's table
/// under the limit, never a sequential read and sort of every title.
mod plans {
    use std::num::NonZeroU32;

    use beam_domain::models::catalog::{
        CatalogFilters, CatalogPosition, CatalogQuery, CatalogSort, CatalogSortField, Seek,
        SortDirection, TitleKind,
    };
    use beam_domain::repositories::CatalogRepository;
    use beam_index::repositories::SqlCatalogRepository;
    use beam_test_support::postgres::ScopedSchema;
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

    /// Titles in the seeded catalogue: enough that reading them all costs
    /// far more than reading one page through an index. As many shows as
    /// movies: the planner reads a table of 3,000 titles whole rather than
    /// through its trigram index, its cost model pricing a trigram match
    /// as one cheap operator.
    const MOVIES: u32 = 6_000;
    const SHOWS: u32 = 6_000;
    const EPISODES_PER_SHOW: u32 = 4;
    /// Roughly half the titles the seeded catalogue lists: every show and
    /// nine movies in ten.
    const MID_LISTING: u32 = (MOVIES / 10 * 9 + SHOWS) / 2;

    /// One title in this many, of either kind, is rare: titled `Needle`, in
    /// the `rare` genre (every second title is in `common`), from
    /// [`RARE_YEAR`] and rated 10 -- 10 movies and 10 shows, fewer than a
    /// page.
    /// They are the titles one past a multiple of it, so none is among the
    /// movies whose file is missing -- every tenth, counting from the tenth.
    const RARE: u32 = 600;
    /// The year only the rare titles are from; every other title is from
    /// 1950-2019.
    const RARE_YEAR: i32 = 2050;

    /// Seed the catalogue set-based -- one statement per table -- with every
    /// tenth movie's file missing, so liveness really filters, then vacuum
    /// and refresh the planner's statistics: a catalogue in its steady state,
    /// not one whose trigram index still holds every title in its pending
    /// list.
    async fn seed_catalogue(db: &DatabaseConnection) {
        let library = uuid::Uuid::new_v4();
        let statements = [
            format!(
                "INSERT INTO libraries (id, name, root_path, created_at, updated_at) \
                 VALUES ('{library}', 'plans', '/plans/{library}', now(), now())"
            ),
            format!(
                "INSERT INTO movies (id, title, year, runtime_mins, rating_tmdb, created_at, updated_at) \
                 SELECT gen_random_uuid(), \
                        CASE WHEN g % {RARE} = 1 THEN 'Needle ' ELSE 'Movie ' END || md5(g::text), \
                        CASE WHEN g % {RARE} = 1 THEN {RARE_YEAR} ELSE 1950 + g % 70 END, \
                        80 + g % 90, \
                        CASE WHEN g % {RARE} = 1 THEN 10 ELSE (g % 100) / 10.0 END, \
                        now() - g * interval '1 minute', now() \
                   FROM generate_series(1, {MOVIES}) g"
            ),
            format!(
                "INSERT INTO movie_entries (id, library_id, movie_id, created_at) \
                 SELECT gen_random_uuid(), '{library}', id, now() FROM movies"
            ),
            "INSERT INTO files (id, movie_entry_id, library_id, file_path, file_size, hash_xxh3, \
                                scanned_at, updated_at, missing_since) \
             SELECT gen_random_uuid(), me.id, me.library_id, '/plans/m/' || me.id, 1, 1, now(), now(), \
                    CASE WHEN row_number() OVER () % 10 = 0 THEN now() END \
               FROM movie_entries me"
                .to_string(),
            format!(
                "INSERT INTO shows (id, title, year, rating_tmdb, created_at, updated_at) \
                 SELECT gen_random_uuid(), \
                        CASE WHEN g % {RARE} = 1 THEN 'Needle ' ELSE 'Show ' END || md5(g::text), \
                        CASE WHEN g % {RARE} = 1 THEN {RARE_YEAR} ELSE 1950 + g % 70 END, \
                        CASE WHEN g % {RARE} = 1 THEN 10 ELSE (g % 100) / 10.0 END, \
                        now() - g * interval '1 minute', now() \
                   FROM generate_series(1, {SHOWS}) g"
            ),
            "INSERT INTO seasons (id, show_id, season_number) \
             SELECT gen_random_uuid(), id, 1 FROM shows"
                .to_string(),
            format!(
                "INSERT INTO episodes (id, season_id, episode_number, title, created_at) \
                 SELECT gen_random_uuid(), se.id, n, 'E' || n, now() \
                   FROM seasons se, generate_series(1, {EPISODES_PER_SHOW}) n"
            ),
            format!(
                "INSERT INTO files (id, episode_id, library_id, file_path, file_size, hash_xxh3, \
                                    scanned_at, updated_at) \
                 SELECT gen_random_uuid(), e.id, '{library}', '/plans/e/' || e.id, 1, 1, now(), now() \
                   FROM episodes e"
            ),
            "INSERT INTO genres (id, name, slug) \
             VALUES (gen_random_uuid(), 'Rare', 'rare'), (gen_random_uuid(), 'Common', 'common')"
                .to_string(),
            format!(
                "INSERT INTO movie_genres (movie_id, genre_id) \
                 SELECT m.id, g.id FROM (SELECT id, title, row_number() OVER () AS n FROM movies) m, \
                        genres g \
                  WHERE (g.slug = 'rare' AND m.title LIKE 'Needle %') \
                     OR (g.slug = 'common' AND m.n % 2 = 0)"
            ),
            format!(
                "INSERT INTO show_genres (show_id, genre_id) \
                 SELECT s.id, g.id FROM (SELECT id, title, row_number() OVER () AS n FROM shows) s, \
                        genres g \
                  WHERE (g.slug = 'rare' AND s.title LIKE 'Needle %') \
                     OR (g.slug = 'common' AND s.n % 2 = 0)"
            ),
            "VACUUM ANALYZE movies, movie_entries, shows, seasons, episodes, files, genres, \
             movie_genres, show_genres"
                .to_string(),
        ];
        for sql in statements {
            db.execute_unprepared(&sql).await.expect(&sql);
        }
    }

    /// Every node of a JSON plan, depth first.
    fn nodes(plan: &serde_json::Value, into: &mut Vec<serde_json::Value>) {
        into.push(plan.clone());
        for child in plan["Plans"].as_array().into_iter().flatten() {
            nodes(child, into);
        }
    }

    /// The plan of exactly the statement `browse` runs for `query`, with its
    /// own bound values.
    /// Exactly the statement `browse` runs for `query`, its genre resolved.
    async fn statement_of(db: &DatabaseConnection, query: &CatalogQuery) -> Statement {
        SqlCatalogRepository::new(std::sync::Arc::new(db.clone()))
            .statement(query)
            .await
            .expect("a statement")
            .expect("a genre the catalogue has")
    }

    async fn plan_of(db: &DatabaseConnection, query: &CatalogQuery) -> Vec<serde_json::Value> {
        let statement = statement_of(db, query).await;
        let explain = Statement {
            sql: format!("EXPLAIN (FORMAT JSON) {}", statement.sql),
            ..statement
        };
        let row = db
            .query_one_raw(explain)
            .await
            .expect("explain")
            .expect("a plan");
        let document: serde_json::Value = row.try_get("", "QUERY PLAN").expect("the plan");
        let mut all = Vec::new();
        nodes(&document[0]["Plan"], &mut all);
        all
    }

    fn page(sort: CatalogSort, seek: Seek, kind: Option<TitleKind>) -> CatalogQuery {
        CatalogQuery {
            filters: CatalogFilters {
                kind,
                ..Default::default()
            },
            sort,
            seek,
            limit: NonZeroU32::new(21).expect("positive"),
        }
    }

    /// `query` reads each kind it lists through `index` on that kind's table,
    /// and nothing -- no title, no file -- sequentially, hashed or sorted:
    /// each branch arrives in order from its index and the outer query only
    /// merges them.
    #[track_caller]
    fn assert_index_served(plan: &[serde_json::Value], query: &CatalogQuery, index: &str) {
        let shown = serde_json::to_string_pretty(&plan[0]).unwrap_or_default();
        for node in plan {
            let node_type = node["Node Type"].as_str().unwrap_or_default();
            assert_ne!(
                node_type, "Seq Scan",
                "{query:?} reads a table whole: {shown}"
            );
            assert_ne!(node_type, "Hash", "{query:?} hashes a whole table: {shown}");
            assert_ne!(node_type, "Sort", "{query:?} sorts a branch: {shown}");
        }
        for (kind, table) in [(TitleKind::Movie, "movies"), (TitleKind::Show, "shows")] {
            let expected = format!("idx_{table}_{index}");
            let listed = query.filters.kind.is_none_or(|only| only == kind);
            let read = plan
                .iter()
                .any(|node| node["Index Name"].as_str() == Some(expected.as_str()));
            assert_eq!(read, listed, "{query:?} and {expected}: {shown}");
        }
    }

    #[tokio::test]
    async fn every_indexed_sort_pages_through_its_index_in_either_direction() {
        let schema = ScopedSchema::create_migrated("catalog_plans")
            .await
            .expect("create a migrated schema");
        let db = schema.db();
        seed_catalogue(db.as_ref()).await;
        let repo = SqlCatalogRepository::new(db.clone());

        for (field, index) in [
            (CatalogSortField::Title, "title_sort"),
            (CatalogSortField::DateAdded, "added_sort"),
        ] {
            for direction in [SortDirection::Asc, SortDirection::Desc] {
                let sort = CatalogSort { field, direction };
                let first = page(sort, Seek::Forward(None), None);
                // A real boundary the listing produced, mid-listing, so a
                // seek either way leaves thousands of titles to page through.
                let boundary: CatalogPosition = repo
                    .browse(&CatalogQuery {
                        limit: NonZeroU32::new(MID_LISTING).expect("positive"),
                        ..first.clone()
                    })
                    .await
                    .expect("the listing up to its middle")
                    .pop()
                    .expect("a listing past its middle");
                for query in [
                    first,
                    page(sort, Seek::Forward(Some(boundary.clone())), None),
                    page(sort, Seek::Backward(Some(boundary.clone())), None),
                    page(sort, Seek::Backward(None), None),
                    page(sort, Seek::Forward(None), Some(TitleKind::Movie)),
                    page(sort, Seek::Forward(Some(boundary)), Some(TitleKind::Show)),
                ] {
                    let plan = plan_of(db.as_ref(), &query).await;
                    assert_index_served(&plan, &query, index);
                }
            }
        }
    }

    /// The most title rows a page with a rare filter may read: a tenth of the
    /// catalogue. The rare titles are fewer than a page, so a page that walks
    /// the sort index checking the filter per row reads all of it -- every
    /// one of the 12,000 -- before it knows there are no more; a page that
    /// starts from what the filter admits reads about those 20.
    const RARE_READ_BOUND: f64 = ((MOVIES + SHOWS) / 10) as f64;

    /// How many movie and show rows `browse` really reads for `query`:
    /// `EXPLAIN ANALYZE` of exactly its statement under exactly its setting,
    /// every row a scan of either table produced or threw away, over all its
    /// loops. With the plan, for a failure to show.
    async fn title_rows_read(db: &DatabaseConnection, query: &CatalogQuery) -> (f64, String) {
        use sea_orm::TransactionTrait;

        let statement = statement_of(db, query).await;
        let explain = Statement {
            sql: format!("EXPLAIN (ANALYZE, FORMAT JSON) {}", statement.sql),
            ..statement
        };
        let txn = db.begin().await.expect("begin");
        if let Some(setting) = SqlCatalogRepository::setting(query) {
            txn.execute_unprepared(&setting).await.expect("the setting");
        }
        let row = txn
            .query_one_raw(explain)
            .await
            .expect("explain analyze")
            .expect("a plan");
        txn.rollback().await.expect("rollback");
        let document: serde_json::Value = row.try_get("", "QUERY PLAN").expect("the plan");
        let mut all = Vec::new();
        nodes(&document[0]["Plan"], &mut all);
        let count = |node: &serde_json::Value, field: &str| node[field].as_f64().unwrap_or(0.0);
        let read = all
            .iter()
            .filter(|node| {
                matches!(
                    node["Relation Name"].as_str(),
                    Some("movies") | Some("shows")
                )
            })
            .map(|node| {
                (count(node, "Actual Rows")
                    + count(node, "Rows Removed by Filter")
                    + count(node, "Rows Removed by Index Recheck"))
                    * count(node, "Actual Loops")
            })
            .sum();
        (
            read,
            serde_json::to_string_pretty(&document[0]["Plan"]).unwrap_or_default(),
        )
    }

    /// NFR-301 for the filters that admit few titles (issue #247): a page
    /// with a rare genre, search, year or minimum rating reads about the
    /// titles the filter admits, not the whole sort index looking for a page
    /// of them -- sorted by title or by date added, either way, and ranked by
    /// relevance.
    #[tokio::test]
    async fn a_rare_filter_reads_what_it_admits_not_the_sort_index() {
        let schema = ScopedSchema::create_migrated("catalog_rare_plans")
            .await
            .expect("create a migrated schema");
        let db = schema.db();
        seed_catalogue(db.as_ref()).await;

        let rare = [
            CatalogFilters {
                genre_slug: Some("rare".to_string()),
                ..Default::default()
            },
            CatalogFilters {
                query: Some("needle".to_string()),
                ..Default::default()
            },
            CatalogFilters {
                year: Some(RARE_YEAR),
                ..Default::default()
            },
            CatalogFilters {
                year_from: Some(RARE_YEAR),
                ..Default::default()
            },
            CatalogFilters {
                min_rating: Some(10.0),
                ..Default::default()
            },
        ];
        let mut sorts = Vec::new();
        for field in [CatalogSortField::Title, CatalogSortField::DateAdded] {
            for direction in [SortDirection::Asc, SortDirection::Desc] {
                sorts.push(CatalogSort { field, direction });
            }
        }
        let mut over = Vec::new();
        for filters in rare {
            let mut ranked = sorts.clone();
            if filters.query.is_some() {
                ranked.push(CatalogSort {
                    field: CatalogSortField::Relevance,
                    direction: SortDirection::Asc,
                });
            }
            for sort in ranked {
                for seek in [Seek::Forward(None), Seek::Backward(None)] {
                    let query = CatalogQuery {
                        filters: filters.clone(),
                        sort,
                        seek,
                        limit: NonZeroU32::new(21).expect("positive"),
                    };
                    let (read, plan) = title_rows_read(db.as_ref(), &query).await;
                    if read > RARE_READ_BOUND {
                        over.push((format!("{query:?} reads {read} title rows"), plan));
                    }
                }
            }
        }
        // Every offender named, and the first one's plan shown.
        assert!(
            over.is_empty(),
            "{}\n\nthe first: {}",
            over.iter()
                .map(|(what, _)| what.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            over.first()
                .map(|(_, plan)| plan.as_str())
                .unwrap_or_default()
        );
    }

    /// Which plan a filtered page wants depends on its filter's value, so
    /// every execution is planned for its own: a connection that has run a
    /// common genre's page many times never settles on the generic plan,
    /// which would walk the whole sort index for a rare genre's (issue #247).
    #[tokio::test]
    async fn a_filtered_page_is_planned_for_its_own_values_every_time() {
        let schema = ScopedSchema::create_migrated("catalog_custom_plans")
            .await
            .expect("create a migrated schema");
        seed_catalogue(schema.db().as_ref()).await;
        // One connection, so every page reuses one prepared statement.
        let db = schema.pool(1).await.expect("a one-connection pool");
        let repo = SqlCatalogRepository::new(db.clone());
        let by_genre = |slug: &str| CatalogQuery {
            filters: CatalogFilters {
                genre_slug: Some(slug.to_string()),
                ..Default::default()
            },
            sort: CatalogSort {
                field: CatalogSortField::Title,
                direction: SortDirection::Asc,
            },
            seek: Seek::Forward(None),
            limit: NonZeroU32::new(21).expect("positive"),
        };

        // A rare genre's own plan costs more than the generic plan's guess
        // at an average one, which is what tempts the plan cache to switch.
        for _ in 0..12 {
            let rare = repo.browse(&by_genre("rare")).await.expect("a rare page");
            assert_eq!(rare.len(), 20, "every rare title, on one page");
        }
        repo.browse(&by_genre("common"))
            .await
            .expect("a common page");

        let row = db
            .query_one_raw(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT COALESCE(sum(generic_plans), 0)::bigint AS generic, \
                        COALESCE(sum(custom_plans), 0)::bigint AS custom \
                   FROM pg_prepared_statements WHERE statement LIKE 'SELECT t.kind, t.id%'",
            ))
            .await
            .expect("read the plan cache")
            .expect("one row");
        let generic: i64 = row.try_get("", "generic").expect("generic");
        let custom: i64 = row.try_get("", "custom").expect("custom");
        assert_eq!(custom, 13, "every page was planned, and on this connection");
        assert_eq!(generic, 0, "no page ran the generic plan");
    }
}
