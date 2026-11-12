//! `classify_media_content`: which movie or episode a file becomes.

use super::test_harness::*;
use super::*;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::MediaFile;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use std::path::PathBuf;
use tempfile::TempDir;

// ─── classify_media_content: episode tests ────────────────────────────────

#[tokio::test]
async fn test_classify_episode_standard_s01e02() {
    let (service, _, show_repo) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let path = PathBuf::from("/media/Breaking Bad/Breaking.Bad.S01E02.mkv");

    let content = service
        .classify_for_test(&path, lib_id, Duration::from_secs(3600))
        .await
        .unwrap();

    let episode_id = match content {
        MediaFileContent::Episode { episode_id, .. } => episode_id,
        _ => panic!("expected Episode, got Movie"),
    };

    let episodes: Vec<_> = show_repo
        .episodes
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].id, episode_id);
    assert_eq!(episodes[0].episode_number, 2);

    let seasons: Vec<_> = show_repo
        .seasons
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(seasons.len(), 1);
    assert_eq!(seasons[0].season_number, 1);

    let shows: Vec<_> = show_repo.shows.lock().unwrap().values().cloned().collect();
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].title, "Breaking Bad");
}

#[tokio::test]
async fn test_classify_episode_lowercase_pattern() {
    let (service, _, show_repo) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let path = PathBuf::from("/media/My Show/show.s02e10.mp4");

    let content = service
        .classify_for_test(&path, lib_id, Duration::from_secs(1800))
        .await
        .unwrap();

    assert!(matches!(content, MediaFileContent::Episode { .. }));

    let episodes: Vec<_> = show_repo
        .episodes
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].episode_number, 10);

    let seasons: Vec<_> = show_repo
        .seasons
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(seasons[0].season_number, 2);
}

#[tokio::test]
async fn test_classify_episode_with_resolution_tag() {
    let (service, _, show_repo) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let path = PathBuf::from("/shows/Series/Series S01E01 720p.mkv");

    let content = service
        .classify_for_test(&path, lib_id, Duration::from_secs(2700))
        .await
        .unwrap();

    assert!(matches!(content, MediaFileContent::Episode { .. }));

    let episodes: Vec<_> = show_repo
        .episodes
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(episodes[0].episode_number, 1);

    let seasons: Vec<_> = show_repo
        .seasons
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(seasons[0].season_number, 1);
}

#[tokio::test]
async fn test_classify_episode_show_title_from_parent_dir() {
    let (service, _, show_repo) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let path = PathBuf::from("/media/Breaking Bad/S03E05.mkv");

    service
        .classify_for_test(&path, lib_id, Duration::from_secs(3000))
        .await
        .unwrap();

    let shows: Vec<_> = show_repo.shows.lock().unwrap().values().cloned().collect();
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].title, "Breaking Bad");
}

#[tokio::test]
async fn test_classify_episode_existing_show_reused() {
    let (service, _, show_repo) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let duration = Duration::from_secs(3600);

    // First call — creates the show
    service
        .classify_for_test(
            &PathBuf::from("/media/My Show/My.Show.S01E01.mkv"),
            lib_id,
            duration,
        )
        .await
        .unwrap();

    // Second call with same parent dir name — must reuse the existing show
    service
        .classify_for_test(
            &PathBuf::from("/media/My Show/My.Show.S01E02.mkv"),
            lib_id,
            duration,
        )
        .await
        .unwrap();

    let shows: Vec<_> = show_repo.shows.lock().unwrap().values().cloned().collect();
    assert_eq!(shows.len(), 1, "show must not be duplicated");
}

#[tokio::test]
async fn test_classify_episode_new_season_created() {
    let (service, _, show_repo) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let duration = Duration::from_secs(3600);

    service
        .classify_for_test(
            &PathBuf::from("/media/Show/ep.S01E01.mkv"),
            lib_id,
            duration,
        )
        .await
        .unwrap();

    service
        .classify_for_test(
            &PathBuf::from("/media/Show/ep.S02E01.mkv"),
            lib_id,
            duration,
        )
        .await
        .unwrap();

    let mut season_nums: Vec<u32> = show_repo
        .seasons
        .lock()
        .unwrap()
        .values()
        .map(|s| s.season_number)
        .collect();
    season_nums.sort_unstable();
    assert_eq!(season_nums, vec![1, 2]);
}

// ─── several files for one episode (#142) ─────────────────────────────────

/// A scan service over in-memory stores, with every probe succeeding and a
/// distinct hash per file so no two files look like duplicate content.
fn make_multi_source_scan_service(
    lib_repo: Arc<InMemoryLibraryRepository>,
    file_repo: Arc<InMemoryFileRepository>,
    show_repo: Arc<InMemoryShowRepository>,
) -> LocalIndexService {
    let next_hash = Arc::new(std::sync::atomic::AtomicU64::new(1));
    let mut mock_hash = MockHashService::new();
    mock_hash
        .expect_hash_async()
        .returning(move |_| Ok(next_hash.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let mut mock_media_info = MockMediaInfoService::new();
    mock_media_info
        .expect_get_video_metadata()
        .returning(|_| Ok(make_video_metadata()));
    LocalIndexService::new(
        lib_repo,
        file_repo,
        Arc::new(InMemoryMovieRepository::default()),
        show_repo,
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(mock_hash),
        Arc::new(mock_media_info),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(InMemoryAdminLogRepository::default()),
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    )
}

fn episode_id_of(file: &MediaFile) -> Uuid {
    match file.content {
        Some(MediaFileContent::Episode { episode_id, .. }) => episode_id,
        ref other => panic!("expected an episode file, got {other:?}"),
    }
}

#[tokio::test]
async fn two_rips_of_one_episode_become_two_sources_of_one_episode() {
    let lib_repo = Arc::new(InMemoryLibraryRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let show_repo = Arc::new(InMemoryShowRepository::default());
    let dir = TempDir::new().unwrap();
    let library = make_library_in_tempdir(&lib_repo, &dir).await;
    let show_dir = dir.path().join("The Show");
    std::fs::create_dir_all(&show_dir).unwrap();
    std::fs::write(show_dir.join("The.Show.S01E01.1080p.mkv"), b"1080p rip").unwrap();
    std::fs::write(show_dir.join("The.Show.S01E01.720p.mkv"), b"720p rip").unwrap();

    let service = make_multi_source_scan_service(lib_repo, file_repo.clone(), show_repo.clone());
    let indexed = service.scan_library(library.id).await.unwrap();

    assert_eq!(indexed, 2, "neither file is rejected by the scan");
    let files = file_repo.find_all_by_library(library.id).await.unwrap();
    assert_eq!(files.len(), 2);
    let episode_id = episode_id_of(&files[0]);
    assert_eq!(
        episode_id_of(&files[1]),
        episode_id,
        "both files attach to the one S01E01"
    );
    assert_eq!(
        file_repo
            .find_by_episode_id(episode_id)
            .await
            .unwrap()
            .len(),
        2,
        "the episode has two sources"
    );
    assert_eq!(show_repo.episodes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_new_file_for_an_existing_episode_attaches_without_rewriting_it() {
    let lib_repo = Arc::new(InMemoryLibraryRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let show_repo = Arc::new(InMemoryShowRepository::default());
    let dir = TempDir::new().unwrap();
    let library = make_library_in_tempdir(&lib_repo, &dir).await;

    // An episode that already exists with no file behind it -- its old
    // file was replaced -- carrying a title enrichment established.
    let show = show_repo
        .find_or_create_by_identity(beam_domain::models::CreateShow::new(
            "Severance".to_string(),
            None,
        ))
        .await
        .unwrap();
    let season = show_repo.find_or_create_season(show.id, 1).await.unwrap();
    let existing = show_repo
        .find_or_create_episode(beam_domain::models::CreateEpisode {
            season_id: season.id,
            episode_number: 1,
            title: "Good News About Hell".to_string(),
            runtime: Some(Duration::from_secs(57 * 60)),
            air_date: None,
        })
        .await
        .unwrap();

    let show_dir = dir.path().join("Severance");
    std::fs::create_dir_all(&show_dir).unwrap();
    std::fs::write(
        show_dir.join("Severance.S01E01.REPACK.2160p.mkv"),
        b"new rip",
    )
    .unwrap();

    let service = make_multi_source_scan_service(lib_repo, file_repo.clone(), show_repo.clone());
    let indexed = service.scan_library(library.id).await.unwrap();

    assert_eq!(indexed, 1);
    let files = file_repo.find_all_by_library(library.id).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(
        episode_id_of(&files[0]),
        existing.id,
        "the differently named file attaches to the existing episode"
    );
    let stored = show_repo
        .find_episode_by_id(existing.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.title, "Good News About Hell",
        "the new file's filename does not overwrite the established title"
    );
    assert_eq!(stored.runtime, Some(Duration::from_secs(57 * 60)));
    assert_eq!(show_repo.episodes.lock().unwrap().len(), 1);
}

// ─── classify_media_content: movie tests ──────────────────────────────────

#[tokio::test]
async fn test_classify_movie_simple_title() {
    let (service, movie_repo, _) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let path = PathBuf::from("/media/movies/Avatar.mp4");

    let content = service
        .classify_for_test(&path, lib_id, Duration::from_secs(9600))
        .await
        .unwrap();

    let entry_id = match content {
        MediaFileContent::Movie { movie_entry_id, .. } => movie_entry_id,
        _ => panic!("expected Movie, got Episode"),
    };

    let entries: Vec<_> = movie_repo
        .entries
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, entry_id);

    let movies: Vec<_> = movie_repo
        .movies
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(movies.len(), 1);
    assert_eq!(movies[0].title, "Avatar");
}

#[tokio::test]
async fn test_classify_movie_with_year() {
    let (service, movie_repo, _) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let path = PathBuf::from("/media/The.Matrix.Reloaded.2003.mkv");

    let content = service
        .classify_for_test(&path, lib_id, Duration::from_secs(7200))
        .await
        .unwrap();

    assert!(matches!(content, MediaFileContent::Movie { .. }));

    let movies: Vec<_> = movie_repo
        .movies
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(movies.len(), 1);
    assert_eq!(movies[0].title, "The Matrix Reloaded");
    assert_eq!(movies[0].year, Some(2003));
}

#[tokio::test]
async fn test_classify_movie_with_parentheses() {
    let (service, movie_repo, _) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let path = PathBuf::from("/media/movie (2024).avi");

    let content = service
        .classify_for_test(&path, lib_id, Duration::from_secs(6000))
        .await
        .unwrap();

    assert!(matches!(content, MediaFileContent::Movie { .. }));

    let movies: Vec<_> = movie_repo
        .movies
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(movies.len(), 1);
    assert_eq!(movies[0].title, "movie");
    assert_eq!(movies[0].year, Some(2024));
}

#[tokio::test]
async fn test_classify_movie_existing_reused() {
    let (service, movie_repo, _) = make_classify_service();
    let lib_id = Uuid::new_v4();
    let duration = Duration::from_secs(7200);

    // First call — creates the movie
    service
        .classify_for_test(&PathBuf::from("/media/Avatar.mp4"), lib_id, duration)
        .await
        .unwrap();

    // Second call with the same title — must reuse the existing movie record
    service
        .classify_for_test(&PathBuf::from("/backup/Avatar.mp4"), lib_id, duration)
        .await
        .unwrap();

    let movies: Vec<_> = movie_repo
        .movies
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(movies.len(), 1, "movie must not be duplicated");

    // Both copies are files of the one entry for the film's default
    // edition in this library (issue #182), not an entry each.
    let entries: Vec<_> = movie_repo
        .entries
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(entries.len(), 1);
}

// ─── classify_media_content: edge cases ───────────────────────────────────

#[tokio::test]
async fn test_classify_empty_file_stem_falls_to_movie() {
    let (service, movie_repo, _) = make_classify_service();
    let lib_id = Uuid::new_v4();
    // Root path has no file-stem component — file_stem() returns None → empty string
    let path = PathBuf::from("/");

    let content = service
        .classify_for_test(&path, lib_id, Duration::from_secs(100))
        .await
        .unwrap();

    assert!(
        matches!(content, MediaFileContent::Movie { .. }),
        "path with no file stem should fall back to Movie"
    );

    let movies: Vec<_> = movie_repo
        .movies
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(movies.len(), 1);
    assert_eq!(movies[0].title, "");
}

#[tokio::test]
async fn test_classify_episode_no_parent_dir_uses_unknown_show() {
    let (service, _, show_repo) = make_classify_service();
    let lib_id = Uuid::new_v4();
    // Bare filename with no directory component; parent() → Some("") → file_name() → None
    let path = PathBuf::from("S01E01.mkv");

    let content = service
        .classify_for_test(&path, lib_id, Duration::from_secs(3600))
        .await
        .unwrap();

    assert!(matches!(content, MediaFileContent::Episode { .. }));

    let shows: Vec<_> = show_repo.shows.lock().unwrap().values().cloned().collect();
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].title, "Unknown Show");
}
