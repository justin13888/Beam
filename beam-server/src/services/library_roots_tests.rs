//! Library root conflicts and the startup audit of stored roots (issue #186):
//! a new root may not overlap a library or the data directory, and a stored
//! root that does is caught at startup.

use super::library_tests::tests::stored_library;
use crate::services::library::{
    InMemoryPathValidator, LibraryError, LibraryService, LibraryServiceDependencies,
    LocalLibraryService,
};
use beam_domain::repositories::LibraryRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_index::services::index::MockIndexService;
use beam_index::services::notification::InMemoryNotificationService;
use std::path::PathBuf;
use std::sync::Arc;

// ── root conflicts (issue #186) ──────────────────────────────────────────

#[test]
fn find_root_conflict_compares_whole_components_in_both_directions() {
    use crate::services::library::{RootConflict, find_root_conflict};

    let existing = vec![PathBuf::from("/m/movies"), PathBuf::from("/m/shows")];
    let data_dir = PathBuf::from("/srv/beam/data");
    let library = |p: &str| Some(RootConflict::Library(PathBuf::from(p)));
    let cases: [(&str, Option<RootConflict>); 9] = [
        ("/m/movies", library("/m/movies")),
        ("/m/movies/4k", library("/m/movies")),
        ("/m", library("/m/movies")),
        ("/m/movies2", None),
        ("/m/show", None),
        ("/m/anime", None),
        ("/srv/beam/data", Some(RootConflict::DataDir)),
        ("/srv/beam/data/artwork", Some(RootConflict::DataDir)),
        ("/srv", Some(RootConflict::DataDir)),
    ];
    for (candidate, expected) in cases {
        assert_eq!(
            find_root_conflict(std::path::Path::new(candidate), &existing, &data_dir),
            expected,
            "{candidate}"
        );
    }
}

fn conflict_service(
    lib_repo: Arc<InMemoryLibraryRepository>,
    resolves_to: &str,
) -> LocalLibraryService {
    LocalLibraryService::new(LibraryServiceDependencies {
        library_repo: lib_repo,
        file_repo: Arc::new(InMemoryFileRepository::default()),
        video_dir: PathBuf::from("/media"),
        data_dir: PathBuf::from("/media/beam-data"),
        notification_service: Arc::new(InMemoryNotificationService::new()),
        index_service: Arc::new(MockIndexService::new()),
        path_validator: Arc::new(InMemoryPathValidator::success(PathBuf::from(resolves_to))),
        watch_hook: Arc::new(beam_index::runtime::LibraryWatches::new(None)),
    })
}

#[tokio::test]
async fn a_root_nested_in_an_existing_library_is_rejected_and_nothing_is_stored() {
    let lib_repo = Arc::new(InMemoryLibraryRepository::default());
    conflict_service(lib_repo.clone(), "/media/movies")
        .create_library("Movies".to_string(), "movies".to_string())
        .await
        .expect("the first library is disjoint");

    let err = conflict_service(lib_repo.clone(), "/media/movies/4k")
        .create_library("4K".to_string(), "movies/4k".to_string())
        .await
        .expect_err("nested inside Movies");

    assert!(
        matches!(err, LibraryError::PathOverlapsLibrary),
        "got {err:?}"
    );
    assert!(!err.to_string().contains('/'), "no path in {err}");
    assert_eq!(lib_repo.find_all().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_root_holding_the_data_directory_is_rejected() {
    let lib_repo = Arc::new(InMemoryLibraryRepository::default());

    let err = conflict_service(lib_repo.clone(), "/media")
        .create_library("Everything".to_string(), ".".to_string())
        .await
        .expect_err("contains the data directory");

    assert!(
        matches!(err, LibraryError::PathOverlapsDataDir),
        "got {err:?}"
    );
    assert!(!err.to_string().contains('/'), "no path in {err}");
    assert!(lib_repo.find_all().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_sibling_sharing_a_name_prefix_is_accepted() {
    let lib_repo = Arc::new(InMemoryLibraryRepository::default());
    conflict_service(lib_repo.clone(), "/media/movies")
        .create_library("Movies".to_string(), "movies".to_string())
        .await
        .unwrap();

    conflict_service(lib_repo.clone(), "/media/movies2")
        .create_library("Movies 2".to_string(), "movies2".to_string())
        .await
        .expect("/media/movies2 is not inside /media/movies");

    assert_eq!(lib_repo.find_all().await.unwrap().len(), 2);
}

// ── startup audit of stored roots (issue #186) ───────────────────────────

#[tokio::test]
async fn a_data_directory_moved_inside_an_existing_library_stops_startup() {
    use crate::services::library::{StartupRootError, audit_existing_roots};

    let lib_repo = InMemoryLibraryRepository::default();
    stored_library(&lib_repo, "Shows", "/media/shows").await;
    stored_library(&lib_repo, "Movies", "/media/movies").await;

    let err = audit_existing_roots(&lib_repo, std::path::Path::new("/media/movies/.beam"))
        .await
        .expect_err("the artwork cache would be written inside Movies");

    match &err {
        StartupRootError::DataDirOverlapsLibrary { library, root, .. } => {
            assert_eq!(library, "Movies");
            assert_eq!(root, &PathBuf::from("/media/movies"));
        }
        other => panic!("expected DataDirOverlapsLibrary, got {other:?}"),
    }
    assert!(
        err.to_string().contains("BEAM_DATA_DIR"),
        "the error names the setting to change: {err}"
    );
}

#[tokio::test]
async fn a_data_directory_holding_a_library_stops_startup() {
    use crate::services::library::{StartupRootError, audit_existing_roots};

    let lib_repo = InMemoryLibraryRepository::default();
    stored_library(&lib_repo, "Movies", "/srv/beam/movies").await;

    let err = audit_existing_roots(&lib_repo, std::path::Path::new("/srv/beam"))
        .await
        .expect_err("the library lies inside the data directory");
    assert!(
        matches!(err, StartupRootError::DataDirOverlapsLibrary { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn stored_libraries_that_overlap_each_other_are_reported_not_refused() {
    use crate::services::library::{ExistingLibraryOverlap, audit_existing_roots};

    let lib_repo = InMemoryLibraryRepository::default();
    let movies = stored_library(&lib_repo, "Movies", "/media/movies").await;
    let four_k = stored_library(&lib_repo, "4K", "/media/movies/4k").await;
    stored_library(&lib_repo, "Movies 2", "/media/movies2").await;

    let overlaps = audit_existing_roots(&lib_repo, std::path::Path::new("/srv/beam"))
        .await
        .expect("an overlap between libraries does not stop startup");

    assert_eq!(
        overlaps.len(),
        1,
        "only Movies and 4K overlap: {overlaps:?}"
    );
    let ExistingLibraryOverlap { first, second } = overlaps[0].clone();
    let mut pair = [first, second];
    pair.sort();
    let mut expected = [movies, four_k];
    expected.sort();
    assert_eq!(pair, expected);
}
