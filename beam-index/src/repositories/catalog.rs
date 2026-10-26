//! The SQL catalogue: browse and search as one ordered, limited statement over
//! movies and shows together (issue #187).
//!
//! ```text
//! SELECT kind, id, title_key, year, rating, created_at, runtime, relevance
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
//! nested-loop `EXISTS` per row read under the limit. A kind in the middle of
//! the tuple, as it once was, made every page a sequential scan and a sort of
//! every live title. The year, rating and runtime sorts have no index: their
//! pages are sorted per branch and still read every matching title.
//!
//! **Filtering** (issue #247). An unfiltered or kind-only page stops at the
//! page size. A filter checked per row the same way does too while most
//! titles pass it -- but one that admits fewer titles than a page walks the
//! whole sort index before it knows. So every filter is also one an index
//! can answer, and the planner, reading how many titles that filter admits,
//! starts from those titles and sorts them instead: a search through
//! `idx_*_title_trgm` (as `%` and `ILIKE`, which it indexes, under
//! [`SIMILARITY_THRESHOLD`]), a genre through `idx_*_genres_genre_id` (by the
//! id its slug resolves to, so the statistics are that genre's), a year
//! through `idx_*_year`, a minimum rating through `idx_*_rating_floor`.
//! Which plan is cheaper depends on the value, so each page is planned with
//! its own.
//!
//! **Seeking.** A page after a position keeps the rows whose tuple compares
//! past it in the display direction; a page before one flips both the
//! comparison and the `ORDER BY`, takes the nearest rows, and is reversed
//! into display order by the caller.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait, Value,
};
use uuid::Uuid;

use beam_domain::models::catalog::{
    CatalogFilters, CatalogPosition, CatalogQuery, CatalogSort, CatalogSortField, Seek,
    SortDirection, SortKey, TitleKind,
};
use beam_domain::repositories::CatalogRepository;
use beam_domain::repositories::catalog::{mismatched_position, unranked_relevance};

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
    /// The title's trigram distance from the query; `NULL` without one.
    relevance: Option<f32>,
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
            relevance,
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
            CatalogSortField::Relevance => SortKey::Relevance(relevance.ok_or_else(|| {
                DbErr::Custom("the catalogue ranked a title without a query".to_string())
            })?),
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
    /// `genre` is the id `filters.genre_slug` names, resolved beforehand.
    fn bind(filters: &CatalogFilters, genre: Option<Uuid>, binds: &mut Binds) -> Self {
        let CatalogFilters {
            kind: _,
            query,
            genre_slug: _,
            year,
            year_from,
            year_to,
            min_rating,
        } = filters;
        Self {
            query: query.clone().map(|q| binds.push(q)),
            genre: genre.map(|id| binds.push(id)),
            year: year.map(|y| binds.push(y)),
            year_from: year_from.map(|y| binds.push(y)),
            year_to: year_to.map(|y| binds.push(y)),
            min_rating: min_rating.map(|r| binds.push(r)),
        }
    }

    fn conditions(&self, kind: TitleKind) -> Vec<String> {
        let (table, live, genre_join) = match kind {
            TitleKind::Movie => (
                "movies",
                LIVE_MOVIE,
                "movie_genres j WHERE j.movie_id = movies.id",
            ),
            TitleKind::Show => (
                "shows",
                LIVE_SHOW,
                "show_genres j WHERE j.show_id = shows.id",
            ),
        };
        let mut conditions = vec![live.to_string()];
        if let Some(q) = &self.query {
            // `%` is `similarity() > pg_trgm.similarity_threshold`, which
            // `SIMILARITY_THRESHOLD` sets for the statement; unlike the
            // function, the operator and `ILIKE` are both what
            // `idx_*_title_trgm` answers, so a search can start from its
            // matches rather than from the whole sort index.
            conditions.push(format!(
                "({table}.title % {q} OR {table}.title ILIKE '%' || {q} || '%')"
            ));
        }
        if let Some(genre) = &self.genre {
            // By the genre's id, bound, never joined from its slug: the
            // planner then reads how many titles carry *this* genre from the
            // junction's statistics, where a join through `genres` leaves it
            // to assume every genre is as common as the average one -- and a
            // rare genre then walks the whole sort index for its page.
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM {genre_join} AND j.genre_id = {genre})"
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
            // Bound as a `real`, the precision the rating is stored in, so a
            // stored 7.2 meets a minimum of 7.2 exactly: in double precision
            // the stored value is 7.19999... and would miss it.
            conditions.push(format!("COALESCE({table}.rating_tmdb, 0) >= {r}"));
        }
        conditions
    }

    /// A branch's trigram distance from the query, or `NULL` with none.
    fn relevance(&self, table: &str) -> String {
        match &self.query {
            Some(q) => format!("({table}.title <-> {q})"),
            None => "NULL::real".to_string(),
        }
    }
}

/// Where a branch of `kind` reads `field` from.
///
/// The title is `lower(title)` spelled exactly as `idx_*_title_sort` index
/// it, so a branch ordered by it reads that index in order.
fn branch_column(kind: TitleKind, field: CatalogSortField, filters: &BranchFilters) -> String {
    let column = match (kind, field) {
        (TitleKind::Movie, CatalogSortField::Relevance) => return filters.relevance("movies"),
        (TitleKind::Show, CatalogSortField::Relevance) => return filters.relevance("shows"),
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
    };
    column.to_string()
}

/// The outer query's name for the column a branch projects `field` as.
fn outer_column(field: CatalogSortField) -> &'static str {
    match field {
        CatalogSortField::Title => "t.title_key",
        CatalogSortField::Year => "t.year",
        CatalogSortField::Rating => "t.rating",
        CatalogSortField::DateAdded => "t.created_at",
        CatalogSortField::Runtime => "t.runtime",
        CatalogSortField::Relevance => "t.relevance",
    }
}

fn branch(
    kind: TitleKind,
    filters: &BranchFilters,
    sort: CatalogSort,
    keyset: &Keyset,
    limit: &str,
) -> String {
    let (columns, table) = match kind {
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
    let projection = format!("{columns}, {} AS relevance", filters.relevance(table));
    let tuple = sort_tuple(
        sort,
        &branch_column(kind, sort.field, filters),
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
        CatalogSortField::Title | CatalogSortField::DateAdded | CatalogSortField::Relevance => {
            vec![column.to_string()]
        }
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
        // A `real`, as `<->` computes it, so the position's own row compares
        // equal to it rather than a double-precision neighbour.
        (SortKey::Relevance(distance), CatalogSortField::Relevance) => {
            vec![binds.push(*distance)]
        }
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

/// The whole statement for `query`, its genre filter -- if it has one --
/// already resolved to `genre`, the genre's id.
fn browse_statement(query: &CatalogQuery, genre: Option<Uuid>) -> Result<Statement, DbErr> {
    if let Some(err) = mismatched_position(query).or_else(|| unranked_relevance(query)) {
        return Err(err);
    }
    if query.filters.genre_slug.is_some() != genre.is_some() {
        return Err(DbErr::Custom(
            "a genre filter is read by the id its slug resolves to".to_string(),
        ));
    }
    let mut binds = Binds::default();
    let filters = BranchFilters::bind(&query.filters, genre, &mut binds);
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
        "SELECT t.kind, t.id, t.title_key, t.year, t.rating, t.created_at, t.runtime, \
         t.relevance FROM ({}) AS t ORDER BY {} LIMIT {limit}",
        branches.join(" UNION ALL "),
        keyset.order_by(&order),
    );
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        binds.values,
    ))
}

/// What a title search's `%` means: a similarity above 0.2, looser than
/// pg_trgm's default 0.3, so a one-letter typo in a short title still finds
/// it. Set for the browse's own transaction only, never for the session a
/// pooled connection goes back with.
pub const SIMILARITY_THRESHOLD: &str = "SET LOCAL pg_trgm.similarity_threshold = 0.2";

/// That a filtered page is planned for its own values every time it runs.
///
/// The statement is prepared once per connection and its text is the same
/// for every value of a filter; after a few executions Postgres may settle
/// on one generic plan for all of them, costed for an average genre, year or
/// search. For a filter that admits few titles that plan is the one that
/// walks the whole sort index -- the plan an index on the filter is there to
/// avoid -- so a page is never left to it. Planning a page costs well under
/// what that walk does.
pub const CUSTOM_PLAN: &str = "SET LOCAL plan_cache_mode = force_custom_plan";

/// The id of the genre a page's filter names.
#[derive(Debug, FromQueryResult)]
struct GenreId {
    id: Uuid,
}

impl SqlCatalogRepository {
    /// The genre `slug` names, if any does.
    async fn genre_id(&self, slug: &str) -> Result<Option<Uuid>, DbErr> {
        let genre = GenreId::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM genres WHERE slug = $1",
            [Value::from(slug)],
        ))
        .one(self.db.as_ref())
        .await?;
        Ok(genre.map(|genre| genre.id))
    }

    /// The one statement [`CatalogRepository::browse`] runs for `query`,
    /// exposed so the `pg-integration` tier can `EXPLAIN` exactly it -- or
    /// `None` when its genre filter names no genre, so nothing is listed and
    /// nothing is run.
    pub async fn statement(&self, query: &CatalogQuery) -> Result<Option<Statement>, DbErr> {
        let genre = match &query.filters.genre_slug {
            Some(slug) => match self.genre_id(slug).await? {
                Some(id) => Some(id),
                None => return Ok(None),
            },
            None => None,
        };
        browse_statement(query, genre).map(Some)
    }

    /// The settings `browse` runs [`Self::statement`] under, in a
    /// transaction of its own, as one command: [`CUSTOM_PLAN`] for any page
    /// an index filters, and [`SIMILARITY_THRESHOLD`] too for a search. An
    /// unfiltered or kind-only page has one plan whatever it asks, and runs
    /// alone.
    #[must_use]
    pub fn setting(query: &CatalogQuery) -> Option<String> {
        let CatalogFilters {
            kind: _,
            query: search,
            genre_slug,
            year,
            year_from,
            year_to,
            min_rating,
        } = &query.filters;
        let filtered = search.is_some()
            || genre_slug.is_some()
            || year.is_some()
            || year_from.is_some()
            || year_to.is_some()
            || min_rating.is_some();
        match (filtered, search.is_some()) {
            (false, _) => None,
            (true, false) => Some(CUSTOM_PLAN.to_string()),
            (true, true) => Some(format!("{CUSTOM_PLAN}; {SIMILARITY_THRESHOLD}")),
        }
    }
}

#[async_trait]
impl CatalogRepository for SqlCatalogRepository {
    async fn browse(&self, query: &CatalogQuery) -> Result<Vec<CatalogPosition>, DbErr> {
        // Refused before the genre is looked up, so a bad request reads
        // nothing.
        if let Some(err) = mismatched_position(query).or_else(|| unranked_relevance(query)) {
            return Err(err);
        }
        let Some(statement) = self.statement(query).await? else {
            return Ok(Vec::new());
        };
        let rows = match Self::setting(query) {
            Some(setting) => {
                let txn = self.db.begin().await?;
                txn.execute_unprepared(&setting).await?;
                let rows = CatalogRow::find_by_statement(statement).all(&txn).await?;
                txn.commit().await?;
                rows
            }
            None => {
                CatalogRow::find_by_statement(statement)
                    .all(self.db.as_ref())
                    .await?
            }
        };
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
