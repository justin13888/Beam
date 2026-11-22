use super::*;
use beam_domain::models::{CreateMovie, CreateShow};
use beam_domain::providers::enrichment::{MovieEnrichment, ShowEnrichment};
use beam_domain::repositories::{MovieRepository, ShowRepository};

use crate::repositories::{SqlMovieRepository, SqlShowRepository};

fn stored_movie() -> beam_entity::movie::Model {
    let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
    beam_entity::movie::Model {
        id: Uuid::from_u128(81),
        title: "Amelie".to_string(),
        identity_key: Some("amelie|2001".to_string()),
        pinned_ref: None,
        pin_source: None,
        identity_key_version: 1,
        title_localized: None,
        description: None,
        year: Some(2001),
        release_date: None,
        runtime_mins: None,
        poster_url: None,
        backdrop_url: None,
        tmdb_id: None,
        imdb_id: None,
        tvdb_id: None,
        anilist_id: None,
        rating_tmdb: None,
        rating_imdb: None,
        content_rating: None,
        created_at: now,
        updated_at: now,
    }
}

fn stored_show() -> beam_entity::show::Model {
    let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
    beam_entity::show::Model {
        id: Uuid::from_u128(82),
        title: "Shogun".to_string(),
        identity_key: Some("shogun|".to_string()),
        pinned_ref: None,
        pin_source: None,
        identity_key_version: 1,
        title_localized: None,
        description: None,
        year: None,
        poster_url: None,
        backdrop_url: None,
        tmdb_id: None,
        imdb_id: None,
        tvdb_id: None,
        anilist_id: None,
        rating_tmdb: None,
        content_rating: None,
        created_at: now,
        updated_at: now,
    }
}

/// Find-or-create is the atomic insert on the identity key, then a read
/// by that key -- never a lookup by display title, and never an update of
/// the row that already holds the key.
#[tokio::test]
async fn find_or_create_inserts_with_do_nothing_on_the_identity_key_then_reads_by_it() {
    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies
        .find_or_create_by_identity(CreateMovie::new("Amélie", Some(2001), None))
        .await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows
        .find_or_create_by_identity(CreateShow::new("Shogun", None))
        .await;
    drop((movies, shows));

    let sql = statements(db);
    assert_eq!(sql.len(), 4, "an insert and a read-back each: {sql:?}");
    for (insert, read, table, key) in [
        (&sql[0], &sql[1], "movies", "amelie|2001"),
        (&sql[2], &sql[3], "shows", "shogun|"),
    ] {
        assert_contains(insert, r#"ON CONFLICT ("identity_key") DO NOTHING"#);
        assert!(!insert.sql.contains("DO UPDATE"), "{}", insert.sql);
        assert_filters(read, table, "identity_key", "=");
        assert_bound(read, key);
        assert!(
            !read.sql.contains(r#""title" ="#),
            "a title is never looked up by its display title:\n{}",
            read.sql
        );
    }
}

/// Enrichment writes display fields only. An `UPDATE` that set
/// `identity_key` -- even to the value it had -- is the bug this issue
/// removed, waiting to come back.
#[tokio::test]
async fn apply_enrichment_never_writes_the_identity_key() {
    let db = connection(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![stored_movie()], vec![stored_movie()]])
            .append_query_results([vec![stored_show()], vec![stored_show()]])
            .append_exec_results((0..4).map(|_| sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })),
    );
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies
        .apply_enrichment(
            Uuid::from_u128(81),
            &MovieEnrichment {
                title: "Amélie".to_string(),
                ..Default::default()
            },
            &beam_domain::models::enrichment::FieldLocks::none(),
        )
        .await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows
        .apply_enrichment(
            Uuid::from_u128(82),
            &ShowEnrichment {
                title: "Shōgun".to_string(),
                ..Default::default()
            },
            &beam_domain::models::enrichment::FieldLocks::none(),
        )
        .await;
    drop((movies, shows));

    let updates: Vec<Statement> = statements(db)
        .into_iter()
        .filter(|s| s.sql.starts_with("UPDATE"))
        .collect();
    assert_eq!(updates.len(), 2, "one update per title: {updates:?}");
    for update in &updates {
        assert_contains(update, r#""title" ="#);
        // Only the `SET` list: `RETURNING` names every column.
        let set_list = update.sql.split(" WHERE ").next().unwrap_or_default();
        assert!(
            !set_list.contains("identity_key"),
            "enrichment must not write the identity key:\n{}",
            update.sql
        );
    }
}

/// A locked field (issue #185) is not named by enrichment's `UPDATE` at
/// all, so what an administrator set stays byte for byte; the match's ids
/// are always written.
#[tokio::test]
async fn apply_enrichment_never_writes_a_locked_column() {
    use beam_domain::models::enrichment::{FieldLocks, MetadataField};

    let db = connection(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![stored_movie()], vec![stored_movie()]])
            .append_query_results([vec![stored_show()], vec![stored_show()]])
            .append_exec_results((0..4).map(|_| sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })),
    );
    let locks: FieldLocks = [
        MetadataField::Title,
        MetadataField::Poster,
        MetadataField::Rating,
        MetadataField::Runtime,
    ]
    .into_iter()
    .collect();
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies
        .apply_enrichment(
            Uuid::from_u128(81),
            &MovieEnrichment {
                title: "Amélie".to_string(),
                tmdb_id: Some(194),
                ..Default::default()
            },
            &locks,
        )
        .await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows
        .apply_enrichment(
            Uuid::from_u128(82),
            &ShowEnrichment {
                title: "Shōgun".to_string(),
                tmdb_id: Some(126308),
                ..Default::default()
            },
            &locks,
        )
        .await;
    drop((movies, shows));

    let updates: Vec<Statement> = statements(db)
        .into_iter()
        .filter(|s| s.sql.starts_with("UPDATE"))
        .collect();
    assert_eq!(updates.len(), 2, "one update per title: {updates:?}");
    for update in &updates {
        let set_list = update.sql.split(" WHERE ").next().unwrap_or_default();
        for locked in [
            "\"title\" =",
            "\"poster_url\"",
            "\"rating_tmdb\"",
            "\"runtime_mins\"",
        ] {
            assert!(!set_list.contains(locked), "{locked}: {}", update.sql);
        }
        assert!(set_list.contains("\"description\""), "{}", update.sql);
        assert!(set_list.contains("\"tmdb_id\""), "{}", update.sql);
    }
}

/// The backfill only ever keys a keyless row, and only with a key no
/// other row holds.
#[tokio::test]
async fn assign_identity_key_touches_only_a_keyless_row_and_a_free_key() {
    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies
        .assign_identity_key(Uuid::from_u128(83), "dune|1984", 7)
        .await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows
        .assign_identity_key(Uuid::from_u128(84), "shogun|", 7)
        .await;
    drop((movies, shows));

    let sql = statements(db);
    for (statement, table, id, key) in [
        (&sql[0], "movies", Uuid::from_u128(83), "dune|1984"),
        (&sql[1], "shows", Uuid::from_u128(84), "shogun|"),
    ] {
        assert!(statement.sql.starts_with("UPDATE"), "{}", statement.sql);
        assert_filters(statement, table, "id", "=");
        assert_bound(statement, &id.to_string());
        assert_filters(statement, table, "identity_key", "IS NULL");
        assert_contains(statement, "NOT EXISTS");
        assert_bound(statement, key);
        assert_contains(statement, r#""identity_key_version" = $2"#);
        assert_eq!(bound_values(statement)[1], "SmallInt(Some(7))");
    }
}

/// A rekey replaces the key and its version on the one row, refusing a
/// key another row -- not the row itself -- holds; releasing a key needs
/// no such check.
#[tokio::test]
async fn rekey_updates_one_row_and_checks_only_other_rows_for_the_key() {
    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies
        .rekey(Uuid::from_u128(85), Some("greys anatomy|".to_string()), 7)
        .await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows
        .rekey(
            Uuid::from_u128(86),
            Some("oceans eleven|2001".to_string()),
            7,
        )
        .await;
    let _ = shows.rekey(Uuid::from_u128(87), None, 7).await;
    drop((movies, shows));

    let sql = statements(db);
    for (statement, table, id, key) in [
        (&sql[0], "movies", Uuid::from_u128(85), "greys anatomy|"),
        (&sql[1], "shows", Uuid::from_u128(86), "oceans eleven|2001"),
    ] {
        assert!(statement.sql.starts_with("UPDATE"), "{}", statement.sql);
        assert_contains(statement, r#""identity_key" = $1"#);
        assert_contains(statement, r#""identity_key_version" = $2"#);
        assert_filters(statement, table, "id", "=");
        assert_contains(statement, "NOT EXISTS");
        assert_contains(statement, r#""other"."identity_key" = "#);
        assert_contains(statement, r#""other"."id" <> "#);
        let values = bound_values(statement);
        assert!(values[0].contains(key), "{values:?}");
        assert_eq!(values[1], "SmallInt(Some(7))");
        assert_eq!(
            values
                .iter()
                .filter(|v| v.contains(&id.to_string()))
                .count(),
            2,
            "the row updated, and the row excluded from the clash check: {values:?}"
        );
    }
    let release = &sql[2];
    assert_filters(release, "shows", "id", "=");
    assert!(
        !release.sql.contains("NOT EXISTS"),
        "releasing a key clashes with nothing: {}",
        release.sql
    );
}

/// A retitle compares and replaces the title in one statement, on the one
/// row, so a title written in between is never overwritten (issue #233).
#[tokio::test]
async fn retitle_sets_the_title_only_where_it_is_still_the_expected_one() {
    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies
        .retitle_from(Uuid::from_u128(88), "Movie - CD1", "Movie")
        .await;
    drop(movies);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "one statement: {sql:?}");
    let statement = &sql[0];
    assert!(statement.sql.starts_with("UPDATE"), "{}", statement.sql);
    assert_contains(statement, r#""title" = $1"#);
    assert_filters(statement, "movies", "id", "=");
    assert_filters(statement, "movies", "title", "=");
    let values = bound_values(statement);
    assert!(values[0].contains("\"Movie\""), "the new title: {values:?}");
    assert!(
        values.iter().any(|v| v.contains("\"Movie - CD1\"")),
        "the expected title is compared: {values:?}"
    );
    assert!(
        values
            .iter()
            .any(|v| v.contains(&Uuid::from_u128(88).to_string())),
        "{values:?}"
    );
}

/// The rekey pass reads keyed titles whose key older rules derived,
/// oldest first, never a keyless one.
#[tokio::test]
async fn find_keyed_before_version_reads_stale_keys_oldest_first() {
    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies.find_keyed_before_version(7).await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows.find_keyed_before_version(7).await;
    drop((movies, shows));

    let sql = statements(db);
    for (statement, table) in [(&sql[0], "movies"), (&sql[1], "shows")] {
        assert_filters(statement, table, "identity_key", "IS NOT NULL");
        assert_filters(statement, table, "identity_key_version", "<");
        assert_eq!(bound_values(statement), vec!["SmallInt(Some(7))"]);
        assert_contains(
            statement,
            &format!(r#"ORDER BY "{table}"."created_at" ASC, "{table}"."id" ASC"#),
        );
    }
}

/// A pin is looked up first on the pin column, then -- oldest first -- on
/// the one provider-id column the pin's provider names (issue #184).
#[tokio::test]
async fn find_by_pin_reads_the_pin_then_the_one_provider_column() {
    use beam_domain::models::ProviderPin;

    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies.find_by_pin(&ProviderPin::Tmdb(603)).await;
    let _ = movies
        .find_by_pin(&ProviderPin::Imdb("tt0133093".to_string()))
        .await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows.find_by_pin(&ProviderPin::Tvdb(81189)).await;
    let _ = shows.find_by_pin(&ProviderPin::Anilist(5114)).await;
    drop((movies, shows));

    let sql = statements(db);
    for (pair, table, stored, column, id) in [
        (
            &sql[0..2],
            "movies",
            "tmdb:603",
            "tmdb_id",
            "Int(Some(603))",
        ),
        (
            &sql[2..4],
            "movies",
            "imdb:tt0133093",
            "imdb_id",
            "tt0133093",
        ),
        (
            &sql[4..6],
            "shows",
            "tvdb:81189",
            "tvdb_id",
            "Int(Some(81189))",
        ),
        (
            &sql[6..8],
            "shows",
            "anilist:5114",
            "anilist_id",
            "Int(Some(5114))",
        ),
    ] {
        let (pinned, matched) = (&pair[0], &pair[1]);
        assert_filters(pinned, table, "pinned_ref", "=");
        assert_bound(pinned, stored);
        assert_filters(matched, table, column, "=");
        let values = bound_values(matched);
        assert!(values[0].contains(id), "{values:?}");
        let (_, filter) = matched.sql.split_once("WHERE").expect("a filtered read");
        assert!(
            !filter.contains("pinned_ref"),
            "the fallback filters on the provider column alone: {}",
            matched.sql
        );
        assert_contains(
            matched,
            &format!(r#"ORDER BY "{table}"."created_at" ASC, "{table}"."id" ASC"#),
        );
    }
}

/// Pinning sets the pin and who set it on the one row, refusing a pin
/// another row -- not the row itself -- holds; an NFO's pin also refuses
/// a row an administrator pinned (FR-312), and an administrator's does not.
#[tokio::test]
async fn set_pinned_ref_updates_one_row_and_checks_only_other_rows_for_the_pin() {
    use beam_domain::models::{PinSource, ProviderPin};

    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let shows = SqlShowRepository::new(db.clone());
    for source in [PinSource::Nfo, PinSource::Admin] {
        let _ = movies
            .set_pinned_ref(Uuid::from_u128(91), &ProviderPin::Tmdb(603), source)
            .await;
        let _ = shows
            .set_pinned_ref(Uuid::from_u128(92), &ProviderPin::Tvdb(81189), source)
            .await;
    }
    drop((movies, shows));

    let sql = statements(db);
    for (statement, table, id, stored, source) in [
        (
            &sql[0],
            "movies",
            Uuid::from_u128(91),
            "tmdb:603",
            PinSource::Nfo,
        ),
        (
            &sql[1],
            "shows",
            Uuid::from_u128(92),
            "tvdb:81189",
            PinSource::Nfo,
        ),
        (
            &sql[2],
            "movies",
            Uuid::from_u128(91),
            "tmdb:603",
            PinSource::Admin,
        ),
        (
            &sql[3],
            "shows",
            Uuid::from_u128(92),
            "tvdb:81189",
            PinSource::Admin,
        ),
    ] {
        assert!(statement.sql.starts_with("UPDATE"), "{}", statement.sql);
        assert_contains(statement, r#""pinned_ref" = $1"#);
        assert_contains(statement, r#""pin_source" = $2"#);
        assert_filters(statement, table, "id", "=");
        assert_contains(statement, "NOT EXISTS");
        assert_contains(statement, r#""other"."pinned_ref" = "#);
        assert_contains(statement, r#""other"."id" <> "#);
        let values = bound_values(statement);
        assert!(
            values[1].contains(source.as_str()),
            "the source set: {values:?}"
        );
        match source {
            PinSource::Nfo => {
                assert_filters(statement, table, "pin_source", "IS NULL");
                assert_filters(statement, table, "pin_source", "<>");
                assert_eq!(
                    values.iter().filter(|v| v.contains("admin")).count(),
                    1,
                    "an administrator's pin is the one an NFO may not replace: {values:?}"
                );
            }
            PinSource::Admin => assert!(
                !statement
                    .sql
                    .contains(&format!(r#""{table}"."pin_source" <>"#)),
                "an administrator replaces any pin: {}",
                statement.sql
            ),
        }
        assert_eq!(
            values.iter().filter(|v| v.contains(stored)).count(),
            2,
            "the pin set, and the pin checked: {values:?}"
        );
        assert_eq!(
            values
                .iter()
                .filter(|v| v.contains(&id.to_string()))
                .count(),
            2,
            "the row updated, and the row excluded from the clash check: {values:?}"
        );
    }
}

/// A lookup by key binds the key to the key column, never the title.
#[tokio::test]
async fn find_by_identity_key_filters_on_the_key() {
    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies.find_by_identity_key("heat|1995").await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows.find_by_identity_key("the wire|").await;
    drop((movies, shows));

    let sql = statements(db);
    for (statement, table, key) in [
        (&sql[0], "movies", "heat|1995"),
        (&sql[1], "shows", "the wire|"),
    ] {
        assert_filters(statement, table, "identity_key", "=");
        assert_bound(statement, key);
    }
}

/// The backfill reads keyless titles oldest first, so of two legacy
/// duplicates the original -- not whichever the planner returns first --
/// takes the key.
#[tokio::test]
async fn find_unkeyed_reads_keyless_titles_oldest_first() {
    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies.find_unkeyed().await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows.find_unkeyed().await;
    drop((movies, shows));

    let sql = statements(db);
    for (statement, table) in [(&sql[0], "movies"), (&sql[1], "shows")] {
        assert_filters(statement, table, "identity_key", "IS NULL");
        assert_contains(
            statement,
            &format!(r#"ORDER BY "{table}"."created_at" ASC, "{table}"."id" ASC"#),
        );
    }
}

/// Orphan deletion walks down from the file rows -- entries (episodes)
/// before titles -- counts any file row, soft-deleted or not, and binds
/// the caller's cutoff to every step that has a `created_at`.
#[tokio::test]
async fn delete_orphaned_deletes_children_first_and_binds_the_cutoff() {
    let cutoff = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let _ = movies.delete_orphaned(cutoff).await;
    let shows = SqlShowRepository::new(db.clone());
    let _ = shows.delete_orphaned(cutoff).await;
    drop((movies, shows));

    let sql = statements(db);
    let tables: Vec<&str> = sql
        .iter()
        .map(|s| {
            s.sql
                .strip_prefix("DELETE FROM ")
                .and_then(|rest| rest.split_whitespace().next())
                .unwrap_or("")
        })
        .collect();
    assert_eq!(
        tables,
        vec!["movie_entries", "movies", "episodes", "seasons", "shows"],
        "children first, so a title is orphaned once its last child goes"
    );
    for statement in sql
        .iter()
        .filter(|s| !s.sql.starts_with("DELETE FROM seasons"))
    {
        assert_contains(statement, "created_at < $1");
        assert_bound(statement, "2023-11-14");
    }
    for statement in [&sql[0], &sql[2]] {
        assert!(
            !statement.sql.contains("missing_since"),
            "a soft-deleted file still holds its title:\n{}",
            statement.sql
        );
    }
}
