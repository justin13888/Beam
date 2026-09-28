//! Hermetic assertions on the SQL the catalogue and its hydration reads send
//! (issue #187), in the manner of `sql_shape_tests`: properties -- which branch
//! runs, what a filter binds, which way a page seeks, where the limit goes --
//! never a whole statement. That the statement is *right* against Postgres is
//! the catalogue contract's job under `pg-integration`.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use beam_domain::models::catalog::{
    CatalogFilters, CatalogPosition, CatalogQuery, CatalogSort, CatalogSortField, Seek,
    SortDirection, SortKey, TitleKind,
};
use beam_domain::repositories::{
    CatalogRepository, GenreRepository, MovieRepository, ShowRepository,
};
use proptest::prelude::*;
use sea_orm::{DatabaseConnection, DbBackend, MockDatabase, Statement, Value};
use uuid::Uuid;

use super::{Binds, SqlCatalogRepository, browse_statement, keyset, sort_tuple};
use crate::repositories::{SqlGenreRepository, SqlMovieRepository, SqlShowRepository};

fn empty_mock() -> MockDatabase {
    let no_rows: Vec<Vec<BTreeMap<String, Value>>> = (0..4).map(|_| Vec::new()).collect();
    MockDatabase::new(DbBackend::Postgres).append_query_results(no_rows)
}

fn statements(db: Arc<DatabaseConnection>) -> Vec<Statement> {
    Arc::try_unwrap(db)
        .expect("drop the repository before draining its statement log")
        .into_transaction_log()
        .into_iter()
        .flat_map(|transaction| transaction.statements().to_vec())
        .collect()
}

fn values(statement: &Statement) -> Vec<Value> {
    statement
        .values
        .as_ref()
        .map(|values| values.0.clone())
        .unwrap_or_default()
}

fn query(filters: CatalogFilters, sort: CatalogSort, seek: Seek, limit: u32) -> CatalogQuery {
    CatalogQuery {
        filters,
        sort,
        seek,
        limit: NonZeroU32::new(limit).expect("a positive limit"),
    }
}

fn by(field: CatalogSortField, direction: SortDirection) -> CatalogSort {
    CatalogSort { field, direction }
}

fn statement_for(q: &CatalogQuery) -> Statement {
    browse_statement(q).expect("a statement")
}

/// The one statement `browse` sends for `q`.
async fn sent(q: CatalogQuery) -> Statement {
    let db = Arc::new(empty_mock().into_connection());
    let repo = SqlCatalogRepository::new(db.clone());
    repo.browse(&q).await.expect("browse against no rows");
    drop(repo);
    let mut sql = statements(db);
    assert_eq!(sql.len(), 1, "one statement per page: {sql:?}");
    sql.remove(0)
}

#[tokio::test]
async fn a_kind_filter_reads_only_that_kinds_table() {
    let everything = sent(query(
        CatalogFilters::default(),
        by(CatalogSortField::Title, SortDirection::Asc),
        Seek::Forward(None),
        20,
    ))
    .await;
    assert!(everything.sql.contains("FROM movies"), "{}", everything.sql);
    assert!(everything.sql.contains("FROM shows"), "{}", everything.sql);
    assert!(everything.sql.contains("UNION ALL"), "{}", everything.sql);

    for (kind, read, skipped) in [
        (TitleKind::Movie, "FROM movies", "FROM shows"),
        (TitleKind::Show, "FROM shows", "FROM movies"),
    ] {
        let only = sent(query(
            CatalogFilters {
                kind: Some(kind),
                ..Default::default()
            },
            by(CatalogSortField::Title, SortDirection::Asc),
            Seek::Forward(None),
            20,
        ))
        .await;
        assert!(only.sql.contains(read), "{}", only.sql);
        assert!(!only.sql.contains(skipped), "{}", only.sql);
        assert!(!only.sql.contains("UNION ALL"), "{}", only.sql);
    }
}

/// Browse and search list only titles with a present file: the soft-delete
/// filter sits inside the liveness condition of both branches, whatever else
/// is filtered.
#[tokio::test]
async fn both_branches_keep_only_titles_with_a_present_file() {
    let statement = sent(query(
        CatalogFilters {
            query: Some("amelie".to_string()),
            year: Some(2001),
            ..Default::default()
        },
        by(CatalogSortField::Title, SortDirection::Asc),
        Seek::Forward(None),
        20,
    ))
    .await;
    let sql = &statement.sql;
    assert!(
        sql.contains("JOIN files f ON f.movie_entry_id = me.id"),
        "{sql}"
    );
    assert!(sql.contains("me.movie_id = movies.id"), "{sql}");
    assert!(sql.contains("JOIN files f ON f.episode_id = e.id"), "{sql}");
    assert!(sql.contains("se.show_id = shows.id"), "{sql}");
    assert_eq!(sql.matches("f.missing_since IS NULL").count(), 2, "{sql}");
}

/// Every filter binds its value once, and both branches read that one
/// placeholder -- the genre slug through each kind's own junction table, the
/// minimum rating against each kind's own rating.
#[tokio::test]
async fn each_filter_binds_once_and_both_branches_read_it() {
    let statement = sent(query(
        CatalogFilters {
            genre_slug: Some("science-fiction".to_string()),
            min_rating: Some(7.2),
            year_from: Some(1990),
            year_to: Some(2000),
            ..Default::default()
        },
        by(CatalogSortField::Title, SortDirection::Asc),
        Seek::Forward(None),
        20,
    ))
    .await;
    let sql = &statement.sql;
    let bound = values(&statement);
    let slug_at = bound
        .iter()
        .position(|v| *v == Value::from("science-fiction"))
        .expect("the slug is bound")
        + 1;
    assert_eq!(
        bound
            .iter()
            .filter(|v| **v == Value::from("science-fiction"))
            .count(),
        1
    );
    assert_eq!(
        sql.matches(&format!("g.slug = ${slug_at}")).count(),
        2,
        "{sql}"
    );
    assert!(sql.contains("FROM movie_genres j"), "{sql}");
    assert!(sql.contains("FROM show_genres j"), "{sql}");

    let rating_at = bound
        .iter()
        .position(|v| *v == Value::from(7.2_f32))
        .expect("the rating is bound")
        + 1;
    assert!(
        sql.contains(&format!("COALESCE(movies.rating_tmdb, 0) >= ${rating_at}")),
        "{sql}"
    );
    assert!(
        sql.contains(&format!("COALESCE(shows.rating_tmdb, 0) >= ${rating_at}")),
        "a show's rating is filtered too: {sql}"
    );
    let from_at = bound
        .iter()
        .position(|v| *v == Value::from(1990_i32))
        .unwrap()
        + 1;
    let to_at = bound
        .iter()
        .position(|v| *v == Value::from(2000_i32))
        .unwrap()
        + 1;
    assert!(sql.contains(&format!("movies.year >= ${from_at}")), "{sql}");
    assert!(sql.contains(&format!("shows.year <= ${to_at}")), "{sql}");
}

#[tokio::test]
async fn the_limit_is_the_last_value_and_closes_the_statement() {
    let statement = sent(query(
        CatalogFilters {
            query: Some("x".to_string()),
            ..Default::default()
        },
        by(CatalogSortField::Year, SortDirection::Desc),
        Seek::Forward(Some(CatalogPosition {
            kind: TitleKind::Show,
            id: Uuid::from_u128(7),
            key: SortKey::Year(Some(1999)),
        })),
        21,
    ))
    .await;
    let bound = values(&statement);
    assert_eq!(bound.last(), Some(&Value::from(21_i64)));
    assert!(
        statement.sql.ends_with(&format!("LIMIT ${}", bound.len())),
        "{}",
        statement.sql
    );
}

/// Which way a page scans: the display direction, flipped for a page before a
/// position. The comparison and every `ORDER BY` term follow the scan.
#[test]
fn a_page_scans_in_the_display_direction_and_backwards_against_it() {
    let position = CatalogPosition {
        kind: TitleKind::Movie,
        id: Uuid::from_u128(1),
        key: SortKey::Title("m".to_string()),
    };
    for (direction, backward, comparison, order) in [
        (SortDirection::Asc, false, " > ", "ASC"),
        (SortDirection::Desc, false, " < ", "DESC"),
        (SortDirection::Asc, true, " < ", "DESC"),
        (SortDirection::Desc, true, " > ", "ASC"),
    ] {
        let seek = if backward {
            Seek::Backward(Some(position.clone()))
        } else {
            Seek::Forward(Some(position.clone()))
        };
        let sort = by(CatalogSortField::Title, direction);
        let scan = keyset(sort, &seek, &mut Binds::default()).expect("a keyset");
        let tuple = sort_tuple(sort, "lower(movies.title)", "movies.id");
        // The position's own kind: an equal row is not past it, so strict.
        let predicate = scan
            .branch_predicate(TitleKind::Movie, &tuple)
            .expect("a position seeks");
        assert!(
            predicate.contains(comparison),
            "{direction:?} backward={backward}: {predicate}"
        );
        let order_by = scan.order_by(&tuple);
        let terms: Vec<&str> = order_by.split(", ").collect();
        assert_eq!(terms.len(), 2, "title, id: {order_by}");
        assert!(
            terms.iter().all(|term| term.ends_with(order)),
            "{direction:?} backward={backward}: {order_by}"
        );
    }
}

/// The order is `(key, id, kind)`, and a branch compares only `(key, id)`: a
/// row whose key and id equal the position's is past it exactly when its kind
/// sorts after the position's in the scan, so that branch alone compares
/// inclusively.
#[test]
fn only_the_branch_whose_kind_sorts_after_the_position_includes_an_equal_row() {
    let tuple = ["k".to_string(), "id".to_string()];
    for (from, direction, backward, movie, show) in [
        (TitleKind::Movie, SortDirection::Asc, false, " > ", " >= "),
        (TitleKind::Show, SortDirection::Asc, false, " > ", " > "),
        (TitleKind::Movie, SortDirection::Desc, false, " < ", " < "),
        (TitleKind::Show, SortDirection::Desc, false, " <= ", " < "),
        (TitleKind::Movie, SortDirection::Asc, true, " < ", " < "),
        (TitleKind::Show, SortDirection::Asc, true, " <= ", " < "),
    ] {
        let position = CatalogPosition {
            kind: from,
            id: Uuid::from_u128(1),
            key: SortKey::Title("m".to_string()),
        };
        let seek = if backward {
            Seek::Backward(Some(position))
        } else {
            Seek::Forward(Some(position))
        };
        let scan = keyset(
            by(CatalogSortField::Title, direction),
            &seek,
            &mut Binds::default(),
        )
        .expect("a keyset");
        for (kind, comparison) in [(TitleKind::Movie, movie), (TitleKind::Show, show)] {
            let predicate = scan
                .branch_predicate(kind, &tuple)
                .expect("a position seeks");
            assert_eq!(
                predicate,
                format!("(k, id){comparison}($1, $2)"),
                "{kind:?} branch from a {from:?}, {direction:?} backward={backward}"
            );
        }
    }
}

/// Each branch seeks, orders and limits itself on the columns the title index
/// covers -- `lower(title)` then the id, the kind nowhere in them -- which is
/// what lets it read that index in order; the outer query merges the branches
/// on the same tuple with the kind last.
#[tokio::test]
async fn each_branch_seeks_orders_and_limits_itself_on_its_index_columns() {
    let statement = sent(query(
        CatalogFilters::default(),
        by(CatalogSortField::Title, SortDirection::Asc),
        Seek::Forward(Some(CatalogPosition {
            kind: TitleKind::Movie,
            id: Uuid::from_u128(5),
            key: SortKey::Title("m".to_string()),
        })),
        20,
    ))
    .await;
    let sql = &statement.sql;
    let limit = format!("LIMIT ${}", values(&statement).len());
    for (table, comparison) in [("movies", ">"), ("shows", ">=")] {
        let branch = format!(
            "(lower({table}.title), {table}.id) {comparison} ($1, $2) \
             ORDER BY lower({table}.title) ASC, {table}.id ASC {limit})"
        );
        assert!(sql.contains(&branch), "{branch} in {sql}");
    }
    assert!(
        sql.ends_with(&format!(
            ") AS t ORDER BY t.title_key ASC, t.id ASC, t.kind ASC {limit}"
        )),
        "{sql}"
    );
}

/// A nullable field sorts on a flag that puts missing values last in both
/// directions, and a position binds the flag its own key has.
#[test]
fn a_nullable_key_seeks_on_a_flag_that_keeps_missing_values_last() {
    for (direction, flag, key, bound_flag) in [
        (SortDirection::Asc, "(t.year IS NULL)", Some(2001), false),
        (SortDirection::Asc, "(t.year IS NULL)", None, true),
        (
            SortDirection::Desc,
            "(t.year IS NOT NULL)",
            Some(2001),
            true,
        ),
        (SortDirection::Desc, "(t.year IS NOT NULL)", None, false),
    ] {
        let mut binds = Binds::default();
        let sort = by(CatalogSortField::Year, direction);
        let scan = keyset(
            sort,
            &Seek::Forward(Some(CatalogPosition {
                kind: TitleKind::Show,
                id: Uuid::from_u128(3),
                key: SortKey::Year(key),
            })),
            &mut binds,
        )
        .expect("a keyset");
        let tuple = sort_tuple(sort, "t.year", "t.id");
        assert!(
            scan.order_by(&tuple).starts_with(flag),
            "{direction:?}: {tuple:?}"
        );
        assert!(
            scan.branch_predicate(TitleKind::Show, &tuple)
                .expect("a position seeks")
                .starts_with(&format!("({flag}"))
        );
        assert_eq!(
            binds.values[0],
            Value::from(bound_flag),
            "{direction:?} {key:?}"
        );
        assert_eq!(binds.values[1], Value::from(key.unwrap_or(0)));
        assert_eq!(binds.values[2], Value::from(Uuid::from_u128(3)));
        assert_eq!(binds.values.len(), 3, "the kind is decided, not bound");
    }
}

fn catalog_row(kind: &str, id: u128, title: &str) -> BTreeMap<String, Value> {
    let created: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
    [
        ("kind", Value::from(kind)),
        ("id", Value::from(Uuid::from_u128(id))),
        ("title_key", Value::from(title)),
        ("year", Value::Int(None)),
        ("rating", Value::Float(None)),
        ("created_at", Value::from(created)),
        ("runtime", Value::Int(None)),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect()
}

/// A page before a position is read nearest-first and handed back in display
/// order.
#[tokio::test]
async fn a_backward_page_is_returned_in_display_order() {
    let db = Arc::new(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![
                catalog_row("show", 2, "bravo"),
                catalog_row("movie", 1, "alpha"),
            ]])
            .into_connection(),
    );
    let repo = SqlCatalogRepository::new(db);

    let page = repo
        .browse(&query(
            CatalogFilters::default(),
            by(CatalogSortField::Title, SortDirection::Asc),
            Seek::Backward(None),
            2,
        ))
        .await
        .expect("browse");

    let keys: Vec<SortKey> = page.into_iter().map(|p| p.key).collect();
    assert_eq!(
        keys,
        [
            SortKey::Title("alpha".to_string()),
            SortKey::Title("bravo".to_string())
        ]
    );
}

#[tokio::test]
async fn a_projected_kind_the_model_does_not_know_is_an_error_not_a_guess() {
    let db = Arc::new(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![catalog_row("episode", 1, "alpha")]])
            .into_connection(),
    );
    let repo = SqlCatalogRepository::new(db);
    let result = repo
        .browse(&query(
            CatalogFilters::default(),
            by(CatalogSortField::Title, SortDirection::Asc),
            Seek::Forward(None),
            2,
        ))
        .await;
    assert!(result.is_err());
}

/// A page's titles, genres and counts are read by id in one statement each,
/// and an empty page reads nothing.
#[tokio::test]
async fn hydration_reads_one_statement_per_kind_and_none_for_an_empty_page() {
    let db = Arc::new(empty_mock().into_connection());
    let movies = SqlMovieRepository::new(db.clone());
    let shows = SqlShowRepository::new(db.clone());
    let genres = SqlGenreRepository::new(db.clone());
    movies.find_by_ids(&[]).await.unwrap();
    shows.find_by_ids(&[]).await.unwrap();
    shows.child_counts(&[]).await.unwrap();
    genres.movie_genre_names(&[]).await.unwrap();
    genres.show_genre_names(&[]).await.unwrap();
    drop((movies, shows, genres));
    assert!(
        statements(db).is_empty(),
        "an empty page issues no statement"
    );

    let ids = [Uuid::from_u128(1), Uuid::from_u128(2)];
    let db = Arc::new(empty_mock().into_connection());
    let shows = SqlShowRepository::new(db.clone());
    let genres = SqlGenreRepository::new(db.clone());
    shows.child_counts(&ids).await.unwrap();
    genres.show_genre_names(&ids).await.unwrap();
    drop((shows, genres));
    let sql = statements(db);
    assert_eq!(sql.len(), 2);
    for statement in &sql {
        assert_eq!(
            values(statement),
            ids.iter().map(|id| Value::from(*id)).collect::<Vec<_>>()
        );
        assert!(statement.sql.contains("IN ($1, $2)"), "{}", statement.sql);
    }
    assert!(
        sql[0].sql.contains("LEFT JOIN episodes"),
        "a season without episodes counts: {}",
        sql[0].sql
    );
    assert!(sql[1].sql.contains("show_genres"), "{}", sql[1].sql);
}

fn any_sort() -> impl Strategy<Value = CatalogSort> {
    (
        prop_oneof![
            Just(CatalogSortField::Title),
            Just(CatalogSortField::Year),
            Just(CatalogSortField::Rating),
            Just(CatalogSortField::DateAdded),
            Just(CatalogSortField::Runtime),
        ],
        prop_oneof![Just(SortDirection::Asc), Just(SortDirection::Desc)],
    )
        .prop_map(|(field, direction)| CatalogSort { field, direction })
}

fn key_for(field: CatalogSortField, present: bool, n: i32) -> SortKey {
    match field {
        CatalogSortField::Title => SortKey::Title(format!("t{n}")),
        CatalogSortField::Year => SortKey::Year(present.then_some(n)),
        CatalogSortField::Rating => SortKey::Rating(present.then_some(n as f32 / 10.0)),
        CatalogSortField::DateAdded => SortKey::DateAdded(chrono::DateTime::UNIX_EPOCH),
        CatalogSortField::Runtime => SortKey::Runtime(present.then_some(n)),
    }
}

fn any_query() -> impl Strategy<Value = CatalogQuery> {
    (
        (
            proptest::option::of(prop_oneof![Just(TitleKind::Movie), Just(TitleKind::Show)]),
            proptest::option::of("[a-z]{1,6}"),
            proptest::option::of("[a-z-]{1,6}"),
            proptest::option::of(1900_i32..2100),
            proptest::option::of(1900_i32..2100),
            proptest::option::of(1900_i32..2100),
            proptest::option::of((0_u32..=100).prop_map(|tenths| tenths as f32 / 10.0)),
        ),
        any_sort(),
        (0_u8..3, any::<bool>(), any::<bool>(), -5_i32..5),
        1_u32..=100,
    )
        .prop_map(
            |(
                (kind, text, genre_slug, year, year_from, year_to, min_rating),
                sort,
                (seek, backward, present, n),
                limit,
            )| {
                let position = (seek > 0).then(|| CatalogPosition {
                    kind: TitleKind::Movie,
                    id: Uuid::from_u128(n.unsigned_abs().into()),
                    key: key_for(sort.field, present, n),
                });
                CatalogQuery {
                    filters: CatalogFilters {
                        kind,
                        query: text,
                        genre_slug,
                        year,
                        year_from,
                        year_to,
                        min_rating,
                    },
                    sort,
                    seek: if backward {
                        Seek::Backward(position)
                    } else {
                        Seek::Forward(position)
                    },
                    limit: NonZeroU32::new(limit).expect("positive"),
                }
            },
        )
}

/// Every `$n` in the text, in order of appearance.
fn placeholders(sql: &str) -> Vec<usize> {
    let bytes = sql.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'$' {
            let digits: String = sql[at + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(n) = digits.parse() {
                found.push(n);
            }
            at += 1 + digits.len();
        } else {
            at += 1;
        }
    }
    found
}

proptest! {
    /// Whatever is filtered, sorted and sought, the text and the values agree:
    /// no placeholder names a value that was not bound, and no bound value goes
    /// unread -- the two ways a hand-numbered statement silently breaks.
    #[test]
    fn every_placeholder_names_a_bound_value_and_every_value_is_read(q in any_query()) {
        let statement = statement_for(&q);
        let bound = values(&statement).len();
        let used = placeholders(&statement.sql);
        prop_assert!(used.iter().all(|n| (1..=bound).contains(n)), "{} with {bound} values", statement.sql);
        for n in 1..=bound {
            prop_assert!(used.contains(&n), "${n} is bound but unread in {}", statement.sql);
        }
    }
}
