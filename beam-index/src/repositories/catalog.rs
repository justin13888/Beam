//! The SQL catalogue: browse and search as one ordered, limited statement over
//! movies and shows together (issue #187).
//!
//! ```text
//! SELECT kind, id, title_key, year, rating, created_at, runtime
//!   FROM (<movies branch> UNION ALL <shows branch>) AS t
//!  WHERE <keyset predicate>
//!  ORDER BY <sort tuple>
//!  LIMIT $n
//! ```
//!
//! Each branch projects the same columns and applies every filter, liveness
//! included, to its own table; a kind filter drops the other branch. The outer
//! query only orders, seeks and limits, so one page costs one statement
//! however large the library is -- no candidate set is loaded to sort in
//! memory.
//!
//! **Ordering.** A sort is a tuple compared as a whole, which is what makes a
//! keyset seek one row comparison: `(key, kind, id)` for the fields that are
//! never null (title, date added), and `(null flag, COALESCE(key, 0), kind,
//! id)` for the ones that can be (year, rating, runtime). The flag is
//! `key IS NULL` ascending and `key IS NOT NULL` descending, so a missing value
//! sorts last in both directions while the whole tuple still runs one way --
//! a row comparison cannot mix directions. `COALESCE` keeps the comparison
//! from going `NULL`; the flag has already separated the rows it touches.
//!
//! **Seeking.** A page after a position keeps the rows whose tuple compares
//! past it in the display direction; a page before one flips both the
//! comparison and the `ORDER BY`, takes the nearest rows, and is reversed
//! into display order by the caller.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement, Value};
use uuid::Uuid;

use beam_domain::models::catalog::{
    CatalogFilters, CatalogPosition, CatalogQuery, CatalogSort, CatalogSortField, Seek,
    SortDirection, SortKey, TitleKind,
};
use beam_domain::repositories::CatalogRepository;
use beam_domain::repositories::catalog::mismatched_position;

/// Only live movies: a present file behind one of the movie's entries.
const LIVE_MOVIE: &str = "EXISTS (SELECT 1 FROM movie_entries me \
     JOIN files f ON f.movie_entry_id = me.id \
     WHERE me.movie_id = movies.id AND f.missing_since IS NULL)";

/// Only live shows: some episode with a present file.
const LIVE_SHOW: &str = "EXISTS (SELECT 1 FROM seasons se \
     JOIN episodes e ON e.season_id = se.id \
     JOIN files f ON f.episode_id = e.id \
     WHERE se.show_id = shows.id AND f.missing_since IS NULL)";

/// SQL-based implementation of the catalogue read model.
#[derive(Debug, Clone)]
pub struct SqlCatalogRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlCatalogRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

/// One row of the outer query: every column any sort reads.
#[derive(Debug, FromQueryResult)]
struct CatalogRow {
    kind: String,
    id: Uuid,
    title_key: String,
    year: Option<i32>,
    rating: Option<f32>,
    created_at: chrono::DateTime<chrono::FixedOffset>,
    runtime: Option<i32>,
}

impl CatalogRow {
    fn into_position(self, field: CatalogSortField) -> Result<CatalogPosition, DbErr> {
        let Self {
            kind,
            id,
            title_key,
            year,
            rating,
            created_at,
            runtime,
        } = self;
        let kind = TitleKind::parse(&kind).ok_or_else(|| {
            DbErr::Custom(format!("the catalogue projected an unknown kind {kind:?}"))
        })?;
        let key = match field {
            CatalogSortField::Title => SortKey::Title(title_key),
            CatalogSortField::Year => SortKey::Year(year),
            CatalogSortField::Rating => SortKey::Rating(rating),
            CatalogSortField::DateAdded => {
                SortKey::DateAdded(created_at.with_timezone(&chrono::Utc))
            }
            CatalogSortField::Runtime => SortKey::Runtime(runtime),
        };
        Ok(CatalogPosition { kind, id, key })
    }
}

/// Placeholders are numbered by the order values are pushed, so every
/// fragment that binds one goes through here.
#[derive(Debug, Default)]
struct Binds {
    values: Vec<Value>,
}

impl Binds {
    /// Bind `value`, returning its placeholder.
    fn push(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("${}", self.values.len())
    }
}

/// The `WHERE` of one branch: liveness, then every filter that is set.
///
/// The placeholders each filter binds are shared by both branches, so a
/// filter's value is bound once however many branches read it.
struct BranchFilters {
    query: Option<String>,
    genre: Option<String>,
    year: Option<String>,
    year_from: Option<String>,
    year_to: Option<String>,
    min_rating: Option<String>,
}

impl BranchFilters {
    fn bind(filters: &CatalogFilters, binds: &mut Binds) -> Self {
        let CatalogFilters {
            kind: _,
            query,
            genre_slug,
            year,
            year_from,
            year_to,
            min_rating,
        } = filters;
        Self {
            query: query.clone().map(|q| binds.push(q)),
            genre: genre_slug.clone().map(|slug| binds.push(slug)),
            year: year.map(|y| binds.push(y as i32)),
            year_from: year_from.map(|y| binds.push(y as i32)),
            year_to: year_to.map(|y| binds.push(y as i32)),
            min_rating: min_rating.map(|r| binds.push(r as i32)),
        }
    }

    fn conditions(&self, kind: TitleKind) -> Vec<String> {
        let (table, live, genre_join) = match kind {
            TitleKind::Movie => (
                "movies",
                LIVE_MOVIE,
                "movie_genres j JOIN genres g ON g.id = j.genre_id WHERE j.movie_id = movies.id",
            ),
            TitleKind::Show => (
                "shows",
                LIVE_SHOW,
                "show_genres j JOIN genres g ON g.id = j.genre_id WHERE j.show_id = shows.id",
            ),
        };
        let mut conditions = vec![live.to_string()];
        if let Some(q) = &self.query {
            conditions.push(format!(
                "(similarity({table}.title, {q}) > 0.2 OR {table}.title ILIKE '%' || {q} || '%')"
            ));
        }
        if let Some(slug) = &self.genre {
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM {genre_join} AND g.slug = {slug})"
            ));
        }
        if let Some(y) = &self.year {
            conditions.push(format!("{table}.year = {y}"));
        }
        if let Some(y) = &self.year_from {
            conditions.push(format!("{table}.year >= {y}"));
        }
        if let Some(y) = &self.year_to {
            conditions.push(format!("{table}.year <= {y}"));
        }
        if let Some(r) = &self.min_rating {
            // `10::real` keeps the product single precision, the precision
            // the rating is stored and shown in, so a 7.2 is 72 here as it
            // is on the wire.
            conditions.push(format!(
                "COALESCE({table}.rating_tmdb * 10::real, 0) >= {r}"
            ));
        }
        conditions
    }
}

fn branch(kind: TitleKind, filters: &BranchFilters) -> String {
    let (projection, table) = match kind {
        TitleKind::Movie => (
            "'movie'::text AS kind, movies.id, lower(movies.title) AS title_key, movies.year, \
             movies.rating_tmdb AS rating, movies.created_at, movies.runtime_mins AS runtime",
            "movies",
        ),
        TitleKind::Show => (
            "'show'::text AS kind, shows.id, lower(shows.title) AS title_key, shows.year, \
             shows.rating_tmdb AS rating, shows.created_at, NULL::integer AS runtime",
            "shows",
        ),
    };
    format!(
        "SELECT {projection} FROM {table} WHERE {}",
        filters.conditions(kind).join(" AND ")
    )
}

/// The ordering and seek of one page: what [`keyset_sql`] produces.
#[derive(Debug, PartialEq)]
pub struct KeysetSql {
    /// The `ORDER BY` list, without the keywords.
    pub order_by: String,
    /// The row comparison a page after (or before) a position adds, if one
    /// was given.
    pub predicate: Option<String>,
}

/// The sort tuple of `sort` over the outer query's columns, as expressions
/// running in one direction; `nullable` names the flag-guarded column.
fn sort_tuple(sort: CatalogSort) -> Vec<String> {
    let nullable = |column: &str| {
        let flag = match sort.direction {
            SortDirection::Asc => format!("(t.{column} IS NULL)"),
            SortDirection::Desc => format!("(t.{column} IS NOT NULL)"),
        };
        vec![flag, format!("COALESCE(t.{column}, 0)")]
    };
    let mut tuple = match sort.field {
        CatalogSortField::Title => vec!["t.title_key".to_string()],
        CatalogSortField::DateAdded => vec!["t.created_at".to_string()],
        CatalogSortField::Year => nullable("year"),
        CatalogSortField::Rating => nullable("rating"),
        CatalogSortField::Runtime => nullable("runtime"),
    };
    tuple.push("t.kind".to_string());
    tuple.push("t.id".to_string());
    tuple
}

/// Bind `position` as the right-hand side of [`sort_tuple`]'s comparison.
fn bind_position(
    sort: CatalogSort,
    position: &CatalogPosition,
    binds: &mut Binds,
) -> Result<Vec<String>, DbErr> {
    let flagged = |present: bool, binds: &mut Binds| {
        binds.push(match sort.direction {
            SortDirection::Asc => !present,
            SortDirection::Desc => present,
        })
    };
    let mut placeholders = match (&position.key, sort.field) {
        (SortKey::Title(title), CatalogSortField::Title) => vec![binds.push(title.clone())],
        (SortKey::DateAdded(at), CatalogSortField::DateAdded) => vec![binds.push(*at)],
        (SortKey::Year(value), CatalogSortField::Year)
        | (SortKey::Runtime(value), CatalogSortField::Runtime) => vec![
            flagged(value.is_some(), binds),
            binds.push(value.unwrap_or(0)),
        ],
        (SortKey::Rating(value), CatalogSortField::Rating) => vec![
            flagged(value.is_some(), binds),
            binds.push(value.unwrap_or(0.0)),
        ],
        (key, field) => {
            return Err(DbErr::Custom(format!(
                "a {:?} position cannot seek a listing sorted by {field:?}",
                key.field()
            )));
        }
    };
    placeholders.push(binds.push(position.kind.as_str()));
    placeholders.push(binds.push(position.id));
    Ok(placeholders)
}

/// The `ORDER BY` and keyset predicate for one page, binding the seek
/// position after whatever `binds` already holds.
fn keyset_sql(sort: CatalogSort, seek: &Seek, binds: &mut Binds) -> Result<KeysetSql, DbErr> {
    let (backward, position) = match seek {
        Seek::Forward(position) => (false, position.as_ref()),
        Seek::Backward(position) => (true, position.as_ref()),
    };
    // Which way the scan runs over the sort tuple: the display direction,
    // flipped when reading backwards.
    let ascending = (sort.direction == SortDirection::Asc) != backward;
    let tuple = sort_tuple(sort);
    let order_by = tuple
        .iter()
        .map(|expr| format!("{expr} {}", if ascending { "ASC" } else { "DESC" }))
        .collect::<Vec<_>>()
        .join(", ");
    let predicate = position
        .map(|position| -> Result<String, DbErr> {
            let values = bind_position(sort, position, binds)?;
            Ok(format!(
                "({}) {} ({})",
                tuple.join(", "),
                if ascending { ">" } else { "<" },
                values.join(", ")
            ))
        })
        .transpose()?;
    Ok(KeysetSql {
        order_by,
        predicate,
    })
}

/// The whole statement for `query`.
fn browse_statement(query: &CatalogQuery) -> Result<Statement, DbErr> {
    let mut binds = Binds::default();
    let filters = BranchFilters::bind(&query.filters, &mut binds);
    let branches: Vec<String> = [TitleKind::Movie, TitleKind::Show]
        .into_iter()
        .filter(|kind| query.filters.kind.is_none_or(|only| only == *kind))
        .map(|kind| branch(kind, &filters))
        .collect();
    let KeysetSql {
        order_by,
        predicate,
    } = keyset_sql(query.sort, &query.seek, &mut binds)?;
    let limit = binds.push(i64::from(query.limit.get()));
    let where_clause = predicate.map(|p| format!(" WHERE {p}")).unwrap_or_default();
    let sql = format!(
        "SELECT t.kind, t.id, t.title_key, t.year, t.rating, t.created_at, t.runtime \
         FROM ({}) AS t{where_clause} ORDER BY {order_by} LIMIT {limit}",
        branches.join(" UNION ALL ")
    );
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        binds.values,
    ))
}

#[async_trait]
impl CatalogRepository for SqlCatalogRepository {
    async fn browse(&self, query: &CatalogQuery) -> Result<Vec<CatalogPosition>, DbErr> {
        if let Some(err) = mismatched_position(query) {
            return Err(err);
        }
        let rows = CatalogRow::find_by_statement(browse_statement(query)?)
            .all(self.db.as_ref())
            .await?;
        let mut positions = rows
            .into_iter()
            .map(|row| row.into_position(query.sort.field))
            .collect::<Result<Vec<_>, _>>()?;
        if matches!(query.seek, Seek::Backward(_)) {
            positions.reverse();
        }
        Ok(positions)
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
