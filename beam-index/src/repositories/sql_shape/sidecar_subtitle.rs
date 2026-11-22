use super::*;
use std::path::{Path, PathBuf};

use beam_domain::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
use beam_domain::repositories::SidecarSubtitleRepository;

use crate::repositories::SqlSidecarSubtitleRepository;

/// The upsert is keyed by the path alone and rewrites everything a scan
/// re-reads, never the row's id or `created_at`; the read that follows
/// is by that same path (issue #184).
#[tokio::test]
async fn upsert_by_path_conflicts_on_the_path_and_keeps_id_and_created_at() {
    let db = connection(empty_mock());
    let repo = SqlSidecarSubtitleRepository::new(db.clone());
    let _ = repo
        .upsert_by_path(UpsertSidecarSubtitle {
            file_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            path: PathBuf::from("/videos/Movie.en.srt"),
            info: SidecarInfo {
                format: SubtitleFormat::Vtt,
                language: Some("eng".to_string()),
                title: None,
                is_forced: true,
                is_sdh: false,
                is_default: false,
            },
            size_bytes: 10,
            mtime: None,
        })
        .await;
    drop(repo);

    let sql = statements(db);
    let insert = &sql[0];
    assert_contains(insert, r#"ON CONFLICT ("path") DO UPDATE"#);
    for column in [
        "file_id",
        "library_id",
        "format",
        "language",
        "title",
        "is_forced",
        "is_sdh",
        "is_default",
        "size_bytes",
        "mtime",
        "updated_at",
    ] {
        assert_contains(insert, &format!(r#""{column}" = "excluded"."{column}""#));
    }
    for kept in ["id", "created_at", "path"] {
        assert!(
            !insert
                .sql
                .contains(&format!(r#""{kept}" = "excluded"."{kept}""#)),
            "{kept} is never rewritten: {}",
            insert.sql
        );
    }
    assert_bound(insert, "vtt");
    let read = &sql[1];
    assert_filters(read, "sidecar_subtitles", "path", "=");
    assert_bound(read, "/videos/Movie.en.srt");
}

/// Listing binds the file or the library and orders by path; deleting
/// nothing issues nothing.
#[tokio::test]
async fn listings_filter_and_order_and_an_empty_delete_is_no_statement() {
    let db = connection(empty_mock());
    let repo = SqlSidecarSubtitleRepository::new(db.clone());
    let _ = repo.find_by_file_id(Uuid::from_u128(3)).await;
    let _ = repo.find_all_by_library(Uuid::from_u128(4)).await;
    assert_eq!(repo.delete_by_ids(Vec::new()).await.unwrap(), 0);
    let _ = repo.delete_by_ids(vec![Uuid::from_u128(5)]).await;
    let _ = repo.find_by_path(Path::new("/videos/x.srt")).await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 4, "the empty delete issued nothing");
    for (statement, column, id) in [
        (&sql[0], "file_id", Uuid::from_u128(3)),
        (&sql[1], "library_id", Uuid::from_u128(4)),
    ] {
        assert_filters(statement, "sidecar_subtitles", column, "=");
        assert_bound(statement, &id.to_string());
        assert_contains(statement, r#"ORDER BY "sidecar_subtitles"."path" ASC"#);
    }
    assert!(sql[2].sql.starts_with("DELETE"), "{}", sql[2].sql);
    assert_filters(&sql[2], "sidecar_subtitles", "id", "IN");
    assert_bound(&sql[2], &Uuid::from_u128(5).to_string());
}
