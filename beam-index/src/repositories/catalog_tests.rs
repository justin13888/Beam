//! Hermetic assertions on the SQL the catalogue and its hydration reads send
//! (issue #187), in the manner of `sql_shape_tests`: properties -- which branch
//! runs, what a filter binds, which way a page seeks, where the limit goes --
//! never a whole statement. That the statement is *right* against Postgres is
//! the catalogue contract's job under `pg-integration`.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use beam_domain::models::access::{AccessPolicy, ContentRating, LibraryAccess};
use beam_domain::models::catalog::{
    CatalogFilters, CatalogPosition, CatalogQuery, CatalogSort, CatalogSortField, Seek,
    SortDirection, SortKey, TitleKind,
};
use beam_domain::repositories::{
    CatalogRepository, GenreRepository, MovieRepository, ShowRepository,
};
use proptest::prelude::*;
use sea_orm::{DatabaseConnection, DbBackend, MockDatabase, MockExecResult, Statement, Value};
use uuid::Uuid;

use super::{Binds, SqlCatalogRepository, browse_statement, keyset, sort_tuple};
use crate::repositories::{SqlGenreRepository, SqlMovieRepository, SqlShowRepository};

fn empty_mock() -> MockDatabase {
    let no_rows: Vec<Vec<BTreeMap<String, Value>>> = (0..4).map(|_| Vec::new()).collect();
    with_setting(MockDatabase::new(DbBackend::Postgres).append_query_results(no_rows))
}

/// What a search's `SET LOCAL` answers with.
fn with_setting(mock: MockDatabase) -> MockDatabase {
    mock.append_exec_results([MockExecResult {
        last_insert_id: 0,
        rows_affected: 0,
    }])
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
        access: AccessPolicy::unrestricted(),
        sort,
        seek,
        limit: NonZeroU32::new(limit).expect("a positive limit"),
    }
}

fn by(field: CatalogSortField, direction: SortDirection) -> CatalogSort {
    CatalogSort { field, direction }
}

/// The id the mock's genre lookup answers with.
const GENRE: Uuid = Uuid::from_u128(0x6e);

fn genre_row(id: Uuid) -> BTreeMap<String, Value> {
    BTreeMap::from([("id".to_string(), Value::from(id))])
}

/// A mock answering `q`'s genre lookup with [`GENRE`], if it has one, then
/// the page with no rows.
fn mock_for(q: &CatalogQuery) -> MockDatabase {
    let mock = MockDatabase::new(DbBackend::Postgres);
    let mock = if q.filters.genre_slug.is_some() {
        mock.append_query_results([vec![genre_row(GENRE)]])
    } else {
        mock
    };
    let no_rows: Vec<Vec<BTreeMap<String, Value>>> = (0..4).map(|_| Vec::new()).collect();
    with_setting(mock.append_query_results(no_rows))
}

fn statement_for(q: &CatalogQuery) -> Statement {
    browse_statement(q, q.filters.genre_slug.as_ref().map(|_| GENRE)).expect("a statement")
}

/// Every statement `browse` sends for `q`, in order.
async fn sent_all(q: &CatalogQuery) -> Vec<Statement> {
    let db = Arc::new(mock_for(q).into_connection());
    let repo = SqlCatalogRepository::new(db.clone());
    repo.browse(q).await.expect("browse against no rows");
    drop(repo);
    statements(db)
}

/// The one query `browse` sends for `q`: after its genre lookup, if it has
/// one, and alone, or for a filtered page between the `BEGIN` and the
/// settings before it and the `COMMIT` after it.
async fn sent(q: CatalogQuery) -> Statement {
    let mut sql = sent_all(&q).await;
    let lookup = usize::from(q.filters.genre_slug.is_some());
    let (expected, at) = if SqlCatalogRepository::setting(&q).is_some() {
        (4, 2)
    } else {
        (1, 0)
    };
    assert_eq!(sql.len(), lookup + expected, "one query per page: {sql:?}");
    sql.remove(lookup + at)
}

/// A genre filter is read by the id its slug names, looked up first, so the
/// page's own statement compares the junction's `genre_id` to a bound id the
/// planner can read statistics for -- and a slug naming no genre lists
/// nothing without running the page at all.
#[tokio::test]
async fn a_genre_is_looked_up_by_slug_then_filtered_by_id() {
    let by_genre = query(
        CatalogFilters {
            genre_slug: Some("science-fiction".to_string()),
            ..Default::default()
        },
        by(CatalogSortField::Title, SortDirection::Asc),
        Seek::Forward(None),
        20,
    );
    let sql = sent_all(&by_genre).await;
    assert!(
        sql[0].sql.contains("FROM genres WHERE slug = $1"),
        "{}",
        sql[0].sql
    );
    assert_eq!(values(&sql[0]), [Value::from("science-fiction")]);
    let page = &sent(by_genre.clone()).await;
    assert_eq!(
        values(page)
            .iter()
            .filter(|v| **v == Value::from(GENRE))
            .count(),
        1,
        "the id is bound once: {page:?}"
    );
    assert!(
        !values(page).contains(&Value::from("science-fiction")),
        "the slug is not the page's to read: {page:?}"
    );

    let db = Arc::new(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection(),
    );
    let repo = SqlCatalogRepository::new(db.clone());
    let page = repo
        .browse(&by_genre)
        .await
        .expect("browse an unknown genre");
    assert!(page.is_empty());
    drop(repo);
    let sql = statements(db);
    assert_eq!(sql.len(), 1, "only the lookup runs: {sql:?}");
}

/// A restricted caller's page checks, in both branches, that the title is
/// associated with a granted library and certified within the ceiling, each
/// against exactly the values the policy names; an unrestricted caller's page
/// checks neither (issue #263).
#[test]
fn an_access_policy_binds_its_libraries_and_the_certifications_within_its_ceiling() {
    let (kids, family) = (Uuid::from_u128(0x1d), Uuid::from_u128(0xfa));
    let page = |access: AccessPolicy| CatalogQuery {
        access,
        ..query(
            CatalogFilters::default(),
            by(CatalogSortField::Title, SortDirection::Asc),
            Seek::Forward(None),
            20,
        )
    };

    let open = statement_for(&page(AccessPolicy::unrestricted()));
    assert!(!open.sql.contains("library_movies"), "{}", open.sql);
    assert!(!open.sql.contains("content_rating"), "{}", open.sql);

    let restricted = statement_for(&page(AccessPolicy {
        libraries: LibraryAccess::Only([kids, family].into_iter().collect()),
        max_rating: Some(ContentRating::ParentalGuidance),
    }));
    let sql = &restricted.sql;
    let bound = values(&restricted);
    let at = |value: Value| {
        bound
            .iter()
            .position(|v| *v == value)
            .unwrap_or_else(|| panic!("{value:?} is bound: {bound:?}"))
            + 1
    };
    let libraries = format!(
        "l.library_id IN (${}, ${})",
        at(kids.into()),
        at(family.into())
    );
    assert!(
        sql.contains(&format!(
            "library_movies l WHERE l.movie_id = movies.id AND {libraries}"
        )),
        "{sql}"
    );
    assert!(
        sql.contains(&format!(
            "library_shows l WHERE l.show_id = shows.id AND {libraries}"
        )),
        "{sql}"
    );
    let within: Vec<String> = ContentRating::ParentalGuidance
        .certifications_within()
        .map(|label| format!("${}", at(label.into())))
        .collect();
    for table in ["movies", "shows"] {
        assert!(
            sql.contains(&format!(
                "{table}.content_rating IN ({})",
                within.join(", ")
            )),
            "{sql}"
        );
    }
    assert!(
        !bound.contains(&Value::from("PG-13")),
        "nothing above the ceiling is admitted: {bound:?}"
    );

    let nothing_granted = statement_for(&page(AccessPolicy {
        libraries: LibraryAccess::Only(Default::default()),
        max_rating: None,
    }));
    assert!(
        nothing_granted.sql.contains("AND FALSE"),
        "{}",
        nothing_granted.sql
    );
    assert!(!nothing_granted.sql.contains("library_movies"));
}

/// The statement never drops a genre filter it was not given the id of, nor
/// filters by an id no slug asked for.
#[test]
fn a_genre_filter_and_its_id_come_together() {
    let page = |genre_slug: Option<&str>| {
        query(
            CatalogFilters {
                genre_slug: genre_slug.map(str::to_string),
                ..Default::default()
            },
            by(CatalogSortField::Title, SortDirection::Asc),
            Seek::Forward(None),
            20,
        )
    };
    assert!(browse_statement(&page(Some("drama")), None).is_err());
    assert!(browse_statement(&page(None), Some(GENRE)).is_err());
    assert!(browse_statement(&page(Some("drama")), Some(GENRE)).is_ok());
    assert!(browse_statement(&page(None), None).is_ok());
}

/// A filtered page runs inside one transaction of its own, so its settings
/// end with it: planned for its own values, and a search at its similarity
/// threshold too. An unfiltered or kind-only page sends its query alone.
#[tokio::test]
async fn a_filtered_page_runs_under_its_settings_in_its_own_transaction() {
    use super::{CUSTOM_PLAN, SIMILARITY_THRESHOLD};

    let page = |filters| {
        query(
            filters,
            by(CatalogSortField::Title, SortDirection::Asc),
            Seek::Forward(None),
            20,
        )
    };
    let search = CatalogFilters {
        query: Some("matrix".to_string()),
        ..Default::default()
    };
    let searched = format!("{CUSTOM_PLAN}; {SIMILARITY_THRESHOLD}");
    let mut cases = vec![(search, searched.as_str(), "movies.title % $1")];
    for (filters, reads) in [
        (
            CatalogFilters {
                year: Some(1999),
                ..Default::default()
            },
            "movies.year = $1",
        ),
        (
            CatalogFilters {
                year_from: Some(1999),
                ..Default::default()
            },
            "movies.year >= $1",
        ),
        (
            CatalogFilters {
                year_to: Some(1999),
                ..Default::default()
            },
            "movies.year <= $1",
        ),
        (
            CatalogFilters {
                min_rating: Some(7.0),
                ..Default::default()
            },
            "COALESCE(movies.rating_tmdb, 0) >= $1",
        ),
    ] {
        cases.push((filters, CUSTOM_PLAN, reads));
    }
    for (filters, settings, reads) in cases {
        let q = page(filters);
        let db = Arc::new(mock_for(&q).into_connection());
        let repo = SqlCatalogRepository::new(db.clone());
        repo.browse(&q).await.expect("browse against no rows");
        drop(repo);
        let log = Arc::try_unwrap(db)
            .expect("the repository is dropped")
            .into_transaction_log();
        assert_eq!(log.len(), 1, "one transaction: {log:?}");
        let sql: Vec<&str> = log[0]
            .statements()
            .iter()
            .map(|statement| statement.sql.as_str())
            .collect();
        assert_eq!(sql.len(), 4, "{sql:?}");
        assert_eq!(sql[0], "BEGIN");
        assert_eq!(sql[1], settings, "{:?}", q.filters);
        assert!(sql[2].contains(reads), "{sql:?}");
        assert_eq!(sql[3], "COMMIT");
    }
    assert!(SIMILARITY_THRESHOLD.starts_with("SET LOCAL "));
    assert!(CUSTOM_PLAN.starts_with("SET LOCAL "));

    for filters in [
        CatalogFilters::default(),
        CatalogFilters {
            kind: Some(TitleKind::Movie),
            ..Default::default()
        },
    ] {
        let sql = sent_all(&page(filters)).await;
        assert_eq!(sql.len(), 1, "{sql:?}");
        assert!(!sql[0].sql.contains("SET "), "{}", sql[0].sql);
    }
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
/// placeholder -- the genre's id through each kind's own junction table, the
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
    let genre_at = bound
        .iter()
        .position(|v| *v == Value::from(GENRE))
        .expect("the genre's id is bound")
        + 1;
    assert_eq!(
        bound.iter().filter(|v| **v == Value::from(GENRE)).count(),
        1
    );
    assert_eq!(
        sql.matches(&format!("j.genre_id = ${genre_at}")).count(),
        2,
        "{sql}"
    );
    assert!(
        sql.contains("FROM movie_genres j WHERE j.movie_id = movies.id"),
        "{sql}"
    );
    assert!(
        sql.contains("FROM show_genres j WHERE j.show_id = shows.id"),
        "{sql}"
    );
    assert!(
        !sql.contains("JOIN genres"),
        "never joined from the slug: {sql}"
    );

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

/// A relevance page orders and seeks each branch on its title's distance from
/// the one bound query, the distance compared as the `real` `<->` yields.
#[tokio::test]
async fn a_relevance_page_ranks_each_branch_by_distance_from_the_one_query() {
    let statement = sent(query(
        CatalogFilters {
            query: Some("matrix".to_string()),
            ..Default::default()
        },
        by(CatalogSortField::Relevance, SortDirection::Asc),
        Seek::Forward(Some(CatalogPosition {
            kind: TitleKind::Movie,
            id: Uuid::from_u128(7),
            key: SortKey::Relevance(0.25),
        })),
        21,
    ))
    .await;
    let sql = &statement.sql;
    let bound = values(&statement);
    assert_eq!(
        bound
            .iter()
            .filter(|v| **v == Value::from("matrix"))
            .count(),
        1,
        "the query is bound once: {bound:?}"
    );
    let q = bound
        .iter()
        .position(|v| *v == Value::from("matrix"))
        .expect("the query is bound")
        + 1;
    let distance = bound
        .iter()
        .position(|v| *v == Value::from(0.25_f32))
        .expect("the position's distance is bound as a real")
        + 1;
    for table in ["movies", "shows"] {
        let ranked = format!("({table}.title <-> ${q})");
        assert!(
            sql.contains(&format!("ORDER BY {ranked} ASC, {table}.id ASC")),
            "{table} orders by its distance: {sql}"
        );
        assert!(
            sql.contains(&format!("({ranked}, {table}.id) ")),
            "{table} seeks on its distance: {sql}"
        );
        assert!(
            sql.contains(&format!("{ranked} AS relevance")),
            "{table} projects its distance: {sql}"
        );
    }
    assert!(sql.contains(&format!("${distance}, ")), "{sql}");
    assert!(
        sql.contains("ORDER BY t.relevance ASC, t.id ASC, t.kind ASC"),
        "{sql}"
    );
}

/// A relevance sort with no query has nothing to rank by: refused before any
/// statement is built, never ordered by an arbitrary key.
#[tokio::test]
async fn a_relevance_sort_without_a_query_sends_nothing() {
    let unranked = query(
        CatalogFilters::default(),
        by(CatalogSortField::Relevance, SortDirection::Asc),
        Seek::Forward(None),
        20,
    );
    assert!(browse_statement(&unranked, None).is_err());
    let db = Arc::new(empty_mock().into_connection());
    let repo = SqlCatalogRepository::new(db.clone());
    assert!(repo.browse(&unranked).await.is_err());
    drop(repo);
    assert!(statements(db).is_empty());
}

/// A ranked row the store sent without its distance is an error, not a
/// guessed key a cursor would then carry.
#[tokio::test]
async fn a_ranked_row_without_its_distance_is_an_error() {
    for distance in [Some(0.5_f32), None] {
        let mut row = catalog_row("movie", 1, "alpha");
        row.insert("relevance".to_string(), Value::Float(distance));
        let db = Arc::new(
            with_setting(MockDatabase::new(DbBackend::Postgres).append_query_results([vec![row]]))
                .into_connection(),
        );
        let repo = SqlCatalogRepository::new(db);
        let result = repo
            .browse(&query(
                CatalogFilters {
                    query: Some("alpha".to_string()),
                    ..Default::default()
                },
                by(CatalogSortField::Relevance, SortDirection::Asc),
                Seek::Forward(None),
                2,
            ))
            .await;
        match distance {
            Some(distance) => assert_eq!(
                result.expect("a ranked row").pop().map(|p| p.key),
                Some(SortKey::Relevance(distance))
            ),
            None => assert!(result.is_err()),
        }
    }
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
        ("relevance", Value::Float(None)),
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
            Just(CatalogSortField::Relevance),
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
        CatalogSortField::Relevance => SortKey::Relevance(n.unsigned_abs() as f32 / 10.0),
    }
}

fn any_access() -> impl Strategy<Value = AccessPolicy> {
    (
        proptest::option::of(proptest::collection::btree_set(any::<u128>(), 0..4)),
        proptest::option::of(prop::sample::select(ContentRating::ALL.to_vec())),
    )
        .prop_map(|(granted, max_rating)| AccessPolicy {
            libraries: granted.map_or(LibraryAccess::All, |ids| {
                LibraryAccess::Only(ids.into_iter().map(Uuid::from_u128).collect())
            }),
            max_rating,
        })
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
        any_access(),
    )
        .prop_map(
            |(
                (kind, text, genre_slug, year, year_from, year_to, min_rating),
                sort,
                (seek, backward, present, n),
                limit,
                access,
            )| {
                let position = (seek > 0).then(|| CatalogPosition {
                    kind: TitleKind::Movie,
                    id: Uuid::from_u128(n.unsigned_abs().into()),
                    key: key_for(sort.field, present, n),
                });
                // A relevance sort ranks by a query, so it always has one.
                let text = match sort.field {
                    CatalogSortField::Relevance => text.or_else(|| Some("q".to_string())),
                    _ => text,
                };
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
                    access,
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
