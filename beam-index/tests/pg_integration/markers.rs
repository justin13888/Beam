//! The shared `MarkerRepository` contract, run against real SQL, plus what
//! only a real Postgres can show: that a file's markers and its detection
//! record go with it, and that the table's `CHECK`s refuse what the domain
//! never writes.

use std::sync::Arc;

use async_trait::async_trait;
use beam_test_support::{postgres, seed};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use uuid::Uuid;

use beam_domain::repositories::{FileRepository, MarkerRepository};
use beam_domain::services::{Clock, RealClock};
use beam_index::repositories::{SqlFileRepository, SqlMarkerRepository};

mod marker_contract {
    use super::*;

    pub(super) struct PgFixture {
        pub(super) db: Arc<DatabaseConnection>,
        pub(super) repo: SqlMarkerRepository,
    }

    #[async_trait]
    impl beam_domain::repositories::contract::fixture::MarkerFixture for PgFixture {
        fn repo(&self) -> &dyn MarkerRepository {
            &self.repo
        }

        async fn new_file(&self) -> Uuid {
            seed::file(self.db.as_ref()).await.expect("seed a file")
        }
    }

    pub(super) async fn setup() -> PgFixture {
        let db = postgres::connection().await;
        PgFixture {
            repo: SqlMarkerRepository::new(db.clone()),
            db,
        }
    }

    beam_domain::marker_repository_contract!(setup);
}

/// Purging a file takes its kept markers and its detection record with it
/// (`ON DELETE CASCADE`): nothing else deletes them once the file is gone
/// for good.
#[tokio::test]
async fn purging_a_file_takes_its_markers_and_detection_with_it() {
    use beam_domain::models::chapter::{MarkerDetection, MarkerKind, PlaybackMarker};

    let db = postgres::connection().await;
    let file = seed::file(db.as_ref()).await.unwrap();
    let markers = SqlMarkerRepository::new(db.clone());
    markers
        .set_admin(
            file,
            PlaybackMarker {
                kind: MarkerKind::Credits,
                start_ms: 1_000,
                end_ms: 2_000,
            },
        )
        .await
        .unwrap();
    markers
        .record_detection(MarkerDetection {
            file_id: file,
            file_hash: 7,
            markers: vec![PlaybackMarker {
                kind: MarkerKind::Intro,
                start_ms: 0,
                end_ms: 500,
            }],
            detected_at: RealClock.now(),
        })
        .await
        .unwrap();

    let files = SqlFileRepository::new(db.clone());
    files
        .mark_missing(vec![file], RealClock.now())
        .await
        .unwrap();
    assert_eq!(files.purge_missing(vec![file]).await.unwrap(), 1);

    assert!(markers.find_by_file_id(file).await.unwrap().is_empty());
    assert!(markers.detected_hashes(&[file]).await.unwrap().is_empty());
}

/// The table refuses a marker that does not end after it starts, and a kind
/// or source the domain has no name for: the domain never writes either, so
/// a row holding one is corruption the store keeps out.
#[tokio::test]
async fn the_table_refuses_an_empty_span_and_an_unknown_kind_or_source() {
    let db = postgres::connection().await;
    let file = seed::file(db.as_ref()).await.unwrap();
    let insert = |kind: &str, start_ms: i64, end_ms: i64, source: &str| {
        format!(
            "INSERT INTO file_markers (file_id, kind, start_ms, end_ms, source) \
             VALUES ('{file}', '{kind}', {start_ms}, {end_ms}, '{source}')"
        )
    };

    for refused in [
        insert("intro", 5_000, 5_000, "admin"),
        insert("intro", -1, 5_000, "admin"),
        insert("opening", 0, 5_000, "admin"),
        insert("intro", 0, 5_000, "user"),
    ] {
        let result = db.execute_unprepared(&refused).await;
        assert!(result.is_err(), "{refused}: {result:?}");
    }
    db.execute_unprepared(&insert("intro", 0, 5_000, "admin"))
        .await
        .expect("a well-formed marker is kept");
}
