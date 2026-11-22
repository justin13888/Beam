//! The marker worker over a real `TempDir` library, with in-memory
//! repositories and the fake fingerprinter answering each episode's audio
//! (issue #332). The real matching runs on the fake's fingerprints; the
//! FFmpeg decode itself is tested against real files in `beam-index`.

use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;

use super::*;
use beam_domain::models::chapter::{StoredMarker, StoredMarkerSource};
use beam_domain::models::file::CreateMediaFile;
use beam_domain::models::{CreateEpisode, CreateLibrary, CreateShow, FileStatus};
use beam_domain::repositories::chapter::in_memory::InMemoryChapterRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::markers::MockMarkerRepository;
use beam_domain::repositories::markers::in_memory::InMemoryMarkerRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::services::TestClock;
use beam_domain::utils::media_path::EpisodeNumbering;
use beam_index::probe::fingerprint::{FRAME_MS, SubFingerprint};
use beam_index::services::{FakeAudioFingerprinter, FakeListen};

/// A deterministic stream of fingerprint windows: xorshift64.
struct Windows(u64);

impl Windows {
    fn take(&mut self, ms: u64) -> Vec<SubFingerprint> {
        (0..ms / FRAME_MS)
            .map(|_| {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                Some((self.0 >> 32) as u32)
            })
            .collect()
    }
}

/// The season's theme and credits music, as fingerprint windows.
struct Season {
    theme: Vec<SubFingerprint>,
    credits: Vec<SubFingerprint>,
}

impl Season {
    fn new() -> Self {
        let mut windows = Windows(0x5eed);
        Self {
            theme: windows.take(30_000),
            credits: windows.take(40_000),
        }
    }

    /// An episode's fingerprint: two minutes of opening with the theme at
    /// `theme_at_ms`, and two minutes of ending from 20 minutes in with the
    /// credits `credits_at_ms` into it; everything else its own.
    fn episode(&self, seed: u64, theme_at_ms: u64, credits_at_ms: u64) -> AudioFingerprint {
        let mut own = Windows(seed);
        let mut head = own.take(120_000);
        let at = (theme_at_ms / FRAME_MS) as usize;
        head[at..at + self.theme.len()].copy_from_slice(&self.theme);
        let mut tail = own.take(120_000);
        let at = (credits_at_ms / FRAME_MS) as usize;
        tail[at..at + self.credits.len()].copy_from_slice(&self.credits);
        AudioFingerprint {
            head,
            tail,
            tail_start_ms: 1_200_000,
        }
    }
}

struct Harness {
    _dir: TempDir,
    root: PathBuf,
    library: Library,
    files: Arc<InMemoryFileRepository>,
    shows: Arc<InMemoryShowRepository>,
    markers: Arc<InMemoryMarkerRepository>,
    listener: Arc<FakeAudioFingerprinter>,
    season_id: Uuid,
    service: MarkerService,
}

impl Harness {
    async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let libraries = Arc::new(InMemoryLibraryRepository::default());
        let library = libraries
            .create(CreateLibrary {
                name: "Shows".into(),
                root_path: root.clone(),
                description: None,
            })
            .await
            .unwrap();
        let files = Arc::new(InMemoryFileRepository::default());
        let shows = Arc::new(InMemoryShowRepository::default());
        let show = shows
            .find_or_create_by_identity(CreateShow::new("Show", None))
            .await
            .unwrap();
        let season_id = shows.find_or_create_season(show.id, 1).await.unwrap().id;
        let markers = Arc::new(InMemoryMarkerRepository::default());
        let listener = Arc::new(FakeAudioFingerprinter::default());
        let service = MarkerService::new(MarkerDependencies {
            libraries,
            files: files.clone(),
            shows: shows.clone(),
            chapters: Arc::new(InMemoryChapterRepository::default()),
            markers: markers.clone(),
            fingerprinter: listener.clone(),
            clock: Arc::new(TestClock::new()),
        });
        Self {
            _dir: dir,
            root,
            library,
            files,
            shows,
            markers,
            listener,
            season_id,
            service,
        }
    }

    /// Episode `number` of the season.
    async fn episode(&self, number: u32) -> Uuid {
        self.shows
            .find_or_create_episode(CreateEpisode {
                season_id: self.season_id,
                episode_number: number,
                title: format!("Episode {number}"),
                runtime: None,
                air_date: None,
                numbering: EpisodeNumbering::Standard,
            })
            .await
            .unwrap()
            .id
    }

    /// A file of `content` named `name`, hashed `hash`, on disk and indexed.
    async fn file(&self, name: &str, hash: u64, content: MediaFileContent) -> MediaFile {
        let path = self.root.join(name);
        std::fs::write(&path, b"not really an episode").unwrap();
        self.files
            .create(CreateMediaFile {
                hash,
                duration: Some(Duration::from_secs(22 * 60)),
                content: Some(content),
                status: FileStatus::Known,
                ..beam_domain::test_utils::create_media_file(self.library.id, path)
            })
            .await
            .unwrap()
    }

    /// A file of episode `number`, answered with `listen`.
    async fn episode_file(&self, number: u32, listen: FakeListen) -> MediaFile {
        let episode = self.episode(number).await;
        let file = self
            .file(
                &format!("S01E{number:02}.mkv"),
                u64::from(number),
                MediaFileContent::episode(episode),
            )
            .await;
        self.listener.answer(file.path.clone(), listen);
        file
    }

    async fn kept(&self, file: &MediaFile) -> Vec<StoredMarker> {
        self.markers.find_by_file_id(file.id).await.unwrap()
    }

    /// Record `file` as already listened to in its current version, so a
    /// pass reaches it only as another file's partner.
    async fn heard_before(&self, file: &MediaFile) {
        self.markers
            .record_detection(MarkerDetection {
                file_id: file.id,
                file_hash: file.hash,
                markers: Vec::new(),
                detected_at: TestClock::new().now(),
            })
            .await
            .unwrap();
    }

    fn times_asked(&self, file: &MediaFile) -> usize {
        self.listener
            .asked()
            .iter()
            .filter(|path| **path == file.path)
            .count()
    }
}

/// Whether `marker` is of `kind` and spans `start_ms..end_ms`, give or take
/// the half window a span is timed from.
fn spans(marker: &StoredMarker, kind: MarkerKind, start_ms: u64, end_ms: u64) -> bool {
    let near = |got: u64, want: u64| got.abs_diff(want) <= 256;
    marker.marker.kind == kind
        && near(marker.marker.start_ms, start_ms)
        && near(marker.marker.end_ms, end_ms)
}

#[tokio::test]
async fn a_seasons_episodes_are_given_the_intro_and_credits_they_share_and_heard_once() {
    let h = Harness::new().await;
    let season = Season::new();
    let one = h
        .episode_file(1, FakeListen::Heard(season.episode(11, 10_000, 50_000)))
        .await;
    let two = h
        .episode_file(2, FakeListen::Heard(season.episode(12, 60_000, 30_000)))
        .await;
    let three = h
        .episode_file(3, FakeListen::Heard(season.episode(13, 0, 70_000)))
        .await;

    let report = h.service.run_pass().await.unwrap();

    assert_eq!(
        report,
        PassReport {
            with_markers: 3,
            without_markers: 0,
            deferred: 0,
        }
    );
    for (file, theme_at, credits_at) in [
        (&one, 10_000, 1_250_000),
        (&two, 60_000, 1_230_000),
        (&three, 0, 1_270_000),
    ] {
        let kept = h.kept(file).await;
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(
            spans(&kept[0], MarkerKind::Intro, theme_at, theme_at + 30_000),
            "{kept:?}"
        );
        assert!(
            spans(
                &kept[1],
                MarkerKind::Credits,
                credits_at,
                credits_at + 40_000
            ),
            "{kept:?}"
        );
        assert!(
            kept.iter()
                .all(|kept| kept.source == StoredMarkerSource::Detected)
        );
        // Each file is decoded once in the pass, however many it was
        // compared with.
        assert_eq!(h.times_asked(file), 1);
    }
    let heard = h
        .markers
        .detected_hashes(&[one.id, two.id, three.id])
        .await
        .unwrap();
    assert_eq!(heard.get(&one.id), Some(&one.hash));
    assert_eq!(heard.get(&three.id), Some(&three.hash));

    // Nothing changed, so the next pass listens to nothing.
    assert_eq!(h.service.run_pass().await.unwrap(), PassReport::default());
    assert_eq!(h.listener.asked().len(), 3);
}

#[tokio::test]
async fn a_changed_file_is_heard_again_and_an_administrators_marker_survives_it() {
    let h = Harness::new().await;
    let season = Season::new();
    let one = h
        .episode_file(1, FakeListen::Heard(season.episode(11, 10_000, 50_000)))
        .await;
    h.episode_file(2, FakeListen::Heard(season.episode(12, 60_000, 30_000)))
        .await;
    h.service.run_pass().await.unwrap();
    let admins = PlaybackMarker {
        kind: MarkerKind::Intro,
        start_ms: 9_000,
        end_ms: 41_000,
    };
    h.service.set(one.id, admins).await.unwrap();

    // The file is replaced by a version with its theme elsewhere.
    h.files.files.lock().unwrap().get_mut(&one.id).unwrap().hash = 99;
    h.listener.answer(
        one.path.clone(),
        FakeListen::Heard(season.episode(21, 40_000, 10_000)),
    );
    let report = h.service.run_pass().await.unwrap();

    assert_eq!(report.with_markers, 1, "only the changed file: {report:?}");
    assert_eq!(h.times_asked(&one), 2);
    let kept = h.kept(&one).await;
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert_eq!(
        kept[0],
        StoredMarker {
            marker: admins,
            source: StoredMarkerSource::Admin
        }
    );
    assert!(
        spans(&kept[1], MarkerKind::Credits, 1_210_000, 1_250_000),
        "{kept:?}"
    );
    assert_eq!(
        h.markers
            .detected_hashes(&[one.id])
            .await
            .unwrap()
            .get(&one.id),
        Some(&99)
    );
}

#[tokio::test]
async fn audio_that_does_not_decode_is_recorded_as_none_and_an_unreadable_file_waits() {
    let h = Harness::new().await;
    let season = Season::new();
    let undecodable = h.episode_file(1, FakeListen::Undecodable).await;
    let silent = h.episode_file(2, FakeListen::NoAudio).await;
    let unreadable = h.episode_file(3, FakeListen::Unreadable).await;
    let heard = h
        .episode_file(4, FakeListen::Heard(season.episode(14, 0, 0)))
        .await;

    let report = h.service.run_pass().await.unwrap();

    // The one heard file has nobody it can be compared with: everyone else
    // gave no fingerprint.
    assert_eq!(
        report,
        PassReport {
            with_markers: 0,
            without_markers: 2,
            deferred: 2,
        }
    );
    let recorded = h
        .markers
        .detected_hashes(&[undecodable.id, silent.id, unreadable.id, heard.id])
        .await
        .unwrap();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert!(recorded.contains_key(&undecodable.id) && recorded.contains_key(&silent.id));
    assert!(h.kept(&undecodable).await.is_empty());

    // Once the unreadable file can be read, both it and the waiting file
    // are given their markers; the files recorded as having none are not
    // recorded again.
    h.listener.answer(
        unreadable.path.clone(),
        FakeListen::Heard(season.episode(13, 20_000, 20_000)),
    );
    let report = h.service.run_pass().await.unwrap();
    assert_eq!(
        report,
        PassReport {
            with_markers: 2,
            without_markers: 0,
            deferred: 0,
        }
    );
    assert!(!h.kept(&unreadable).await.is_empty());
    assert!(!h.kept(&heard).await.is_empty());
}

#[tokio::test]
async fn a_lone_episode_waits_for_another_and_two_cuts_of_one_are_not_season_mates() {
    let h = Harness::new().await;
    let season = Season::new();
    let episode = h.episode(1).await;
    let cut = h
        .file("S01E01.mkv", 1, MediaFileContent::episode(episode))
        .await;
    let other_cut = h
        .file(
            "S01E01 - Extended.mkv",
            2,
            MediaFileContent::episode(episode),
        )
        .await;
    for file in [&cut, &other_cut] {
        h.listener.answer(
            file.path.clone(),
            FakeListen::Heard(season.episode(1, 0, 0)),
        );
    }
    let movie = h
        .file("Movie.mkv", 3, MediaFileContent::movie(Uuid::new_v4()))
        .await;

    assert_eq!(h.service.run_pass().await.unwrap(), PassReport::default());
    assert!(h.listener.asked().is_empty(), "{:?}", h.listener.asked());
    assert!(
        h.markers
            .detected_hashes(&[cut.id, other_cut.id, movie.id])
            .await
            .unwrap()
            .is_empty()
    );

    h.episode_file(2, FakeListen::Heard(season.episode(2, 30_000, 30_000)))
        .await;
    let report = h.service.run_pass().await.unwrap();
    assert_eq!(report.with_markers, 3, "{report:?}");
    assert_eq!(h.times_asked(&movie), 0);
}

#[tokio::test]
async fn a_file_whose_season_mates_give_no_fingerprint_is_not_decoded() {
    let h = Harness::new().await;
    let season = Season::new();
    let pending = h
        .episode_file(1, FakeListen::Heard(season.episode(11, 0, 0)))
        .await;
    let silent = h.episode_file(2, FakeListen::NoAudio).await;
    let undecodable = h.episode_file(3, FakeListen::Undecodable).await;
    h.heard_before(&silent).await;
    h.heard_before(&undecodable).await;

    let report = h.service.run_pass().await.unwrap();

    assert_eq!(
        report,
        PassReport {
            with_markers: 0,
            without_markers: 0,
            deferred: 1,
        }
    );
    // The season-mates are listened to as partners and give nothing, so
    // the pending file -- reachable only as itself -- is never decoded.
    assert_eq!(h.times_asked(&silent), 1);
    assert_eq!(h.times_asked(&undecodable), 1);
    assert_eq!(h.times_asked(&pending), 0);
    assert!(
        h.markers
            .detected_hashes(&[pending.id])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_file_that_does_not_open_waits_without_its_partners_being_decoded() {
    let h = Harness::new().await;
    let season = Season::new();
    let missing = h
        .episode_file(1, FakeListen::Heard(season.episode(11, 10_000, 50_000)))
        .await;
    let two = h
        .episode_file(2, FakeListen::Heard(season.episode(12, 60_000, 30_000)))
        .await;
    let three = h
        .episode_file(3, FakeListen::Heard(season.episode(13, 0, 70_000)))
        .await;
    h.heard_before(&two).await;
    h.heard_before(&three).await;
    std::fs::remove_file(&missing.path).unwrap();

    let report = h.service.run_pass().await.unwrap();

    assert_eq!(
        report,
        PassReport {
            with_markers: 0,
            without_markers: 0,
            deferred: 1,
        }
    );
    assert!(h.listener.asked().is_empty(), "{:?}", h.listener.asked());

    // Once the file is back it is heard against the partners it waited for.
    std::fs::write(&missing.path, b"not really an episode").unwrap();
    let report = h.service.run_pass().await.unwrap();
    assert_eq!(report.with_markers, 1, "{report:?}");
    assert_eq!(h.times_asked(&missing), 1);
    assert_eq!(h.times_asked(&two), 1);
    assert!(!h.kept(&missing).await.is_empty());
}

#[tokio::test]
async fn a_store_that_fails_fails_the_pass() {
    let h = Harness::new().await;
    h.episode_file(1, FakeListen::NoAudio).await;
    let mut markers = MockMarkerRepository::new();
    markers
        .expect_detected_hashes()
        .returning(|_| Err(DbErr::Custom("store unavailable".into())));
    let service = MarkerService::new(MarkerDependencies {
        libraries: {
            let libraries = InMemoryLibraryRepository::default();
            libraries
                .libraries
                .lock()
                .unwrap()
                .insert(h.library.id, h.library.clone());
            Arc::new(libraries)
        },
        files: h.files.clone(),
        shows: h.shows.clone(),
        chapters: Arc::new(InMemoryChapterRepository::default()),
        markers: Arc::new(markers),
        fingerprinter: h.listener.clone(),
        clock: Arc::new(TestClock::new()),
    });

    assert!(service.run_pass().await.is_err());
    assert!(h.listener.asked().is_empty());
}
