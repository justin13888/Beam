//! DVD and Blu-ray folder rips (issue #234): a disc structure is one source
//! of the title its enclosing folder names, playing the disc's main title.
//!
//! The filesystem is a real `TempDir` holding minimal discs -- IFOs and
//! playlists laid out as a disc's are ([`crate::disc::fixtures`]) -- and the
//! hasher is the real one, so a moved disc is found by its content.

use std::sync::atomic::AtomicUsize;

use super::*;
use crate::disc::fixtures::{TitleSet, mpls, ticks, write_blu_ray, write_dvd};
use crate::probe::metadata::MetadataError;
use crate::services::admin_log::LocalAdminLogService;
use crate::services::filesystem_probe::{FilesystemKind, FixedFilesystemProbe};
use crate::services::hash::{HashConfig, LocalHashService};
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::CreateLibrary;
use beam_domain::models::watch_state::{RecordProgress, WatchTarget};
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::WatchStateRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::stream::in_memory::InMemoryMediaStreamRepository;
use beam_domain::repositories::watch_state::in_memory::InMemoryWatchStateRepository;
use beam_domain::services::TestClock;
use tempfile::TempDir;

const MINUTE: Duration = Duration::from_secs(60);

/// What the prober reports for every file.
const PROBED_RUNTIME: Duration = Duration::from_secs(45 * 60);

/// A prober that reports every file as MPEG, counting its calls: a
/// relinked file is never probed.
#[derive(Debug, Default)]
struct CountingProber {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl MediaInfoService for CountingProber {
    async fn get_video_metadata(
        &self,
        file: LibraryFile,
    ) -> Result<VideoFileMetadata, MetadataError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VideoFileMetadata {
            file_path: file.path().to_path_buf(),
            metadata: HashMap::default(),
            best_video_stream: None,
            best_audio_stream: None,
            best_subtitle_stream: None,
            duration: PROBED_RUNTIME.as_micros() as i64,
            streams: vec![],
            format_name: "mpeg".to_string(),
            format_long_name: "MPEG-PS (MPEG-2 Program Stream)".to_string(),
            file_size: 1024,
            bit_rate: 1000,
            probe_score: 100,
        })
    }
}

struct Harness {
    _dir: TempDir,
    root: PathBuf,
    library: Library,
    file_repo: Arc<InMemoryFileRepository>,
    movie_repo: Arc<InMemoryMovieRepository>,
    admin_log_repo: Arc<InMemoryAdminLogRepository>,
    progress: Arc<InMemoryWatchStateRepository>,
    prober: Arc<CountingProber>,
    service: LocalIndexService,
}

impl Harness {
    async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();

        let library_repo = Arc::new(InMemoryLibraryRepository::default());
        let file_repo = Arc::new(InMemoryFileRepository::default());
        let movie_repo = Arc::new(InMemoryMovieRepository::with_files(file_repo.clone()));
        let admin_log_repo = Arc::new(InMemoryAdminLogRepository::default());
        let prober = Arc::new(CountingProber::default());
        // Long before any file here was written, so every file is settled.
        let clock = Arc::new(TestClock::starting_at(
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        ));
        let progress = Arc::new(InMemoryWatchStateRepository::new(clock.clone()));
        let library = library_repo
            .create(CreateLibrary {
                name: "Discs".to_string(),
                root_path: root.clone(),
                description: None,
            })
            .await
            .unwrap();
        let service = LocalIndexService::new(
            library_repo,
            file_repo.clone(),
            movie_repo.clone(),
            Arc::new(InMemoryShowRepository::with_files(file_repo.clone())),
            Arc::new(InMemoryMediaStreamRepository::default()),
            Arc::new(LocalHashService::new(HashConfig { num_threads: 1 })),
            prober.clone(),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(LocalAdminLogService::new(
                admin_log_repo.clone() as Arc<dyn AdminLogRepository>
            )),
            progress.clone(),
        )
        .with_clock(clock)
        .with_filesystem_probe(Arc::new(FixedFilesystemProbe(FilesystemKind::Local)));
        Self {
            _dir: dir,
            root,
            library,
            file_repo,
            movie_repo,
            admin_log_repo,
            progress,
            prober,
            service,
        }
    }

    async fn scan(&self) -> Result<ScanProgress, IndexError> {
        self.service
            .scan_now(self.library.id, ScanTrigger::Manual)
            .await
    }

    async fn reconcile(&self, path: &Path, kind: FsEventKind) -> ReconcileOutcome {
        self.service
            .reconcile_path(self.library.id, path.to_path_buf(), kind)
            .await
            .expect("the event reconciles")
    }

    /// Every row, present or missing, by path.
    fn rows(&self) -> Vec<MediaFile> {
        let mut rows: Vec<MediaFile> = self
            .file_repo
            .files
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        rows
    }

    /// The present rows, as `(path relative to the root, part)`.
    fn present(&self) -> Vec<(String, Option<u32>)> {
        self.rows()
            .into_iter()
            .filter(|row| row.missing_since.is_none())
            .map(|row| {
                let rel = row
                    .path
                    .strip_prefix(&self.root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let part = match row.content {
                    Some(MediaFileContent::Movie { part_number, .. }) => part_number,
                    _ => None,
                };
                (rel, part)
            })
            .collect()
    }

    /// Every movie, as `(title, year)`.
    async fn movies(&self) -> Vec<(String, Option<u32>)> {
        let mut movies: Vec<(String, Option<u32>)> = self
            .movie_repo
            .find_all()
            .await
            .unwrap()
            .into_iter()
            .map(|movie| (movie.title, movie.year))
            .collect();
        movies.sort();
        movies
    }

    /// The entry of every present row's movie content.
    fn entries(&self) -> std::collections::BTreeSet<Uuid> {
        self.rows()
            .into_iter()
            .filter(|row| row.missing_since.is_none())
            .filter_map(|row| match row.content {
                Some(MediaFileContent::Movie { movie_entry_id, .. }) => Some(movie_entry_id),
                _ => None,
            })
            .collect()
    }
}

/// A DVD with a short extra and a film split across three VOBs.
fn heat_dvd(folder: &Path) -> PathBuf {
    write_dvd(
        folder,
        &[
            TitleSet {
                set: 1,
                parts: &[6000],
                duration: Some(4 * MINUTE),
            },
            TitleSet {
                set: 2,
                parts: &[3000, 3000, 1200],
                duration: Some(170 * MINUTE),
            },
        ],
    )
}

#[tokio::test]
async fn a_dvd_folder_is_one_title_playing_its_main_title_set_in_parts() {
    let h = Harness::new().await;
    heat_dvd(&h.root.join("Heat (1995)"));

    h.scan().await.expect("the scan runs");

    assert_eq!(h.movies().await, [("Heat".to_string(), Some(1995))]);
    assert_eq!(
        h.present(),
        [
            ("Heat (1995)/VIDEO_TS/VTS_02_1.VOB".to_string(), Some(1)),
            ("Heat (1995)/VIDEO_TS/VTS_02_2.VOB".to_string(), Some(2)),
            ("Heat (1995)/VIDEO_TS/VTS_02_3.VOB".to_string(), Some(3)),
        ],
        "the main title set's files, in order; no menu, IFO or extra"
    );
    assert_eq!(h.entries().len(), 1, "one entry: one source of parts");
    // A part lasts that part, not the film.
    let movie = h.movie_repo.find_all().await.unwrap().remove(0);
    assert_eq!(movie.runtime, None);
}

#[tokio::test]
async fn a_blu_ray_folder_is_one_title_playing_its_main_playlist() {
    let h = Harness::new().await;
    write_blu_ray(
        &h.root.join("Heat.1995.COMPLETE.BLURAY-GRP"),
        &[("00001", 900), ("00002", 5000), ("00800", 4000)],
        &[
            ("00000.mpls", mpls(&[("00002", 0, ticks(300))])),
            ("00800.mpls", mpls(&[("00800", 0, ticks(10_200))])),
        ],
    );

    h.scan().await.expect("the scan runs");

    assert_eq!(h.movies().await, [("Heat".to_string(), Some(1995))]);
    assert_eq!(
        h.present(),
        [(
            "Heat.1995.COMPLETE.BLURAY-GRP/BDMV/STREAM/00800.m2ts".to_string(),
            None
        )],
        "the main playlist's one clip, played whole"
    );
    // A whole file lasts the film.
    let movie = h.movie_repo.find_all().await.unwrap().remove(0);
    assert_eq!(movie.runtime, Some(PROBED_RUNTIME));
}

/// Every DVD has a `VTS_01_1.VOB` and many Blu-rays a `00001.m2ts`: two discs
/// are two titles, each with its own files, however alike their names.
#[tokio::test]
async fn two_different_discs_never_collapse_into_one_title() {
    let h = Harness::new().await;
    for folder in ["Heat (1995)", "Ronin (1998)"] {
        write_dvd(
            &h.root.join("DVD").join(folder),
            &[TitleSet {
                set: 1,
                parts: &[2000],
                duration: Some(100 * MINUTE),
            }],
        );
        write_blu_ray(
            &h.root.join("Blu-ray").join(folder),
            &[("00001", 2000)],
            &[("00001.mpls", mpls(&[("00001", 0, ticks(6000))]))],
        );
    }

    h.scan().await.expect("the scan runs");

    assert_eq!(
        h.movies().await,
        [
            ("Heat".to_string(), Some(1995)),
            ("Ronin".to_string(), Some(1998)),
        ]
    );
    assert_eq!(h.present().len(), 4);
    // One entry per film -- the DVD and the Blu-ray of one film are two
    // sources of it, in two folders -- never one for every disc.
    assert_eq!(h.entries().len(), 2);
    let entry_of = |rel: &str| {
        let path = h.root.join(rel);
        h.rows()
            .into_iter()
            .find(|row| row.path == path)
            .and_then(|row| match row.content {
                Some(MediaFileContent::Movie { movie_entry_id, .. }) => Some(movie_entry_id),
                _ => None,
            })
            .unwrap()
    };
    assert_ne!(
        entry_of("DVD/Heat (1995)/VIDEO_TS/VTS_01_1.VOB"),
        entry_of("DVD/Ronin (1998)/VIDEO_TS/VTS_01_1.VOB")
    );
    assert_ne!(
        entry_of("Blu-ray/Heat (1995)/BDMV/STREAM/00001.m2ts"),
        entry_of("Blu-ray/Ronin (1998)/BDMV/STREAM/00001.m2ts")
    );
}

/// A library of nothing but disc folders is a mounted library: its discs'
/// stream files are video files, so a rescan is not refused as though the
/// volume had gone away.
#[tokio::test]
async fn a_library_of_only_disc_folders_passes_the_empty_root_guard() {
    let h = Harness::new().await;
    heat_dvd(&h.root.join("Heat (1995)"));
    write_blu_ray(
        &h.root.join("Ronin (1998)"),
        &[("00001", 2000)],
        &[("00001.mpls", mpls(&[("00001", 0, ticks(6000))]))],
    );
    h.scan().await.expect("the first scan runs");
    let indexed = h.present();
    assert_eq!(indexed.len(), 4);

    let rescan = h.scan().await.expect("a rescan is not refused");

    assert_eq!(rescan.unchanged, 4);
    assert_eq!(h.present(), indexed);

    // The guard still reads an emptied root as unmounted.
    std::fs::remove_dir_all(h.root.join("Heat (1995)")).unwrap();
    std::fs::remove_dir_all(h.root.join("Ronin (1998)")).unwrap();
    assert!(h.scan().await.is_err(), "an empty root is refused");
    assert_eq!(h.present(), indexed);
}

/// A disc moved as a folder is the same source: every row follows its file
/// by content, keeping its id, part and progress, and nothing is probed.
#[tokio::test]
async fn a_disc_moved_as_a_folder_relinks() {
    let h = Harness::new().await;
    heat_dvd(&h.root.join("Heat (1995)"));
    h.scan().await.expect("the scan runs");
    let before = h.rows();
    let user_id = Uuid::new_v4();
    let target = WatchTarget::Movie {
        movie_id: h.movie_repo.find_all().await.unwrap()[0].id,
    };
    h.progress
        .record_progress(RecordProgress {
            user_id,
            target,
            file_id: before[1].id,
            position_secs: 600.0,
            duration_secs: Some(2700.0),
            finishes_title: false,
        })
        .await
        .unwrap();
    let probes = h.prober.calls.load(Ordering::SeqCst);

    std::fs::create_dir_all(h.root.join("Movies")).unwrap();
    std::fs::rename(
        h.root.join("Heat (1995)"),
        h.root.join("Movies/Heat (1995)"),
    )
    .unwrap();
    let progress = h.scan().await.expect("the scan runs");

    assert_eq!(progress.relinked, 3);
    let after = h.rows();
    assert_eq!(after.len(), 3, "no row left behind");
    for (old, new) in before.iter().zip(&after) {
        assert_eq!(old.id, new.id);
        assert_eq!(old.content, new.content, "the same title and part");
        assert_eq!(
            new.path,
            h.root
                .join("Movies")
                .join(old.path.strip_prefix(&h.root).unwrap())
        );
        assert_eq!(new.missing_since, None);
    }
    assert_eq!(
        h.prober.calls.load(Ordering::SeqCst),
        probes,
        "nothing probed"
    );
    assert_eq!(
        h.progress
            .find(user_id, target)
            .await
            .unwrap()
            .and_then(|state| state.last_file_id),
        Some(before[1].id),
        "the watch state still names the part it was on"
    );
}

/// A watcher event anywhere inside a disc reconciles the disc whole: a
/// longer title set copied in becomes its main title, and the files that
/// were are marked missing. The disc's removal marks its rows missing.
#[tokio::test]
async fn a_watcher_event_inside_a_disc_reconciles_the_disc_whole() {
    let h = Harness::new().await;
    let disc = write_dvd(
        &h.root.join("Heat (1995)"),
        &[TitleSet {
            set: 1,
            parts: &[2000],
            duration: Some(20 * MINUTE),
        }],
    );
    h.scan().await.expect("the scan runs");
    assert_eq!(
        h.present(),
        [("Heat (1995)/VIDEO_TS/VTS_01_1.VOB".to_string(), None)]
    );

    // The rest of the disc arrives: a longer title set.
    write_dvd(
        &h.root.join("Heat (1995)"),
        &[TitleSet {
            set: 2,
            parts: &[1500, 700],
            duration: Some(170 * MINUTE),
        }],
    );
    let outcome = h
        .reconcile(&disc.join("VTS_02_0.IFO"), FsEventKind::Created)
        .await;

    assert_eq!(outcome, ReconcileOutcome::Done);
    assert_eq!(
        h.present(),
        [
            ("Heat (1995)/VIDEO_TS/VTS_02_1.VOB".to_string(), Some(1)),
            ("Heat (1995)/VIDEO_TS/VTS_02_2.VOB".to_string(), Some(2)),
        ]
    );
    assert_eq!(
        h.rows().len(),
        3,
        "the old main title is missing, not deleted"
    );

    // Another film keeps the root from reading as an unmounted volume, which
    // the watcher would leave to the next scan.
    let ronin = write_blu_ray(
        &h.root.join("Ronin (1998)"),
        &[("00001", 2000)],
        &[("00001.mpls", mpls(&[("00001", 0, ticks(6000))]))],
    );
    h.reconcile(&ronin.join("STREAM/00001.m2ts"), FsEventKind::Created)
        .await;
    std::fs::remove_dir_all(&disc).unwrap();
    h.reconcile(&disc.join("VTS_02_1.VOB"), FsEventKind::Removed)
        .await;
    assert_eq!(
        h.present(),
        [("Ronin (1998)/BDMV/STREAM/00001.m2ts".to_string(), None)],
        "a removed disc's rows are missing"
    );
    assert_eq!(h.rows().len(), 4);
}

/// A disc no folder names a film for is indexed without a title, and the
/// administrator is told why.
#[tokio::test]
async fn a_disc_with_no_title_folder_is_indexed_without_a_title() {
    let h = Harness::new().await;
    write_dvd(
        &h.root,
        &[TitleSet {
            set: 1,
            parts: &[2000],
            duration: Some(100 * MINUTE),
        }],
    );

    h.scan().await.expect("the scan runs");

    let rows = h.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].content, None);
    assert_eq!(rows[0].status, FileStatus::Unknown);
    assert!(h.movies().await.is_empty());
    let logs = h.admin_log_repo.list(100, 0).await.unwrap();
    assert!(
        logs.iter()
            .any(|log| log.message.starts_with("A DVD or Blu-ray folder")),
        "{logs:?}"
    );
}

/// A one-disc DVD in `folder` whose main title is `VTS_01_1.VOB`, of `size`
/// bytes.
fn one_disc(folder: &Path, size: usize) -> PathBuf {
    write_dvd(
        folder,
        &[TitleSet {
            set: 1,
            parts: &[size],
            duration: Some(90 * MINUTE),
        }],
    )
}

/// The most common multi-disc and box-set layouts: a film's discs in bare
/// `Disc N` folders are that film's, never a film called "Disc 1" that every
/// such disc shares, and a show's discs under a season folder -- however far
/// below it -- are indexed without a title rather than as a film.
#[tokio::test]
async fn discs_in_disc_folders_are_their_films_and_a_shows_are_untitled() {
    let h = Harness::new().await;
    one_disc(&h.root.join("Heat (1995)/Disc 1"), 2000);
    one_disc(&h.root.join("Heat (1995)/Disc 2"), 2100);
    one_disc(&h.root.join("Ronin (1998)/Disc 1"), 2200);
    write_blu_ray(
        &h.root.join("Collateral (2004)/DISC1"),
        &[("00001", 2300)],
        &[("00001.mpls", mpls(&[("00001", 0, ticks(6000))]))],
    );
    one_disc(&h.root.join("Show/Season 1/Disc 1"), 2400);
    one_disc(&h.root.join("Other Show/Season 1/Disc 1"), 2500);

    h.scan().await.expect("the scan runs");

    assert_eq!(
        h.movies().await,
        [
            ("Collateral".to_string(), Some(2004)),
            ("Heat".to_string(), Some(1995)),
            ("Ronin".to_string(), Some(1998)),
        ]
    );
    let untitled: Vec<PathBuf> = h
        .rows()
        .into_iter()
        .filter(|row| row.content.is_none())
        .map(|row| row.path.strip_prefix(&h.root).unwrap().to_path_buf())
        .collect();
    assert_eq!(
        untitled,
        [
            PathBuf::from("Other Show/Season 1/Disc 1/VIDEO_TS/VTS_01_1.VOB"),
            PathBuf::from("Show/Season 1/Disc 1/VIDEO_TS/VTS_01_1.VOB"),
        ]
    );
    assert_eq!(h.entries().len(), 3, "one entry per film");
}

/// A disc file's part follows its disc's main title whenever the disc is
/// read, not only when its row is first made: a missing VOB restored makes
/// the one-file title a run of three, and each file takes its place in it.
#[tokio::test]
async fn a_disc_files_part_follows_a_gap_filled_in_its_main_title() {
    let h = Harness::new().await;
    let disc = write_dvd(
        &h.root.join("Heat (1995)"),
        &[TitleSet {
            set: 1,
            parts: &[2000, 2000, 1000],
            duration: Some(170 * MINUTE),
        }],
    );
    let middle = std::fs::read(disc.join("VTS_01_2.VOB")).unwrap();
    std::fs::remove_file(disc.join("VTS_01_2.VOB")).unwrap();
    h.scan().await.expect("the scan runs");
    assert_eq!(
        h.present(),
        [("Heat (1995)/VIDEO_TS/VTS_01_1.VOB".to_string(), None)],
        "the title runs only to the gap: one file, played whole"
    );

    std::fs::write(disc.join("VTS_01_2.VOB"), middle).unwrap();
    h.scan().await.expect("the rescan runs");

    let whole = [
        ("Heat (1995)/VIDEO_TS/VTS_01_1.VOB".to_string(), Some(1)),
        ("Heat (1995)/VIDEO_TS/VTS_01_2.VOB".to_string(), Some(2)),
        ("Heat (1995)/VIDEO_TS/VTS_01_3.VOB".to_string(), Some(3)),
    ];
    assert_eq!(h.present(), whole);
    h.scan().await.expect("another rescan runs");
    assert_eq!(h.present(), whole, "and stays so");
}

/// The watcher re-derives parts too: a Blu-ray main title of one clip grown
/// to two -- its playlist rewritten as a copy finishes -- numbers the clip
/// it already had.
#[tokio::test]
async fn a_disc_files_part_follows_a_one_file_title_grown_to_two() {
    let h = Harness::new().await;
    let disc = write_blu_ray(
        &h.root.join("Heat (1995)"),
        &[("00801", 3000)],
        &[("00800.mpls", mpls(&[("00801", 0, ticks(5000))]))],
    );
    h.scan().await.expect("the scan runs");
    assert_eq!(
        h.present(),
        [("Heat (1995)/BDMV/STREAM/00801.m2ts".to_string(), None)]
    );

    write_blu_ray(
        &h.root.join("Heat (1995)"),
        &[("00802", 2000)],
        &[(
            "00800.mpls",
            mpls(&[("00801", 0, ticks(5000)), ("00802", 0, ticks(4000))]),
        )],
    );
    h.reconcile(&disc.join("PLAYLIST/00800.mpls"), FsEventKind::Modified)
        .await;

    assert_eq!(
        h.present(),
        [
            ("Heat (1995)/BDMV/STREAM/00801.m2ts".to_string(), Some(1)),
            ("Heat (1995)/BDMV/STREAM/00802.m2ts".to_string(), Some(2)),
        ]
    );
}

/// A disc that cannot be read whole leaves its rows exactly as they were
/// (FR-222): a VOB gone meanwhile is not marked missing and no part moves,
/// since a title chosen from part of a disc may not be its main title.
#[cfg(unix)]
#[tokio::test]
async fn a_disc_that_cannot_be_read_leaves_its_rows_as_they_were() {
    use std::os::unix::fs::PermissionsExt;
    let h = Harness::new().await;
    let disc = heat_dvd(&h.root.join("Heat (1995)"));
    // Another film keeps the root from reading as an unmounted volume.
    one_disc(&h.root.join("Ronin (1998)"), 1500);
    h.scan().await.expect("the scan runs");
    type Row = (
        Uuid,
        PathBuf,
        Option<MediaFileContent>,
        Option<DateTime<Utc>>,
    );
    let heat_rows = || -> Vec<Row> {
        h.rows()
            .into_iter()
            .filter(|row| row.path.starts_with(h.root.join("Heat (1995)")))
            .map(|row| (row.id, row.path, row.content, row.missing_since))
            .collect()
    };
    let before = heat_rows();
    assert_eq!(before.len(), 3);

    std::fs::remove_file(disc.join("VTS_02_3.VOB")).unwrap();
    std::fs::set_permissions(&disc, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&disc).is_ok() {
        std::fs::set_permissions(&disc, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: running as root, which ignores file permissions");
        return;
    }
    let scanned = h.scan().await;
    std::fs::set_permissions(&disc, std::fs::Permissions::from_mode(0o755)).unwrap();
    scanned.expect("the scan runs");

    assert_eq!(heat_rows(), before, "no row marked missing, no part moved");
}

/// A rekey reads no disc, so a disc's stream file keeps the part it was
/// given; any other file takes the part its name reads.
#[test]
fn a_rekey_keeps_a_disc_files_part() {
    let row = |path: &str, part: Option<u32>| MediaFile {
        id: Uuid::nil(),
        library_id: Uuid::nil(),
        path: PathBuf::from(path),
        hash: 1,
        size_bytes: 1,
        mtime: None,
        identity: None,
        mime_type: None,
        duration: None,
        container_format: None,
        content: Some(MediaFileContent::Movie {
            movie_entry_id: Uuid::nil(),
            part_number: part,
        }),
        status: FileStatus::Known,
        classifier_version: CLASSIFIER_VERSION,
        container_tags: None,
        scanned_at: DateTime::UNIX_EPOCH,
        updated_at: DateTime::UNIX_EPOCH,
        missing_since: None,
    };
    let cases = [
        ("/lib/Heat (1995)/VIDEO_TS/VTS_02_3.VOB", Some(3), Some(3)),
        ("/lib/Heat (1995)/BDMV/STREAM/00801.m2ts", Some(2), Some(2)),
        ("/lib/Heat (1995)/Heat (1995) - CD2.avi", Some(1), Some(2)),
    ];
    for (path, stored, expected) in cases {
        let file = row(path, stored);
        let inferred = infer_media(Path::new(path).strip_prefix("/lib").unwrap());
        assert_eq!(
            part_as_read(&file, &inferred, "heat|1995"),
            expected,
            "{path}"
        );
    }
}
