//! That every `beam-entity` model matches the schema the migrations build.
//!
//! The hermetic tier never runs an entity against Postgres, so a column an
//! entity names that a migration renamed, or a Rust type Postgres will not
//! bind to its column -- the `file_status` enum bound as `text` was one --
//! passes every other test and fails in production on the first row. Here
//! every entity selects against the migrated schema, and the tables that can
//! be written without a whole repository behind them are written and read
//! back through the entity alone.

use std::fmt::Debug;

use sea_orm::{
    ActiveModelBehavior, ActiveModelTrait, DatabaseConnection, EntityName, EntityTrait,
    IntoActiveModel, Iterable,
};
use uuid::Uuid;

use beam_entity::admin_log::{AdminLogCategory, AdminLogLevel};
use beam_entity::files::FileStatus;
use beam_test_support::postgres::ScopedSchema;

/// Every entity's table name. A schema test compares this with the tables the
/// migrations create, so a table without an entity -- or an entity listed
/// here without its table -- is caught.
pub(crate) fn entity_table_names() -> Vec<&'static str> {
    use beam_entity::*;
    vec![
        AdminLog.table_name(),
        AppliedNfo.table_name(),
        Collection.table_name(),
        CollectionMovie.table_name(),
        Credit.table_name(),
        DeviceAuth.table_name(),
        Episode.table_name(),
        FileChapter.table_name(),
        FileTrickplay.table_name(),
        Files.table_name(),
        Genre.table_name(),
        Library.table_name(),
        LibraryMovie.table_name(),
        LibraryShow.table_name(),
        MediaStream.table_name(),
        MetadataEnrichment.table_name(),
        Movie.table_name(),
        MovieEntry.table_name(),
        MovieGenre.table_name(),
        PendingAuth.table_name(),
        Person.table_name(),
        PlaybackRebufferCount.table_name(),
        PlaybackStartCount.table_name(),
        PlaybackSwitchCount.table_name(),
        Season.table_name(),
        Session.table_name(),
        Show.table_name(),
        ShowGenre.table_name(),
        SidecarSubtitle.table_name(),
        User.table_name(),
        UserAccessPolicy.table_name(),
        UserLanguagePreference.table_name(),
        UserLibraryAccess.table_name(),
        UserListEntry.table_name(),
        WatchState.table_name(),
    ]
}

/// Selects every column `E` declares from its table: fails on a column the
/// schema does not have, whatever the table holds.
async fn selects<E: EntityTrait>(db: &DatabaseConnection, entity: E) {
    E::find()
        .all(db)
        .await
        .unwrap_or_else(|err| panic!("{}: {err}", entity.table_name()));
}

/// Inserts `model` through its entity, every column set, and reads the table
/// back through the entity: the row it wrote must be there, every column
/// decoded to what was written.
async fn round_trips<E, A>(db: &DatabaseConnection, _entity: E, model: E::Model)
where
    E: EntityTrait,
    E::Model: IntoActiveModel<A> + Clone + PartialEq + Debug,
    A: ActiveModelTrait<Entity = E> + ActiveModelBehavior + Send,
{
    let table = E::default().table_name();
    let returned = model
        .clone()
        .into_active_model()
        .reset_all()
        .insert(db)
        .await
        .unwrap_or_else(|err| panic!("{table}: insert {model:?}: {err}"));
    assert_eq!(returned, model, "{table}: the inserted row as returned");
    let rows = E::find()
        .all(db)
        .await
        .unwrap_or_else(|err| panic!("{table}: read back: {err}"));
    assert!(
        rows.contains(&model),
        "{table}: {model:?} not read back from {rows:?}"
    );
}

/// A whole second: Postgres keeps microseconds, so a sub-microsecond
/// `now()` would not read back equal.
fn at(seconds: i64) -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::from_timestamp(seconds, 0)
        .expect("in range")
        .fixed_offset()
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

#[tokio::test]
async fn every_entity_reads_and_writes_against_the_migrated_schema() {
    use beam_entity::*;

    let scoped = ScopedSchema::create_migrated("entity")
        .await
        .expect("create a migrated schema");
    let db = scoped.db();
    let db = db.as_ref();

    // Every entity, read: each column it declares exists.
    selects(db, AdminLog).await;
    selects(db, AppliedNfo).await;
    selects(db, Collection).await;
    selects(db, CollectionMovie).await;
    selects(db, Credit).await;
    selects(db, DeviceAuth).await;
    selects(db, Episode).await;
    selects(db, FileChapter).await;
    selects(db, FileTrickplay).await;
    selects(db, Files).await;
    selects(db, Genre).await;
    selects(db, Library).await;
    selects(db, LibraryMovie).await;
    selects(db, LibraryShow).await;
    selects(db, MediaStream).await;
    selects(db, MetadataEnrichment).await;
    selects(db, Movie).await;
    selects(db, MovieEntry).await;
    selects(db, MovieGenre).await;
    selects(db, PendingAuth).await;
    selects(db, Person).await;
    selects(db, PlaybackRebufferCount).await;
    selects(db, PlaybackStartCount).await;
    selects(db, PlaybackSwitchCount).await;
    selects(db, Season).await;
    selects(db, Session).await;
    selects(db, Show).await;
    selects(db, ShowGenre).await;
    selects(db, SidecarSubtitle).await;
    selects(db, User).await;
    selects(db, UserAccessPolicy).await;
    selects(db, UserLanguagePreference).await;
    selects(db, UserLibraryAccess).await;
    selects(db, UserListEntry).await;
    selects(db, WatchState).await;

    // admin_logs: every value of both Postgres enum columns binds and decodes.
    let mut n = 0_u128;
    for level in AdminLogLevel::iter() {
        for category in AdminLogCategory::iter() {
            n += 1;
            round_trips(
                db,
                AdminLog,
                admin_log::Model {
                    id: id(0xA000 + n),
                    level: level.clone(),
                    category,
                    message: format!("entry {n}"),
                    details: Some(
                        r#"{"path": "/videos/a.mkv", "count": 3}"#.parse().expect("JSON"),
                    ),
                    created_at: at(1_700_000_000),
                },
            )
            .await;
        }
    }

    round_trips(
        db,
        User,
        user::Model {
            id: id(1),
            oidc_issuer: "https://idp.example".to_string(),
            oidc_subject: "subject-1".to_string(),
            email: Some("viewer@example.com".to_string()),
            display_name: "Viewer".to_string(),
            avatar_url: Some("https://idp.example/a.png".to_string()),
            is_admin: true,
            disabled: false,
            created_at: at(1_700_000_000),
            updated_at: at(1_700_000_001),
        },
    )
    .await;
    round_trips(
        db,
        Library,
        library::Model {
            id: id(2),
            name: "Films".to_string(),
            description: Some("the films".to_string()),
            root_path: "/videos".to_string(),
            created_at: at(1_700_000_000),
            updated_at: at(1_700_000_001),
            last_scan_started_at: Some(at(1_700_000_002)),
            last_scan_finished_at: Some(at(1_700_000_003)),
            last_scan_file_count: Some(7),
        },
    )
    .await;
    round_trips(
        db,
        Movie,
        movie::Model {
            id: id(3),
            title: "Arrival".to_string(),
            identity_key: Some("arrival|2016".to_string()),
            identity_key_version: 3,
            pinned_ref: Some("tmdb:329865".to_string()),
            pin_source: Some("nfo".to_string()),
            title_localized: Some("L'Arrivée".to_string()),
            description: Some("Linguists.".to_string()),
            year: Some(2016),
            release_date: chrono::NaiveDate::from_ymd_opt(2016, 11, 11),
            runtime_mins: Some(116),
            poster_url: Some("https://img/p.jpg".to_string()),
            backdrop_url: Some("https://img/b.jpg".to_string()),
            tmdb_id: Some(329_865),
            imdb_id: Some("tt2543164".to_string()),
            tvdb_id: Some(1),
            anilist_id: Some(2),
            rating_tmdb: Some(7.6),
            rating_imdb: Some(7.9),
            content_rating: Some("PG-13".to_string()),
            created_at: at(1_700_000_000),
            updated_at: at(1_700_000_001),
        },
    )
    .await;
    round_trips(
        db,
        Show,
        show::Model {
            id: id(4),
            title: "Severance".to_string(),
            identity_key: Some("severance|2022".to_string()),
            identity_key_version: 3,
            pinned_ref: Some("tmdb:95396".to_string()),
            pin_source: Some("nfo".to_string()),
            title_localized: None,
            description: Some("Work-life balance.".to_string()),
            year: Some(2022),
            poster_url: None,
            backdrop_url: None,
            tmdb_id: Some(95_396),
            imdb_id: Some("tt11280740".to_string()),
            tvdb_id: Some(3),
            anilist_id: None,
            rating_tmdb: Some(8.4),
            content_rating: Some("TV-MA".to_string()),
            created_at: at(1_700_000_000),
            updated_at: at(1_700_000_001),
        },
    )
    .await;
    round_trips(
        db,
        Season,
        season::Model {
            id: id(5),
            show_id: id(4),
            season_number: 1,
            poster_url: Some("https://img/s.jpg".to_string()),
            first_aired: chrono::NaiveDate::from_ymd_opt(2022, 2, 18),
            last_aired: chrono::NaiveDate::from_ymd_opt(2022, 4, 8),
        },
    )
    .await;
    round_trips(
        db,
        Episode,
        episode::Model {
            id: id(6),
            season_id: id(5),
            episode_number: 2,
            title: "Half Loop".to_string(),
            description: Some("Orientation.".to_string()),
            air_date: chrono::NaiveDate::from_ymd_opt(2022, 2, 18),
            runtime_mins: Some(53),
            thumbnail_url: Some("https://img/e.jpg".to_string()),
            created_at: at(1_700_000_000),
        },
    )
    .await;
    round_trips(
        db,
        Genre,
        genre::Model {
            id: id(7),
            name: "Science Fiction".to_string(),
            slug: "science-fiction".to_string(),
        },
    )
    .await;
    round_trips(
        db,
        MovieGenre,
        movie_genre::Model {
            movie_id: id(3),
            genre_id: id(7),
        },
    )
    .await;
    round_trips(
        db,
        ShowGenre,
        show_genre::Model {
            show_id: id(4),
            genre_id: id(7),
        },
    )
    .await;
    round_trips(
        db,
        LibraryMovie,
        library_movie::Model {
            library_id: id(2),
            movie_id: id(3),
        },
    )
    .await;
    round_trips(
        db,
        LibraryShow,
        library_show::Model {
            library_id: id(2),
            show_id: id(4),
        },
    )
    .await;
    round_trips(
        db,
        Person,
        person::Model {
            id: id(8),
            external_ref: "tmdb:287".to_string(),
            name: "Amy Adams".to_string(),
            profile_url: Some("https://img/aa.jpg".to_string()),
            created_at: at(1_700_000_000),
            updated_at: at(1_700_000_001),
        },
    )
    .await;
    round_trips(
        db,
        Credit,
        credit::Model {
            id: id(9),
            person_id: id(8),
            movie_id: Some(id(3)),
            show_id: None,
            kind: "cast".to_string(),
            character: Some("Louise Banks".to_string()),
            billing_order: Some(0),
            job: None,
            department: None,
        },
    )
    .await;
    round_trips(
        db,
        Credit,
        credit::Model {
            id: id(10),
            person_id: id(8),
            movie_id: None,
            show_id: Some(id(4)),
            kind: "crew".to_string(),
            character: None,
            billing_order: None,
            job: Some("Producer".to_string()),
            department: Some("Production".to_string()),
        },
    )
    .await;
    round_trips(
        db,
        Collection,
        collection::Model {
            id: id(11),
            external_ref: "tmdb:c1".to_string(),
            name: "Arrivals".to_string(),
            poster_url: Some("https://img/c.jpg".to_string()),
            backdrop_url: None,
            created_at: at(1_700_000_000),
            updated_at: at(1_700_000_001),
        },
    )
    .await;
    round_trips(
        db,
        CollectionMovie,
        collection_movie::Model {
            movie_id: id(3),
            collection_id: id(11),
        },
    )
    .await;
    round_trips(
        db,
        MovieEntry,
        movie_entry::Model {
            id: id(12),
            library_id: id(2),
            movie_id: id(3),
            edition: Some("Director's Cut".to_string()),
            created_at: at(1_700_000_000),
        },
    )
    .await;
    // files: every value of the `file_status` enum binds and decodes.
    for (n, file_status) in (0_u128..).zip(FileStatus::iter()) {
        round_trips(
            db,
            Files,
            files::Model {
                id: id(0xF000 + n),
                movie_entry_id: Some(id(12)),
                episode_id: None,
                library_id: id(2),
                file_path: format!("/videos/arrival-{n}.mkv"),
                file_size: 4_000_000_000,
                mime_type: Some("video/x-matroska".to_string()),
                hash_xxh3: -42,
                duration_secs: Some(6960.5),
                container_format: Some("matroska".to_string()),
                language: Some("eng".to_string()),
                quality: Some("1080p".to_string()),
                release_group: Some("GRP".to_string()),
                scanned_at: at(1_700_000_000),
                updated_at: at(1_700_000_001),
                file_status,
                mtime: Some(at(1_600_000_000)),
                inode: Some(1234),
                ctime: Some(at(1_600_000_001)),
                missing_since: None,
                last_episode_number: None,
                part_number: None,
                classifier_version: 3,
                container_tags: Some(r#"{"title": "Arrival"}"#.parse().expect("JSON")),
            },
        )
        .await;
    }
    round_trips(
        db,
        WatchState,
        watch_state::Model {
            id: id(13),
            user_id: id(1),
            movie_id: None,
            episode_id: Some(id(6)),
            show_id: Some(id(4)),
            last_file_id: None,
            position_secs: 61.25,
            duration_secs: Some(3180.0),
            completed: true,
            play_count: 2,
            last_played_at: at(1_700_000_000),
            dismissed_at: Some(at(1_700_000_005)),
        },
    )
    .await;
    round_trips(
        db,
        UserListEntry,
        user_list_entry::Model {
            id: id(14),
            user_id: id(1),
            list: "favourites".to_string(),
            movie_id: Some(id(3)),
            show_id: None,
            added_at: at(1_700_000_000),
        },
    )
    .await;

    scoped.drop_schema().await.expect("drop schema");
}
