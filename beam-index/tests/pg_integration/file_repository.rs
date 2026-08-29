//! `SqlFileRepository` against real SQL.
//!
//! The `files.file_status` column is a Postgres *enum* type, while
//! `beam_entity::files::Model` declares the field as `String`. Nothing in the
//! hermetic tier can notice the mismatch -- the in-memory double stores a
//! `String` happily, and `MockDatabase` never type-checks a parameter. Only a
//! real Postgres does.

use std::path::PathBuf;

use beam_domain::models::{CreateMediaFile, FileStatus, MediaFileContent};
use beam_domain::repositories::FileRepository;
use beam_index::repositories::SqlFileRepository;
use beam_test_support::{postgres, seed};

#[tokio::test]
async fn create_persists_a_file_and_reads_it_back_by_path() {
    let db = postgres::connection().await;
    let library_id = seed::library(db.as_ref()).await.unwrap();
    let entry_id = seed::movie_entry(db.as_ref(), library_id).await.unwrap();
    let path = PathBuf::from(format!("/videos/{}/a.mkv", uuid::Uuid::new_v4()));

    let repo = SqlFileRepository::new(db.clone());
    let created = repo
        .create(CreateMediaFile {
            library_id,
            path: path.clone(),
            hash: 0x0123_4567_89ab_cdef,
            size_bytes: 4096,
            mime_type: Some("video/x-matroska".to_string()),
            duration: Some(std::time::Duration::from_secs(120)),
            container_format: Some("matroska".to_string()),
            status: FileStatus::Known,
            content: Some(MediaFileContent::Movie {
                movie_entry_id: entry_id,
            }),
            mtime: None,
        })
        .await
        .expect("inserting a file must work against the real schema");

    let found = repo
        .find_by_path(&path.to_string_lossy())
        .await
        .unwrap()
        .expect("the file just written is readable by path");

    assert_eq!(found.id, created.id);
    assert_eq!(found.status, FileStatus::Known);
    assert_eq!(found.hash, 0x0123_4567_89ab_cdef);
}

#[tokio::test]
async fn find_by_hash_matches_the_full_unsigned_range() {
    let db = postgres::connection().await;
    let library_id = seed::library(db.as_ref()).await.unwrap();
    let entry_id = seed::movie_entry(db.as_ref(), library_id).await.unwrap();
    // A hash above i64::MAX: it is stored in a signed BIGINT column, so the
    // round trip has to reinterpret rather than saturate.
    let hash = u64::MAX - 3;

    let repo = SqlFileRepository::new(db.clone());
    let created = repo
        .create(CreateMediaFile {
            library_id,
            path: PathBuf::from(format!("/videos/{}/b.mkv", uuid::Uuid::new_v4())),
            hash,
            size_bytes: 1,
            mime_type: None,
            duration: None,
            container_format: None,
            status: FileStatus::Unknown,
            content: Some(MediaFileContent::Movie {
                movie_entry_id: entry_id,
            }),
            mtime: None,
        })
        .await
        .unwrap();

    let found = repo.find_by_hash(hash).await.unwrap();
    assert!(
        found.iter().any(|f| f.id == created.id),
        "a hash above i64::MAX must survive the round trip through BIGINT"
    );
}
