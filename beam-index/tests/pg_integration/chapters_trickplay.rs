//! The shared `ChapterRepository` and `TrickplayRepository` contracts, run
//! against real SQL, plus what only a real Postgres can show: that both go
//! with their file, and that the tables' `CHECK`s refuse what the domain
//! never writes.

use std::sync::Arc;

use async_trait::async_trait;
use beam_test_support::{postgres, seed};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use uuid::Uuid;

use beam_domain::repositories::{ChapterRepository, FileRepository, TrickplayRepository};
use beam_domain::services::{Clock, RealClock};
use beam_index::repositories::{SqlChapterRepository, SqlFileRepository, SqlTrickplayRepository};

mod chapter_contract {
    use super::*;

    pub(super) struct PgFixture {
        pub(super) db: Arc<DatabaseConnection>,
        pub(super) repo: SqlChapterRepository,
    }

    #[async_trait]
    impl beam_domain::repositories::contract::fixture::ChapterFixture for PgFixture {
        fn repo(&self) -> &dyn ChapterRepository {
            &self.repo
        }

        async fn new_file(&self) -> Uuid {
            seed::file(self.db.as_ref()).await.expect("seed a file")
        }
    }

    pub(super) async fn setup() -> PgFixture {
        let db = postgres::connection().await;
        PgFixture {
            repo: SqlChapterRepository::new(db.clone()),
            db,
        }
    }

    beam_domain::chapter_repository_contract!(setup);
}

mod trickplay_contract {
    use super::*;

    pub(super) struct PgFixture {
        pub(super) db: Arc<DatabaseConnection>,
        pub(super) repo: SqlTrickplayRepository,
    }

    #[async_trait]
    impl beam_domain::repositories::contract::fixture::TrickplayFixture for PgFixture {
        fn repo(&self) -> &dyn TrickplayRepository {
            &self.repo
        }

        async fn new_file(&self) -> Uuid {
            seed::file(self.db.as_ref()).await.expect("seed a file")
        }
    }

    pub(super) async fn setup() -> PgFixture {
        let db = postgres::connection().await;
        PgFixture {
            repo: SqlTrickplayRepository::new(db.clone()),
            db,
        }
    }

    beam_domain::trickplay_repository_contract!(setup);
}

/// Purge a file's row, as a scan does once its grace period runs out.
async fn purge(db: &Arc<DatabaseConnection>, file_id: Uuid) {
    let files = SqlFileRepository::new(db.clone());
    files
        .mark_missing(vec![file_id], RealClock.now())
        .await
        .unwrap();
    assert_eq!(files.purge_missing(vec![file_id]).await.unwrap(), 1);
}

/// Purging a file takes its chapters and its trickplay record with it (`ON
/// DELETE CASCADE`): nothing else deletes them once the file is gone for
/// good.
#[tokio::test]
async fn purging_a_file_takes_its_chapters_and_trickplay_record_with_it() {
    use beam_domain::models::chapter::Chapter;
    use beam_domain::models::trickplay::TrickplayRecord;

    let db = postgres::connection().await;
    let file = seed::file(db.as_ref()).await.unwrap();
    let chapters = SqlChapterRepository::new(db.clone());
    let trickplay = SqlTrickplayRepository::new(db.clone());
    chapters
        .replace_for_file(
            file,
            vec![Chapter {
                index: 0,
                start_ms: 0,
                end_ms: 1_000,
                title: Some("Opening".into()),
            }],
        )
        .await
        .unwrap();
    trickplay
        .upsert(TrickplayRecord {
            file_id: file,
            file_hash: 7,
            layout: None,
            generated_at: RealClock.now(),
        })
        .await
        .unwrap();

    purge(&db, file).await;

    assert!(chapters.find_by_file_id(file).await.unwrap().is_empty());
    assert_eq!(trickplay.find(file).await.unwrap(), None);
}

/// The tables refuse a chapter that ends before it starts and a layout
/// that is half there: the domain never writes either, so a row holding one
/// is corruption the store keeps out.
#[tokio::test]
async fn the_tables_refuse_a_backwards_chapter_and_a_partial_layout() {
    let db = postgres::connection().await;
    let file = seed::file(db.as_ref()).await.unwrap();

    let backwards = db
        .execute_unprepared(&format!(
            "INSERT INTO file_chapters (file_id, chapter_index, start_ms, end_ms) \
             VALUES ('{file}', 0, 5000, 1000)"
        ))
        .await;
    assert!(backwards.is_err(), "{backwards:?}");

    let partial = db
        .execute_unprepared(&format!(
            "INSERT INTO file_trickplay (file_id, file_hash, interval_ms, generated_at) \
             VALUES ('{file}', 1, 10000, now())"
        ))
        .await;
    assert!(partial.is_err(), "{partial:?}");
}
