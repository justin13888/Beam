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
                    is_primary: true,
                })
                .await
                .unwrap();
            contents.push(MediaFileContent::Movie {
                movie_entry_id: entry.id,
            });
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
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(content),
                    status: FileStatus::Known,
                    classifier_version: 0,
                })
                .await
                .unwrap();
        }
    }
}
