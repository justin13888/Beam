//! SQL implementation of [`LibraryShapeRepository`] (issue #93).
//!
//! Four aggregate statements, none of which loads a row per file: the report
//! runs on a timer against whatever size of library the server holds.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, QueryResult, Statement};

use beam_domain::models::library_shape::{FilesByContentType, LibraryShape, NamedCount};
use beam_domain::repositories::LibraryShapeRepository;
use beam_domain::utils::telemetry::{FileSizeBucket, FileSizeHistogram, UNKNOWN_LABEL};

/// Only what a user could browse: a soft-deleted file (issue #179) is in no
/// count, and neither is anything reached only through one.
const PRESENT: &str = "f.missing_since IS NULL";

/// Titles, seasons and episodes that are live -- reached through at least one
/// present file -- and every library.
fn titles_sql() -> String {
    format!(
        "SELECT \
           (SELECT COUNT(*) FROM libraries) AS libraries, \
           (SELECT COUNT(DISTINCT me.movie_id) FROM files f \
              JOIN movie_entries me ON me.id = f.movie_entry_id WHERE {PRESENT}) AS movies, \
           (SELECT COUNT(DISTINCT se.show_id) FROM files f \
              JOIN episodes e ON e.id = f.episode_id \
              JOIN seasons se ON se.id = e.season_id WHERE {PRESENT}) AS shows, \
           (SELECT COUNT(DISTINCT e.season_id) FROM files f \
              JOIN episodes e ON e.id = f.episode_id WHERE {PRESENT}) AS seasons, \
           (SELECT COUNT(DISTINCT f.episode_id) FROM files f \
              WHERE {PRESENT} AND f.episode_id IS NOT NULL) AS episodes"
    )
}

/// Present files by content type, their total size, and the size histogram.
///
/// One `COUNT(*) FILTER` column per [`FileSizeBucket`], generated from
/// [`FileSizeBucket::ALL`] so the SQL and the in-memory double bucket on the
/// same boundaries. The bounds are compile-time constants, not input, so they
/// are written into the statement rather than bound.
fn files_sql() -> String {
    let buckets: Vec<String> = FileSizeBucket::ALL
        .into_iter()
        .map(|bucket| {
            let lower = bucket.lower_bound_bytes();
            let condition = match bucket.upper_bound_bytes() {
                Some(upper) => format!("f.file_size >= {lower} AND f.file_size < {upper}"),
                None => format!("f.file_size >= {lower}"),
            };
            format!("COUNT(*) FILTER (WHERE {condition}) AS {}", bucket.as_str())
        })
        .collect();
    format!(
        "SELECT \
           COUNT(*) FILTER (WHERE f.movie_entry_id IS NOT NULL) AS movie_files, \
           COUNT(*) FILTER (WHERE f.episode_id IS NOT NULL) AS episode_files, \
           COUNT(*) FILTER (WHERE f.movie_entry_id IS NULL AND f.episode_id IS NULL) \
             AS unclassified_files, \
           COALESCE(SUM(f.file_size), 0)::BIGINT AS total_bytes, \
           {} \
         FROM files f WHERE {PRESENT}",
        buckets.join(", ")
    )
}

/// Present files per container, a missing container counted as `$1`.
fn containers_sql() -> String {
    format!(
        "SELECT COALESCE(f.container_format, $1) AS name, COUNT(*) AS n \
         FROM files f WHERE {PRESENT} GROUP BY 1"
    )
}

/// Streams of present files per type and codec.
fn streams_sql() -> String {
    format!(
        "SELECT ms.stream_type::TEXT AS stream_type, ms.codec AS name, COUNT(*) AS n \
         FROM media_streams ms JOIN files f ON f.id = ms.file_id \
         WHERE {PRESENT} GROUP BY 1, 2"
    )
}

/// A non-negative count column. `COUNT` is never negative; a negative value
/// would be a driver fault, and zero is the honest reading of one.
fn count(row: &QueryResult, column: &str) -> Result<u64, DbErr> {
    let value: i64 = row.try_get("", column)?;
    Ok(u64::try_from(value).unwrap_or(0))
}

/// `NamedCount`s sorted by name, as the trait promises -- in Rust rather than
/// `ORDER BY`, so the order is bytewise whatever the database collation.
fn sorted(mut counts: Vec<NamedCount>) -> Vec<NamedCount> {
    counts.sort();
    counts
}

/// SQL-based implementation of [`LibraryShapeRepository`].
#[derive(Debug, Clone)]
pub struct SqlLibraryShapeRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlLibraryShapeRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    async fn one(&self, sql: String) -> Result<QueryResult, DbErr> {
        self.db
            .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("an aggregate returned no row".to_string()))
    }
}

#[async_trait]
impl LibraryShapeRepository for SqlLibraryShapeRepository {
    async fn shape(&self) -> Result<LibraryShape, DbErr> {
        let titles = self.one(titles_sql()).await?;
        let files = self.one(files_sql()).await?;

        let mut histogram = [0u64; FileSizeBucket::ALL.len()];
        for (slot, bucket) in histogram.iter_mut().zip(FileSizeBucket::ALL) {
            *slot = count(&files, bucket.as_str())?;
        }

        let containers = self
            .db
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                containers_sql(),
                [UNKNOWN_LABEL.into()],
            ))
            .await?
            .iter()
            .map(|row| {
                Ok(NamedCount {
                    name: row.try_get("", "name")?,
                    count: count(row, "n")?,
                })
            })
            .collect::<Result<Vec<_>, DbErr>>()?;

        let mut video = Vec::new();
        let mut audio = Vec::new();
        let mut subtitle = Vec::new();
        for row in self
            .db
            .query_all_raw(Statement::from_string(DbBackend::Postgres, streams_sql()))
            .await?
        {
            let stream_type: String = row.try_get("", "stream_type")?;
            let into = match stream_type.as_str() {
                "video" => &mut video,
                "audio" => &mut audio,
                "subtitle" => &mut subtitle,
                other => {
                    return Err(DbErr::Type(format!("unexpected stream_type {other}")));
                }
            };
            into.push(NamedCount {
                name: row.try_get("", "name")?,
                count: count(&row, "n")?,
            });
        }

        Ok(LibraryShape {
            libraries: count(&titles, "libraries")?,
            movies: count(&titles, "movies")?,
            shows: count(&titles, "shows")?,
            seasons: count(&titles, "seasons")?,
            episodes: count(&titles, "episodes")?,
            files: FilesByContentType {
                movie: count(&files, "movie_files")?,
                episode: count(&files, "episode_files")?,
                unclassified: count(&files, "unclassified_files")?,
            },
            containers: sorted(containers),
            video_codecs: sorted(video),
            audio_codecs: sorted(audio),
            subtitle_codecs: sorted(subtitle),
            file_sizes: FileSizeHistogram::from_counts(histogram),
            total_bytes: count(&files, "total_bytes")?,
        })
    }
}
