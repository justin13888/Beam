//! Duplicate and runtime-divergence warnings (issue #88).

use super::*;
use crate::services::admin_log::LocalAdminLogService;
use crate::services::admin_log::NoOpAdminLogService;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::EventLevel;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::MediaFile;
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::MockFileRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::MockMovieRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use std::path::PathBuf;

// ─── runtime-divergence detection (issue #88) ──────────────────────────────

/// Build a service wired for divergence tests: real in-memory file + movie
/// repos (so sibling lookups resolve), the caller's notification + admin-log
/// services for inspection, and mocks for the parts a bare divergence check
/// never touches.
fn make_divergence_service(
    file_repo: Arc<dyn FileRepository>,
    movie_repo: Arc<dyn MovieRepository>,
    notification: Arc<InMemoryNotificationService>,
    admin_log: Arc<dyn AdminLogService>,
) -> LocalIndexService {
    LocalIndexService::new(
        Arc::new(InMemoryLibraryRepository::default()),
        file_repo,
        movie_repo,
        Arc::new(InMemoryShowRepository::default()),
        Arc::new(InMemoryMediaStreamRepository::default()),
        Arc::new(MockHashService::new()),
        Arc::new(MockMediaInfoService::new()),
        notification,
        admin_log,
        Arc::new(beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository::default()),
    )
}

fn make_file_with_content(
    content: Option<MediaFileContent>,
    duration_secs: Option<f64>,
    path: &str,
) -> MediaFile {
    MediaFile {
        id: Uuid::new_v4(),
        library_id: Uuid::new_v4(),
        path: PathBuf::from(path),
        hash: 0,
        size_bytes: 1024,
        mtime: None,
        identity: None,
        mime_type: None,
        duration: duration_secs.map(Duration::from_secs_f64),
        container_format: None,
        content,
        status: FileStatus::Known,
        classifier_version: 0,
        container_tags: None,
        scanned_at: Utc::now(),
        updated_at: Utc::now(),
        missing_since: None,
    }
}

/// Seed a movie with two entries (one file each) under the same movie id and
/// return the service, the notification fake, the admin-log repo, and the
/// first file (the one that gets checked).
async fn seed_two_movie_renditions(
    first_secs: Option<f64>,
    second_secs: Option<f64>,
) -> (
    LocalIndexService,
    Arc<InMemoryNotificationService>,
    Arc<InMemoryAdminLogRepository>,
    MediaFile,
) {
    use beam_domain::models::{CreateMovie, CreateMovieEntry};

    let movie_repo = Arc::new(InMemoryMovieRepository::default());
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let notification = Arc::new(InMemoryNotificationService::new());
    let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
    let admin_log_svc = Arc::new(LocalAdminLogService::new(
        admin_log_repo.clone() as Arc<dyn AdminLogRepository>
    ));

    let library_id = Uuid::new_v4();
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("Some Movie".to_string(), None, None))
        .await
        .unwrap();
    let entry_a = movie_repo
        .find_or_create_entry(CreateMovieEntry {
            library_id,
            movie_id: movie.id,
            edition: None,
        })
        .await
        .unwrap();
    let entry_b = movie_repo
        .find_or_create_entry(CreateMovieEntry {
            library_id,
            movie_id: movie.id,
            edition: Some("Extended".to_string()),
        })
        .await
        .unwrap();

    let file_a = make_file_with_content(
        Some(MediaFileContent::movie(entry_a.id)),
        first_secs,
        "/media/movie-a.mkv",
    );
    let file_b = make_file_with_content(
        Some(MediaFileContent::movie(entry_b.id)),
        second_secs,
        "/media/movie-b.mkv",
    );
    file_repo
        .files
        .lock()
        .unwrap()
        .insert(file_a.id, file_a.clone());
    file_repo.files.lock().unwrap().insert(file_b.id, file_b);

    let service =
        make_divergence_service(file_repo, movie_repo, notification.clone(), admin_log_svc);
    (service, notification, admin_log_repo, file_a)
}

#[tokio::test]
async fn test_divergence_movie_wildly_different_runtimes_warns() {
    // 40 min vs 90 min: ratio ~0.56 and delta 3000s both blow past the
    // thresholds → exactly one warning across both admin channels.
    let (service, notification, admin_log_repo, file_a) =
        seed_two_movie_renditions(Some(40.0 * 60.0), Some(90.0 * 60.0)).await;

    service.check_and_report_runtime_divergence(&file_a).await;

    let warnings: Vec<_> = notification
        .published_events()
        .into_iter()
        .filter(|e| matches!(e.level, EventLevel::Warning))
        .collect();
    assert_eq!(warnings.len(), 1, "expected exactly one warning event");
    let warning = &warnings[0];
    assert!(matches!(warning.category, EventCategory::LibraryScan));
    assert!(warning.message.contains("/media/movie-a.mkv"));
    assert!(warning.message.contains("/media/movie-b.mkv"));
    assert!(warning.message.contains("40 min"));
    assert!(warning.message.contains("90 min"));

    // The durable admin log must also carry a Warning LibraryScan entry.
    let logs = admin_log_repo.list(100, 0).await.unwrap();
    assert!(logs.iter().any(|l| {
        l.level == AdminLogLevel::Warning
            && l.category == AdminLogCategory::LibraryScan
            && l.message.contains("Runtime mismatch")
    }));
}

/// One part of a multi-part movie (issue #233) lasts that part: it is
/// never compared with a whole file, nor a whole file with it.
#[tokio::test]
async fn test_divergence_a_part_is_not_compared_with_a_whole_file() {
    for part_checked in [false, true] {
        // 40 min against 90 min, which diverge as whole files (see
        // `test_divergence_movie_wildly_different_runtimes_warns`).
        let (service, notification, _admin_log_repo, file_a) =
            seed_two_movie_renditions(Some(40.0 * 60.0), Some(90.0 * 60.0)).await;
        let files = &service.file_repo;
        let other = files
            .find_by_path("/media/movie-b.mkv")
            .await
            .unwrap()
            .expect("seeded");
        let (part, whole) = if part_checked {
            (file_a, other)
        } else {
            (other, file_a)
        };
        let Some(MediaFileContent::Movie { movie_entry_id, .. }) = part.content else {
            panic!("seeded a movie file");
        };
        let part = files
            .set_classification(
                part.id,
                FileClassification {
                    content: Some(MediaFileContent::Movie {
                        movie_entry_id,
                        part_number: Some(1),
                    }),
                    status: FileStatus::Known,
                    classifier_version: CLASSIFIER_VERSION,
                },
            )
            .await
            .unwrap();

        let checked = if part_checked { &part } else { &whole };
        service.check_and_report_runtime_divergence(checked).await;

        assert!(
            notification.published_events().is_empty(),
            "part checked: {part_checked}"
        );
    }
}

#[tokio::test]
async fn test_divergence_within_threshold_no_warning() {
    // 90 min vs 92 min: delta 120s < 240s (and ratio ~0.022 < 0.15) → quiet.
    let (service, notification, _admin_log_repo, file_a) =
        seed_two_movie_renditions(Some(90.0 * 60.0), Some(92.0 * 60.0)).await;

    service.check_and_report_runtime_divergence(&file_a).await;

    assert!(
        notification.published_events().is_empty(),
        "renditions within threshold must not warn"
    );
}

/// Both thresholds must be cleared: three hours against three hours five
/// minutes differ by 300s -- past the 240s floor -- but by under 3%, far
/// under the 15% ratio, so a long film's cut is not reported.
#[tokio::test]
async fn test_divergence_delta_past_the_floor_with_the_ratio_under_no_warning() {
    let (service, notification, admin_log_repo, file_a) =
        seed_two_movie_renditions(Some(180.0 * 60.0), Some(185.0 * 60.0)).await;

    service.check_and_report_runtime_divergence(&file_a).await;

    assert!(
        notification.published_events().is_empty(),
        "a small relative difference must not warn, however many seconds"
    );
    assert!(admin_log_repo.list(100, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_divergence_short_content_guard_no_warning() {
    // Two episodes at 2 min vs 3 min: the ratio (0.33) clears the relative
    // threshold, but the 60s delta is far under the 240s floor → no warning.
    let episode_id = Uuid::new_v4();
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let notification = Arc::new(InMemoryNotificationService::new());

    let file_a = make_file_with_content(
        Some(MediaFileContent::episode(episode_id)),
        Some(2.0 * 60.0),
        "/media/ep-a.mkv",
    );
    let file_b = make_file_with_content(
        Some(MediaFileContent::episode(episode_id)),
        Some(3.0 * 60.0),
        "/media/ep-b.mkv",
    );
    file_repo
        .files
        .lock()
        .unwrap()
        .insert(file_a.id, file_a.clone());
    file_repo.files.lock().unwrap().insert(file_b.id, file_b);

    let service = make_divergence_service(
        file_repo,
        Arc::new(InMemoryMovieRepository::default()),
        notification.clone(),
        Arc::new(NoOpAdminLogService),
    );

    service.check_and_report_runtime_divergence(&file_a).await;

    assert!(
        notification.published_events().is_empty(),
        "short content below the absolute floor must not warn"
    );
}

#[tokio::test]
async fn test_divergence_episode_siblings_warn() {
    // 30 min vs 60 min episodes: both thresholds cleared → one warning.
    let episode_id = Uuid::new_v4();
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let notification = Arc::new(InMemoryNotificationService::new());

    let file_a = make_file_with_content(
        Some(MediaFileContent::episode(episode_id)),
        Some(30.0 * 60.0),
        "/media/ep-a.mkv",
    );
    let file_b = make_file_with_content(
        Some(MediaFileContent::episode(episode_id)),
        Some(60.0 * 60.0),
        "/media/ep-b.mkv",
    );
    file_repo
        .files
        .lock()
        .unwrap()
        .insert(file_a.id, file_a.clone());
    file_repo.files.lock().unwrap().insert(file_b.id, file_b);

    let service = make_divergence_service(
        file_repo,
        Arc::new(InMemoryMovieRepository::default()),
        notification.clone(),
        Arc::new(NoOpAdminLogService),
    );

    service.check_and_report_runtime_divergence(&file_a).await;

    let warnings: Vec<_> = notification
        .published_events()
        .into_iter()
        .filter(|e| matches!(e.level, EventLevel::Warning))
        .collect();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains("/media/ep-b.mkv"));
}

/// A two-episode file runs twice as long as a single episode of the same
/// first episode; that is not a mismatch, so it is never compared.
#[tokio::test]
async fn test_divergence_multi_episode_sibling_is_not_compared_with_a_single_episode() {
    let episode_id = Uuid::new_v4();
    let file_repo = Arc::new(InMemoryFileRepository::default());
    let notification = Arc::new(InMemoryNotificationService::new());

    let single = make_file_with_content(
        Some(MediaFileContent::episode(episode_id)),
        Some(30.0 * 60.0),
        "/media/S01E01.mkv",
    );
    let double = make_file_with_content(
        Some(MediaFileContent::Episode {
            episode_id,
            last_episode_number: Some(2),
        }),
        Some(60.0 * 60.0),
        "/media/S01E01E02.mkv",
    );
    let other_double = make_file_with_content(
        Some(MediaFileContent::Episode {
            episode_id,
            last_episode_number: Some(2),
        }),
        Some(20.0 * 60.0),
        "/media/S01E01E02.short.mkv",
    );
    for file in [&single, &double, &other_double] {
        file_repo
            .files
            .lock()
            .unwrap()
            .insert(file.id, (*file).clone());
    }

    let service = make_divergence_service(
        file_repo,
        Arc::new(InMemoryMovieRepository::default()),
        notification.clone(),
        Arc::new(NoOpAdminLogService),
    );

    service.check_and_report_runtime_divergence(&single).await;
    assert!(
        notification.published_events().is_empty(),
        "a single episode is not compared with a two-episode file"
    );

    // Two files of the same range still are.
    service.check_and_report_runtime_divergence(&double).await;
    let warnings = notification.published_events();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains("S01E01E02.short.mkv"));
}

#[tokio::test]
async fn test_divergence_unprobed_sibling_skipped() {
    // The checked file is probed (40 min) but its only sibling has no
    // duration → the sibling is skipped and nothing is flagged.
    let (service, notification, _admin_log_repo, file_a) =
        seed_two_movie_renditions(Some(40.0 * 60.0), None).await;

    service.check_and_report_runtime_divergence(&file_a).await;

    assert!(
        notification.published_events().is_empty(),
        "an unprobed sibling must be skipped, not flagged"
    );
}

#[tokio::test]
async fn test_divergence_check_never_fails_on_repo_error() {
    // A repository error during the sibling lookup must be swallowed: the
    // check returns without publishing anything, so it can never abort a
    // scan (it is invoked with `.await`, never `?`).
    let mut mock_file = MockFileRepository::new();
    mock_file
        .expect_find_by_episode_id()
        .times(1)
        .returning(|_| Err(sea_orm::DbErr::Custom("simulated lookup failure".into())));

    let notification = Arc::new(InMemoryNotificationService::new());
    let service = make_divergence_service(
        Arc::new(mock_file),
        Arc::new(MockMovieRepository::new()),
        notification.clone(),
        Arc::new(NoOpAdminLogService),
    );

    let file = make_file_with_content(
        Some(MediaFileContent::episode(Uuid::new_v4())),
        Some(45.0 * 60.0),
        "/media/only.mkv",
    );

    // Must complete (infallible) and publish nothing.
    service.check_and_report_runtime_divergence(&file).await;
    assert!(notification.published_events().is_empty());
}
