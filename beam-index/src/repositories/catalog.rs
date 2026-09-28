//! The SQL catalogue: browse and search as one ordered, limited statement over
//! movies and shows together (issue #187).
//!
//! ```text
//! SELECT kind, id, title_key, year, rating, created_at, runtime
//!   FROM ((SELECT .. FROM movies WHERE <filters> AND <seek> ORDER BY <tuple> LIMIT $n)
//!         UNION ALL
//!         (SELECT .. FROM shows  WHERE <filters> AND <seek> ORDER BY <tuple> LIMIT $n)) AS t
//!  ORDER BY <tuple>, kind
//!  LIMIT $n
//! ```
//!
//! Each branch projects the same columns, applies every filter -- liveness
//! included -- and the seek to its own table, and takes its own first `$n`
//! rows in order; a kind filter drops the other branch. The outer query merges
//! the two ordered branches and keeps the first `$n`. So a page reads at most
//! `$n` rows per branch however large the library is, when the branch's order
//! has an index to read (NFR-301).
//!
//! **Ordering.** A sort is a tuple compared as a whole, which is what makes a
//! keyset seek one row comparison: `(key, id, kind)` for the fields that are
//! never null (title, date added), and `(null flag, COALESCE(key, 0), id,
//! kind)` for the ones that can be (year, rating, runtime). The flag is
//! `key IS NULL` ascending and `key IS NOT NULL` descending, so a missing value
//! sorts last in both directions while the whole tuple still runs one way --
//! a row comparison cannot mix directions. `COALESCE` keeps the comparison
//! from going `NULL`; the flag has already separated the rows it touches.
//!
//! The kind comes **last**. Within a branch it is a constant, so the branch
//! orders and seeks on `(key, id)` alone -- exactly the prefix of
//! `idx_movies_title_sort` / `idx_shows_title_sort` (`lower(title), id`) and
//! `idx_movies_added_sort` / `idx_shows_added_sort` (`created_at, id`), so the
//! seek is an index condition and the order an index scan, with liveness a
//! nested-loop `EXISTS` per row read under the limit. The filters are checked
//! per row the same way, so an unfiltered or kind-only page stops at the page
//! size, while a selective genre, search or rating filter may walk the whole
//! index before its page fills (NFR-301). A kind in the middle of
//! the tuple, as it once was, made every page a sequential scan and a sort of
//! every live title. Year, rating and runtime have no index: their pages are
//! sorted per branch and still read every matching title.
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

/// Where a branch of `kind` reads `field` from.
///
/// The title is `lower(title)` spelled exactly as `idx_*_title_sort` index
/// it, so a branch ordered by it reads that index in order.
fn branch_column(kind: TitleKind, field: CatalogSortField) -> &'static str {
    match (kind, field) {
        (TitleKind::Movie, CatalogSortField::Title) => "lower(movies.title)",
        (TitleKind::Show, CatalogSortField::Title) => "lower(shows.title)",
        (TitleKind::Movie, CatalogSortField::Year) => "movies.year",
        (TitleKind::Show, CatalogSortField::Year) => "shows.year",
        (TitleKind::Movie, CatalogSortField::Rating) => "movies.rating_tmdb",
        (TitleKind::Show, CatalogSortField::Rating) => "shows.rating_tmdb",
        (TitleKind::Movie, CatalogSortField::DateAdded) => "movies.created_at",
        (TitleKind::Show, CatalogSortField::DateAdded) => "shows.created_at",
        (TitleKind::Movie, CatalogSortField::Runtime) => "movies.runtime_mins",
        (TitleKind::Show, CatalogSortField::Runtime) => "NULL::integer",
    }
}

/// The outer query's name for the column a branch projects `field` as.
fn outer_column(field: CatalogSortField) -> &'static str {
    match field {
        CatalogSortField::Title => "t.title_key",
        CatalogSortField::Year => "t.year",
        CatalogSortField::Rating => "t.rating",
        CatalogSortField::DateAdded => "t.created_at",
        CatalogSortField::Runtime => "t.runtime",
    }
}

fn branch(
    kind: TitleKind,
    filters: &BranchFilters,
    sort: CatalogSort,
    keyset: &Keyset,
    limit: &str,
) -> String {
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
    let tuple = sort_tuple(
        sort,
        branch_column(kind, sort.field),
        &format!("{table}.id"),
    );
    let mut conditions = filters.conditions(kind);
    conditions.extend(keyset.branch_predicate(kind, &tuple));
    format!(
        "(SELECT {projection} FROM {table} WHERE {} ORDER BY {} LIMIT {limit})",
        conditions.join(" AND "),
        keyset.order_by(&tuple),
    )
}

/// The sort tuple of `sort` over `column` and `id`, as expressions running in
/// one direction: `(key, id)` for the fields that are never null,
/// `(null flag, COALESCE(key, 0), id)` for the ones that can be.
fn sort_tuple(sort: CatalogSort, column: &str, id: &str) -> Vec<String> {
    let mut tuple = match sort.field {
        CatalogSortField::Title | CatalogSortField::DateAdded => vec![column.to_string()],
        CatalogSortField::Year | CatalogSortField::Rating | CatalogSortField::Runtime => {
            let flag = match sort.direction {
                SortDirection::Asc => format!("({column} IS NULL)"),
                SortDirection::Desc => format!("({column} IS NOT NULL)"),
            };
            vec![flag, format!("COALESCE({column}, 0)")]
        }
    };
    tuple.push(id.to_string());
    tuple
}

/// Bind `position`'s key and id as the right-hand side of [`sort_tuple`]'s
/// comparison.
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
    placeholders.push(binds.push(position.id));
    Ok(placeholders)
}

/// Which way one page scans the sort tuple, and the position it seeks from
/// once bound: what [`keyset`] produces.
#[derive(Debug, PartialEq)]
pub struct Keyset {
    /// Whether the scan runs ascending: the display direction, flipped when
    /// reading backwards.
    ascending: bool,
    /// The position's kind and the placeholders of its key and id.
    from: Option<(TitleKind, Vec<String>)>,
}

impl Keyset {
    /// `tuple` as an `ORDER BY` list in the scan direction, without the
    /// keywords.
    fn order_by(&self, tuple: &[String]) -> String {
        let direction = if self.ascending { "ASC" } else { "DESC" };
        tuple
            .iter()
            .map(|expr| format!("{expr} {direction}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The row comparison keeping a branch of `kind` to the rows past the
    /// position in the scan direction, if there is one.
    ///
    /// The whole order is `(key, id, kind)`, but a branch has one kind, so its
    /// comparison is over `(key, id)` alone -- the prefix of the title index,
    /// which makes it an index condition rather than a filter. The kind
    /// decides only a row whose `(key, id)` equals the position's: past it
    /// exactly when `kind` sorts after the position's kind in the scan, so
    /// that branch compares inclusively.
    fn branch_predicate(&self, kind: TitleKind, tuple: &[String]) -> Option<String> {
        let (from_kind, values) = self.from.as_ref()?;
        let equal_is_past = if self.ascending {
            kind > *from_kind
        } else {
            kind < *from_kind
        };
        let comparison = match (self.ascending, equal_is_past) {
            (true, true) => ">=",
            (true, false) => ">",
            (false, true) => "<=",
            (false, false) => "<",
        };
        Some(format!(
            "({}) {comparison} ({})",
            tuple.join(", "),
            values.join(", ")
        ))
    }
}

/// The scan of one page, binding the seek position after whatever `binds`
/// already holds.
fn keyset(sort: CatalogSort, seek: &Seek, binds: &mut Binds) -> Result<Keyset, DbErr> {
    let (backward, position) = match seek {
        Seek::Forward(position) => (false, position.as_ref()),
        Seek::Backward(position) => (true, position.as_ref()),
    };
    let from = position
        .map(|position| -> Result<_, DbErr> {
            Ok((position.kind, bind_position(sort, position, binds)?))
        })
        .transpose()?;
    Ok(Keyset {
        ascending: (sort.direction == SortDirection::Asc) != backward,
        from,
    })
}

/// The whole statement for `query`.
fn browse_statement(query: &CatalogQuery) -> Result<Statement, DbErr> {
    let mut binds = Binds::default();
    let filters = BranchFilters::bind(&query.filters, &mut binds);
    let keyset = keyset(query.sort, &query.seek, &mut binds)?;
    let limit = binds.push(i64::from(query.limit.get()));
    let branches: Vec<String> = [TitleKind::Movie, TitleKind::Show]
        .into_iter()
        .filter(|kind| query.filters.kind.is_none_or(|only| only == *kind))
        .map(|kind| branch(kind, &filters, query.sort, &keyset, &limit))
        .collect();
    let mut order = sort_tuple(query.sort, outer_column(query.sort.field), "t.id");
    order.push("t.kind".to_string());
    let sql = format!(
        "SELECT t.kind, t.id, t.title_key, t.year, t.rating, t.created_at, t.runtime \
         FROM ({}) AS t ORDER BY {} LIMIT {limit}",
        branches.join(" UNION ALL "),
        keyset.order_by(&order),
    );
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        binds.values,
    ))
}

impl SqlCatalogRepository {
    /// The one statement [`CatalogRepository::browse`] runs for `query`,
    /// exposed so the `pg-integration` tier can `EXPLAIN` exactly it.
    pub fn statement(query: &CatalogQuery) -> Result<Statement, DbErr> {
        browse_statement(query)
    }
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
