//! The shared `SidecarSubtitleRepository` contract, run against real SQL,
//! plus what only a real Postgres can show: that the upsert by path is one
//! atomic statement, and that a subtitle goes with its video file.

// `PathBuf`, `Uuid` and the sidecar models come into scope with the
// contract macro below.
use std::sync::Arc;

use async_trait::async_trait;
use beam_domain::models::{CreateMediaFile, FileStatus, MediaFileContent};
use beam_domain::repositories::{FileRepository, SidecarSubtitleRepository};
use beam_index::repositories::{SqlFileRepository, SqlSidecarSubtitleRepository};
use beam_test_support::{postgres, seed};
use sea_orm::DatabaseConnection;

struct PgFixture {
    db: Arc<DatabaseConnection>,
    repo: SqlSidecarSubtitleRepository,
}

async fn video_file(db: &Arc<DatabaseConnection>, library_id: Uuid) -> Uuid {
    let movie_entry_id = seed::movie_entry(db.as_ref(), library_id)
        .await
        .expect("seed a movie entry");
    let unique = Uuid::new_v4();
    SqlFileRepository::new(db.clone())
        .create(CreateMediaFile {
            library_id,
            path: PathBuf::from(format!("/videos/{library_id}/{unique}.mkv")),
            hash: (unique.as_u128() as u64) >> 1,
            size_bytes: 1024,
            mtime: None,
            identity: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: Some(MediaFileContent::movie(movie_entry_id)),
            status: FileStatus::Known,
            classifier_version: 0,
            container_tags: None,
        })
        .await
        .expect("create a video file")
        .id
}

#[async_trait]
impl beam_domain::repositories::contract::fixture::SidecarSubtitleFixture for PgFixture {
    fn repo(&self) -> &dyn SidecarSubtitleRepository {
        &self.repo
    }

    async fn new_library(&self) -> Uuid {
        seed::library(self.db.as_ref())
            .await
            .expect("seed a library")
    }

    async fn new_video_file(&self, library_id: Uuid) -> Uuid {
        video_file(&self.db, library_id).await
    }
}

async fn setup() -> PgFixture {
    let db = postgres::connection().await;
    PgFixture {
        repo: SqlSidecarSubtitleRepository::new(db.clone()),
        db,
    }
}

beam_domain::sidecar_subtitle_repository_contract!(setup);

fn english(library_id: Uuid, file_id: Uuid, path: &PathBuf) -> UpsertSidecarSubtitle {
    UpsertSidecarSubtitle {
        file_id,
        library_id,
        path: path.clone(),
        info: SidecarInfo {
            format: SubtitleFormat::Srt,
            language: Some("eng".to_string()),
            title: None,
            is_forced: false,
            is_sdh: false,
            is_default: false,
        },
        size_bytes: 10,
        mtime: None,
    }
}

/// A scan and a watcher event can find one new subtitle at once. The upsert
/// is one `ON CONFLICT (path)` statement, so both succeed and one row is left;
/// a read-then-write pair would have both read "absent" and one fail.
#[tokio::test]
async fn concurrent_upserts_of_one_path_all_succeed_and_leave_one_row() {
    let fixture = setup().await;
    let library = seed::library(fixture.db.as_ref()).await.unwrap();
    let file = video_file(&fixture.db, library).await;
    let path = PathBuf::from(format!("/videos/{}/Movie.en.srt", Uuid::new_v4()));
    let repo = Arc::new(SqlSidecarSubtitleRepository::new(fixture.db.clone()));

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let repo = repo.clone();
        let upsert = english(library, file, &path);
        tasks.push(tokio::spawn(
            async move { repo.upsert_by_path(upsert).await },
        ));
    }
    for task in tasks {
        task.await
            .expect("task did not panic")
            .expect("every concurrent upsert succeeds");
    }
    assert_eq!(repo.find_by_file_id(file).await.unwrap().len(), 1);
}

/// Purging a video file's row takes its subtitles with it (`ON DELETE
/// CASCADE`): nothing else deletes a subtitle whose video is gone for good.
#[tokio::test]
async fn purging_a_video_file_takes_its_subtitles_with_it() {
    use beam_domain::services::{Clock, RealClock};

    let fixture = setup().await;
    let library = seed::library(fixture.db.as_ref()).await.unwrap();
    let file = video_file(&fixture.db, library).await;
    let path = PathBuf::from(format!("/videos/{}/Movie.en.srt", Uuid::new_v4()));
    fixture
        .repo
        .upsert_by_path(english(library, file, &path))
        .await
        .unwrap();

    let files = SqlFileRepository::new(fixture.db.clone());
    files
        .mark_missing(vec![file], RealClock.now())
        .await
        .unwrap();
    assert_eq!(files.purge_missing(vec![file]).await.unwrap(), 1);

    assert_eq!(fixture.repo.find_by_path(&path).await.unwrap(), None);
}

/// Deleting a library takes its subtitles with it (`ON DELETE CASCADE` on
/// `library_id`): the library-deletion flow stops the library's scan and
/// deletes the row, and no subtitle of it outlives that.
#[tokio::test]
async fn deleting_a_library_takes_its_subtitles_with_it() {
    use beam_domain::repositories::LibraryRepository;
    use beam_index::repositories::library::SqlLibraryRepository;

    let fixture = setup().await;
    let library = seed::library(fixture.db.as_ref()).await.unwrap();
    let file = video_file(&fixture.db, library).await;
    let path = PathBuf::from(format!("/videos/{}/Movie.en.srt", Uuid::new_v4()));
    fixture
        .repo
        .upsert_by_path(english(library, file, &path))
        .await
        .unwrap();

    SqlLibraryRepository::new(fixture.db.clone())
        .delete(library)
        .await
        .expect("delete the library");

    assert_eq!(fixture.repo.find_by_path(&path).await.unwrap(), None);
    assert!(
        fixture
            .repo
            .find_all_by_library(library)
            .await
            .unwrap()
            .is_empty()
    );
}
