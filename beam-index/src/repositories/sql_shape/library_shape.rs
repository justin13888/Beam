use super::*;
use beam_domain::models::library_shape::{FilesByContentType, NamedCount};
use beam_domain::repositories::LibraryShapeRepository;
use beam_domain::utils::telemetry::{FileSizeBucket, UNKNOWN_LABEL};

use crate::repositories::SqlLibraryShapeRepository;

fn count(n: i64) -> Value {
    Value::BigInt(Some(n))
}

fn text(s: &str) -> Value {
    Value::String(Some(s.to_string()))
}

/// A store whose four aggregates answer with distinct numbers, so a
/// column read into the wrong field shows up as a wrong number.
fn answering_mock() -> MockDatabase {
    let mut files = vec![
        ("movie_files", count(3)),
        ("episode_files", count(5)),
        ("unclassified_files", count(7)),
        ("total_bytes", count(123_456)),
    ];
    // Bucket `i` holds `100 + i` files: every bucket distinct.
    for (i, bucket) in FileSizeBucket::ALL.into_iter().enumerate() {
        files.push((bucket.as_str(), count(100 + i as i64)));
    }
    MockDatabase::new(DbBackend::Postgres)
        .append_query_results([vec![row([
            ("libraries", count(2)),
            ("movies", count(11)),
            ("shows", count(13)),
            ("seasons", count(17)),
            ("episodes", count(19)),
        ])]])
        .append_query_results([vec![row(files)]])
        .append_query_results([vec![
            row([("name", text(UNKNOWN_LABEL)), ("n", count(1))]),
            row([("name", text("avi")), ("n", count(4))]),
        ]])
        .append_query_results([vec![
            row([
                ("stream_type", text("audio")),
                ("name", text("aac")),
                ("n", count(6)),
            ]),
            row([
                ("stream_type", text("video")),
                ("name", text("hevc")),
                ("n", count(8)),
            ]),
            row([
                ("stream_type", text("video")),
                ("name", text("h264")),
                ("n", count(9)),
            ]),
            row([
                ("stream_type", text("subtitle")),
                ("name", text("subrip")),
                ("n", count(10)),
            ]),
        ]])
}

#[tokio::test]
async fn every_aggregate_lands_in_its_own_field() {
    let db = connection(answering_mock());
    let repo = SqlLibraryShapeRepository::new(db.clone());

    let shape = repo.shape().await.unwrap();

    assert_eq!(
        (
            shape.libraries,
            shape.movies,
            shape.shows,
            shape.seasons,
            shape.episodes
        ),
        (2, 11, 13, 17, 19)
    );
    assert_eq!(
        shape.files,
        FilesByContentType {
            movie: 3,
            episode: 5,
            unclassified: 7,
        }
    );
    assert_eq!(shape.total_bytes, 123_456);
    for (i, (bucket, n)) in shape.file_sizes.iter().enumerate() {
        assert_eq!(n, 100 + i as u64, "{bucket:?}");
    }
    assert_eq!(
        shape.containers,
        vec![NamedCount::new("avi", 4), NamedCount::new(UNKNOWN_LABEL, 1)],
        "sorted by name whatever order the database answered in"
    );
    assert_eq!(
        shape.video_codecs,
        vec![NamedCount::new("h264", 9), NamedCount::new("hevc", 8)]
    );
    assert_eq!(shape.audio_codecs, vec![NamedCount::new("aac", 6)]);
    assert_eq!(shape.subtitle_codecs, vec![NamedCount::new("subrip", 10)]);
}

/// A soft-deleted file (issue #179) is in no count: every aggregate that
/// reads `files` excludes it, and the container aggregate names a
/// missing container with the report's `unknown` label.
#[tokio::test]
async fn every_file_aggregate_excludes_missing_files() {
    let db = connection(answering_mock());
    let repo = SqlLibraryShapeRepository::new(db.clone());
    repo.shape().await.unwrap();
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 4);
    for statement in &sql {
        let reads = statement.sql.matches(" files f ").count();
        let filters = statement.sql.matches("f.missing_since IS NULL").count();
        assert_eq!(
            reads, filters,
            "every read of files filters missing rows:\n{}",
            statement.sql
        );
    }
    assert_bound(&sql[2], UNKNOWN_LABEL);
    // The histogram's buckets are half-open and adjacent: each bound
    // opens one bucket and closes the one before it.
    for bucket in FileSizeBucket::ALL {
        let lower = bucket.lower_bound_bytes();
        assert_contains(&sql[1], &format!("f.file_size >= {lower}"));
        if let Some(upper) = bucket.upper_bound_bytes() {
            assert_contains(&sql[1], &format!("f.file_size < {upper}"));
        }
    }
}

#[tokio::test]
async fn an_unknown_stream_type_is_an_error_not_a_silent_drop() {
    let db = connection(
        MockDatabase::new(DbBackend::Postgres)
            .append_query_results([vec![row([
                ("libraries", count(0)),
                ("movies", count(0)),
                ("shows", count(0)),
                ("seasons", count(0)),
                ("episodes", count(0)),
            ])]])
            .append_query_results([vec![row([
                ("movie_files", count(0)),
                ("episode_files", count(0)),
                ("unclassified_files", count(0)),
                ("total_bytes", count(0)),
            ]
            .into_iter()
            .chain(FileSizeBucket::ALL.map(|b| (b.as_str(), count(0)))))]])
            .append_query_results([Vec::<Row>::new()])
            .append_query_results([vec![row([
                ("stream_type", text("data")),
                ("name", text("bin_data")),
                ("n", count(1)),
            ])]]),
    );
    let repo = SqlLibraryShapeRepository::new(db);

    assert!(repo.shape().await.is_err());
}
