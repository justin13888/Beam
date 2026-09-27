//! Soft-deleting files that went missing (issue #179).
//!
//! The scan and the watcher never hard-delete a row on first sight any more:
//! a file the walk does not see is stamped `missing_since`, hidden from every
//! visible read, restored under the same id if its path comes back, and
//! purged only once it has stayed missing for the grace period. Time is the
//! injected `TestClock`, the filesystem a real `TempDir`.

use super::*;
use crate::services::admin_log::LocalAdminLogService;
use crate::services::hash::MockHashService;
use crate::services::media_info::MockMediaInfoService;
use crate::services::notification::{EventLevel, InMemoryNotificationService};
use beam_domain::models::{CreateLibrary, Library};
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::services::TestClock;
use tempfile::TempDir;

// ─── plan_missing ────────────────────────────────────────────────────────────

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn instant(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid instant")
}

fn row_at(path: &str, missing_since: Option<DateTime<Utc>>) -> MediaFile {
    MediaFile {
        id: Uuid::new_v4(),
        library_id: Uuid::nil(),
        path: PathBuf::from(path),
        hash: 0,
        size_bytes: 0,
        mtime: None,
        mime_type: None,
        duration: None,
        container_format: None,
        content: None,
        status: FileStatus::Known,
        scanned_at: instant(0),
        updated_at: instant(0),
        missing_since,
    }
}

#[test]
fn plan_missing_decides_each_unseen_row_by_its_stamp_and_the_grace() {
    let grace = 7 * DAY;
    let grace_secs = grace.as_secs() as i64;
    let now = instant(grace_secs * 10);

    struct Case {
        name: &'static str,
        missing_since: Option<DateTime<Utc>>,
        grace: Duration,
        marked: bool,
        purged: bool,
    }
    let case = |name, missing_since, grace, marked, purged| Case {
        name,
        missing_since,
        grace,
        marked,
        purged,
    };
    let cases = [
        case("first noticed now", None, grace, true, false),
        case(
            "missing for less than the grace",
            Some(now - chrono::TimeDelta::seconds(grace_secs - 1)),
            grace,
            false,
            false,
        ),
        case(
            "missing for exactly the grace",
            Some(now - chrono::TimeDelta::seconds(grace_secs)),
            grace,
            false,
            true,
        ),
        case(
            "missing for longer than the grace",
            Some(now - chrono::TimeDelta::seconds(grace_secs * 3)),
            grace,
            false,
            true,
        ),
        case(
            "zero grace, first noticed now: stamped and purged at once",
            None,
            Duration::ZERO,
            true,
            true,
        ),
        case(
            "a grace too long for chrono never purges",
            Some(instant(0)),
            Duration::MAX,
            false,
            false,
        ),
    ];

    for Case {
        name,
        missing_since,
        grace,
        marked,
        purged,
    } in cases
    {
        let row = row_at("/lib/a.mkv", missing_since);
        let plan = plan_missing([&row], &[], false, now, grace);
        assert_eq!(plan.mark.contains(&row.id), marked, "{name}: mark");
        assert_eq!(plan.purge.contains(&row.id), purged, "{name}: purge");
        assert_eq!(plan.shielded, 0, "{name}: nothing is shielded");
    }
}

#[test]
fn plan_missing_shields_rows_under_a_failed_path_by_whole_components() {
    let under = row_at("/lib/a/b/c.mkv", None);
    let itself = row_at("/lib/a/b", Some(instant(0)));
    let sibling_prefix = row_at("/lib/a/bc.mkv", None);
    let elsewhere = row_at("/lib/z.mkv", None);
    let failed = vec![PathBuf::from("/lib/a/b")];

    let plan = plan_missing(
        [&under, &itself, &sibling_prefix, &elsewhere],
        &failed,
        false,
        instant(DAY.as_secs() as i64 * 365),
        DAY,
    );

    assert_eq!(plan.shielded, 2, "the failed path and what is beneath it");
    let mut marked = plan.mark.clone();
    marked.sort();
    let mut expected = vec![sibling_prefix.id, elsewhere.id];
    expected.sort();
    assert_eq!(
        marked, expected,
        "`/lib/a/bc.mkv` shares a string prefix with `/lib/a/b`, not a directory"
    );
    assert!(
        !plan.purge.contains(&itself.id),
        "a shielded row is never purged, however long it has been missing"
    );
}

#[test]
fn plan_missing_shields_every_row_after_an_unscoped_walk_failure() {
    let fresh = row_at("/lib/a.mkv", None);
    let overdue = row_at("/lib/b.mkv", Some(instant(0)));

    let plan = plan_missing(
        [&fresh, &overdue],
        &[],
        true,
        instant(DAY.as_secs() as i64 * 365),
        DAY,
    );

    assert_eq!(
        plan,
        MissingPlan {
            mark: Vec::new(),
            purge: Vec::new(),
            shielded: 2,
        }
    );
}

// ─── the scan and the watcher ────────────────────────────────────────────────

/// The grace the harness configures: short enough to step across with the
/// clock, and not the service default, so a test cannot pass by accident.
const GRACE: Duration = Duration::from_secs(3 * 24 * 60 * 60);

struct Harness {
    /// Holds the library root (`<dir>/library`) and, beside it, a parking
    /// place a file can be moved to and back without changing its mtime.
    dir: TempDir,
    root: PathBuf,
    library: Library,
    file_repo: Arc<InMemoryFileRepository>,
    notifications: Arc<InMemoryNotificationService>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    clock: Arc<TestClock>,
    service: LocalIndexService,
}

impl Harness {
    async fn new() -> Self {
        Self::with_grace(GRACE).await
    }

    async fn with_grace(grace: Duration) -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(dir.path().join("parked")).unwrap();

        let lib_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let notifications = Arc::new(InMemoryNotificationService::new());
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let clock = Arc::new(TestClock::starting_at(instant(0)));
        let library = lib_repo
            .create(CreateLibrary {
                name: "Soft Delete".to_string(),
                root_path: root.clone(),
                description: None,
            })
            .await
            .unwrap();
        // No hasher or prober expectations: every file here is either already
        // indexed and unchanged, or gone. Reaching either would be a rehash.
        let service = LocalIndexService::new(
            lib_repo,
            file_repo.clone(),
            Arc::new(InMemoryMovieRepository::default()),
            Arc::new(InMemoryShowRepository::default()),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(MockHashService::new()),
            Arc::new(MockMediaInfoService::new()),
            notifications.clone(),
            Arc::new(LocalAdminLogService::new(
                admin_log_repo.clone() as Arc<dyn AdminLogRepository>
            )),
        )
        .with_clock(clock.clone())
        .with_missing_file_grace(grace);

        Self {
            dir,
            root,
            library,
            file_repo,
            notifications,
            admin_log_repo,
            clock,
            service,
        }
    }

    /// Put `rel` on disk and index it as an earlier scan would have: a row
    /// whose size and mtime match, so reconciling it touches no hasher.
    fn index_on_disk(&self, rel: &str) -> MediaFile {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rel.as_bytes()).unwrap();
        let (size_bytes, mtime) = read_fs_meta(&path).unwrap();
        let row = MediaFile {
            id: Uuid::new_v4(),
            library_id: self.library.id,
            path,
            hash: 7,
            size_bytes,
            mtime,
            mime_type: Some("video/mp4".to_string()),
            duration: None,
            container_format: None,
            content: None,
            status: FileStatus::Known,
            scanned_at: instant(0),
            updated_at: instant(0),
            missing_since: None,
        };
        self.file_repo
            .files
            .lock()
            .unwrap()
            .insert(row.id, row.clone());
        row
    }

    /// Move an indexed file out of the library, keeping its mtime.
    fn park(&self, row: &MediaFile) {
        std::fs::rename(&row.path, self.parked(row)).unwrap();
    }

    /// Move a parked file back to where it was indexed.
    fn unpark(&self, row: &MediaFile) {
        std::fs::rename(self.parked(row), &row.path).unwrap();
    }

    fn parked(&self, row: &MediaFile) -> PathBuf {
        self.dir.path().join("parked").join(row.id.to_string())
    }

    async fn scan(&self) -> Result<u32, IndexError> {
        self.service.scan_library(self.library.id.to_string()).await
    }

    /// The row as the reconcile reads see it, missing or not.
    async fn stored(&self, row: &MediaFile) -> Option<MediaFile> {
        self.file_repo
            .find_by_path(&row.path.to_string_lossy())
            .await
            .unwrap()
    }

    async fn visible_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .file_repo
            .find_all_by_library(self.library.id)
            .await
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        ids.sort();
        ids
    }

    async fn completion_details(&self) -> serde_json::Value {
        self.admin_log_repo
            .list(100, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|l| l.message.contains("scan completed"))
            .and_then(|l| l.details)
            .expect("a completed scan logs its counts")
    }
}

#[tokio::test]
async fn a_file_gone_from_disk_is_marked_missing_and_hidden_not_deleted() {
    let h = Harness::new().await;
    let kept = h.index_on_disk("kept.mp4");
    let gone = h.index_on_disk("gone.mp4");
    h.park(&gone);
    h.clock.advance(Duration::from_secs(90));

    assert_eq!(h.scan().await.unwrap(), 0);

    let stored = h.stored(&gone).await.expect("the row is kept");
    assert_eq!(stored.id, gone.id);
    assert_eq!(
        stored.missing_since,
        Some(instant(90)),
        "stamped from the injected clock"
    );
    assert_eq!(h.visible_ids().await, vec![kept.id]);
    let details = h.completion_details().await;
    assert_eq!(details["marked_missing"], serde_json::json!(1));
    assert_eq!(details["purged"], serde_json::json!(0));
}

#[tokio::test]
async fn a_missing_file_within_the_grace_keeps_its_first_stamp() {
    let h = Harness::new().await;
    h.index_on_disk("kept.mp4");
    let gone = h.index_on_disk("gone.mp4");
    h.park(&gone);
    h.scan().await.unwrap();

    h.clock.advance(GRACE - Duration::from_secs(1));
    h.scan().await.unwrap();

    let stored = h
        .stored(&gone)
        .await
        .expect("one second short of the grace, the row is kept");
    assert_eq!(stored.missing_since, Some(instant(0)));
}

#[tokio::test]
async fn a_file_missing_for_the_whole_grace_is_purged_by_the_next_scan() {
    let h = Harness::new().await;
    let kept = h.index_on_disk("kept.mp4");
    let gone = h.index_on_disk("gone.mp4");
    h.park(&gone);
    h.scan().await.unwrap();

    h.clock.advance(GRACE);
    h.scan().await.unwrap();

    assert!(h.stored(&gone).await.is_none(), "the row is purged");
    assert!(h.stored(&kept).await.is_some());
    let logs = h.admin_log_repo.list(100, 0).await.unwrap();
    let purge = logs
        .iter()
        .find(|l| l.message.starts_with("Purged"))
        .expect("a purge is reported in the admin log");
    assert_eq!(
        purge.details.as_ref().unwrap()["purged"],
        serde_json::json!(1)
    );
}

#[tokio::test]
async fn a_zero_grace_purges_at_the_first_healthy_scan() {
    let h = Harness::with_grace(Duration::ZERO).await;
    h.index_on_disk("kept.mp4");
    let gone = h.index_on_disk("gone.mp4");
    h.park(&gone);

    h.scan().await.unwrap();

    assert!(
        h.stored(&gone).await.is_none(),
        "stamped and purged in one scan: purge only removes stamped rows"
    );
}

#[tokio::test]
async fn a_file_that_comes_back_is_restored_under_the_same_id_without_a_rehash() {
    let h = Harness::new().await;
    h.index_on_disk("kept.mp4");
    let away = h.index_on_disk("away.mp4");
    h.park(&away);
    h.scan().await.unwrap();
    assert!(h.stored(&away).await.unwrap().missing_since.is_some());

    h.unpark(&away);
    h.clock.advance(DAY);
    // `MockHashService` has no expectations: a rehash would panic.
    h.scan().await.unwrap();

    let restored = h
        .file_repo
        .find_by_id(away.id)
        .await
        .unwrap()
        .expect("visible again under its old id");
    assert_eq!(restored.missing_since, None);
    let details = h.completion_details().await;
    assert_eq!(details["restored"], serde_json::json!(1));
    assert_eq!(details["added"], serde_json::json!(0), "not indexed as new");
}

#[tokio::test]
async fn an_empty_root_is_still_refused_when_the_indexed_files_are_already_missing() {
    let h = Harness::new().await;
    let a = h.index_on_disk("a.mp4");
    let b = h.index_on_disk("b.mkv");
    h.park(&a);
    h.park(&b);
    // Mark both missing through the watcher, as a volume emptied file by file
    // would.
    for row in [&a, &b] {
        h.service
            .reconcile_path(h.library.id, row.path.clone(), FsEventKind::Removed)
            .await
            .unwrap();
    }

    h.clock.advance(GRACE * 2);
    let err = h
        .scan()
        .await
        .expect_err("missing rows still count: the empty-root guard refuses");
    assert!(matches!(err, IndexError::PathNotFound(_)));

    for row in [&a, &b] {
        assert!(
            h.stored(row).await.is_some(),
            "a refused scan purges nothing, however overdue"
        );
    }
}

#[tokio::test]
async fn a_watcher_removal_marks_the_file_missing_and_a_creation_restores_it() {
    let h = Harness::new().await;
    let row = h.index_on_disk("show.mkv");
    h.park(&row);
    h.clock.advance(Duration::from_secs(30));

    h.service
        .reconcile_path(h.library.id, row.path.clone(), FsEventKind::Removed)
        .await
        .unwrap();

    let stored = h.stored(&row).await.expect("a removal never hard-deletes");
    assert_eq!(stored.missing_since, Some(instant(30)));
    assert!(h.file_repo.find_by_id(row.id).await.unwrap().is_none());

    h.unpark(&row);
    h.service
        .reconcile_path(h.library.id, row.path.clone(), FsEventKind::Created)
        .await
        .unwrap();

    let back = h
        .file_repo
        .find_by_id(row.id)
        .await
        .unwrap()
        .expect("the same row is visible again");
    assert_eq!(back.missing_since, None);
    assert_eq!(h.visible_ids().await, vec![row.id]);
}

#[tokio::test]
async fn a_watcher_removal_while_the_root_is_gone_changes_nothing() {
    let h = Harness::new().await;
    let row = h.index_on_disk("film.mp4");
    // The whole volume goes away, not one file.
    std::fs::remove_dir_all(&h.root).unwrap();

    h.service
        .reconcile_path(h.library.id, row.path.clone(), FsEventKind::Removed)
        .await
        .unwrap();

    let stored = h.stored(&row).await.unwrap();
    assert_eq!(
        stored.missing_since, None,
        "an absent root says nothing about the file"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn rows_under_an_unreadable_directory_are_left_alone_and_the_failure_reported() {
    use std::os::unix::fs::PermissionsExt;

    let h = Harness::new().await;
    let kept = h.index_on_disk("kept.mp4");
    let locked_row = h.index_on_disk("locked/inside.mp4");
    let gone = h.index_on_disk("gone.mp4");
    h.park(&gone);
    let locked = h.root.join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // Running as root: permissions do not bind, so the failure this test
        // is about cannot be produced.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let result = h.scan().await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    result.unwrap();

    assert_eq!(
        h.stored(&locked_row).await.unwrap().missing_since,
        None,
        "a row the walk could not see is not a row the walk found gone"
    );
    assert!(h.stored(&gone).await.unwrap().missing_since.is_some());
    assert!(h.stored(&kept).await.unwrap().missing_since.is_none());

    let warnings: Vec<_> = h
        .notifications
        .published_events()
        .into_iter()
        .filter(|e| matches!(e.level, EventLevel::Warning))
        .collect();
    assert_eq!(warnings.len(), 1, "one warning for the walk: {warnings:?}");
    let logs = h.admin_log_repo.list(100, 0).await.unwrap();
    let entry = logs
        .iter()
        .find(|l| l.level == AdminLogLevel::Warning && l.category == AdminLogCategory::LibraryScan)
        .expect("the unreadable path is written to the admin log");
    let details = entry.details.as_ref().unwrap();
    assert_eq!(
        details["failed_paths"],
        serde_json::json!([locked.display().to_string()])
    );
    assert_eq!(details["shielded"], serde_json::json!(1));
}

/// Make `dir` listable but not searchable (`0o444`): the walk still reads its
/// entries, but every stat beneath it fails with `EACCES` -- the same shape as
/// a transient `EIO` or `ESTALE` on a network mount. Returns `false` when the
/// process runs as root, where permissions do not bind and the failure these
/// tests are about cannot be produced; the caller then skips.
#[cfg(unix)]
fn make_unsearchable(dir: &Path, probe: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o444)).unwrap();
    if std::fs::metadata(probe).is_ok() {
        restore_searchable(dir);
        return false;
    }
    true
}

#[cfg(unix)]
fn restore_searchable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_listed_file_the_scan_cannot_stat_is_shielded_not_marked_missing() {
    let h = Harness::new().await;
    h.index_on_disk("kept.mp4");
    let unstattable = h.index_on_disk("locked/a.mkv");
    let locked = h.root.join("locked");
    if !make_unsearchable(&locked, &unstattable.path) {
        return;
    }

    let result = h.scan().await;
    restore_searchable(&locked);
    result.unwrap();

    assert_eq!(
        h.stored(&unstattable).await.unwrap().missing_since,
        None,
        "a file the walk listed but could not stat was not found gone"
    );
    let logs = h.admin_log_repo.list(100, 0).await.unwrap();
    let entry = logs
        .iter()
        .find(|l| l.level == AdminLogLevel::Warning && l.category == AdminLogCategory::LibraryScan)
        .expect("the unstattable path is reported as a walk failure");
    let details = entry.details.as_ref().unwrap();
    assert_eq!(
        details["failed_paths"],
        serde_json::json!([unstattable.path.display().to_string()])
    );
    assert_eq!(details["shielded"], serde_json::json!(1));
}

#[cfg(unix)]
#[tokio::test]
async fn a_watcher_event_for_a_path_that_cannot_be_statted_changes_nothing() {
    let h = Harness::new().await;
    let row = h.index_on_disk("locked/a.mkv");
    let locked = h.root.join("locked");
    if !make_unsearchable(&locked, &row.path) {
        return;
    }

    let result = h
        .service
        .reconcile_path(h.library.id, row.path.clone(), FsEventKind::Modified)
        .await;
    restore_searchable(&locked);
    result.unwrap();

    assert_eq!(
        h.stored(&row).await.unwrap().missing_since,
        None,
        "a failed stat says nothing about whether the file is there"
    );
}
