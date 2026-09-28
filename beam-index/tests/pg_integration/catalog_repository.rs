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
    /// far more than reading one page through an index.
    const MOVIES: u32 = 6_000;
    const SHOWS: u32 = 3_000;
    const EPISODES_PER_SHOW: u32 = 4;
    /// Roughly half the titles the seeded catalogue lists: every show and
    /// nine movies in ten.
    const MID_LISTING: u32 = (MOVIES / 10 * 9 + SHOWS) / 2;

    /// Seed the catalogue set-based -- one statement per table -- with every
    /// tenth movie's file missing, so liveness really filters, then refresh
    /// the planner's statistics.
    async fn seed_catalogue(db: &DatabaseConnection) {
        let library = uuid::Uuid::new_v4();
        let statements = [
            format!(
                "INSERT INTO libraries (id, name, root_path, created_at, updated_at) \
                 VALUES ('{library}', 'plans', '/plans/{library}', now(), now())"
            ),
            format!(
                "INSERT INTO movies (id, title, year, runtime_mins, rating_tmdb, created_at, updated_at) \
                 SELECT gen_random_uuid(), 'Movie ' || md5(g::text), 1950 + g % 70, 80 + g % 90, \
                        (g % 100) / 10.0, now() - g * interval '1 minute', now() \
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
                 SELECT gen_random_uuid(), 'Show ' || md5(g::text), 1950 + g % 70, (g % 100) / 10.0, \
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
            "ANALYZE movies, movie_entries, shows, seasons, episodes, files".to_string(),
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
    async fn plan_of(db: &DatabaseConnection, query: &CatalogQuery) -> Vec<serde_json::Value> {
        let statement = SqlCatalogRepository::statement(query).expect("a statement");
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
}
