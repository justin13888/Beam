//! Skip markers Beam keeps for each file (issue #332): set by an
//! administrator, and found in the background for files whose chapters name
//! none.
//!
//! A worker walks every library's present episode files and listens to each
//! one it has not heard in its current version: the opening and the ending of
//! its audio are fingerprinted ([`AudioFingerprinter`]) and matched against a
//! few other episodes of its season, and the longest span it shares with one
//! of them becomes its intro, or its credits ([`detect_markers`]). One file at
//! a time, [`PASS_INTERVAL`] between passes, as the trickplay worker runs.
//!
//! What a detection found is recorded with the content hash it heard, even
//! when it found nothing, so a file is listened to again only once its
//! content changes. A file that could not be read just now is left for the
//! next pass, unrecorded; one whose audio cannot be decoded is recorded as
//! having none. A season with a single episode file waits, unrecorded, for a
//! second to compare with.
//!
//! The file is read through its open [`LibraryFile`] handle alone, and
//! nothing is written anywhere but the database (FR-202).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use sea_orm::DbErr;
use tracing::{debug, info, warn};
use uuid::Uuid;

use beam_domain::models::chapter::{
    MarkerDetection, MarkerKind, PlaybackMarker, ResolvedMarker, resolve_markers,
};
use beam_domain::models::{Library, MediaFile, MediaFileContent};
use beam_domain::repositories::{
    ChapterRepository, FileRepository, LibraryRepository, MarkerRepository, ShowRepository,
};
use beam_domain::services::Clock;
use beam_index::library_file::LibraryFile;
use beam_index::probe::fingerprint::{AudioFingerprint, FingerprintSpec, detect_markers};
use beam_index::services::markers::AudioFingerprinter;

/// How long the worker rests between passes.
pub const PASS_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// How many other episodes of its season an episode is compared with: the
/// nearest by number, which most likely share its intro.
pub const PARTNERS: usize = 3;

/// What the service is built from.
#[derive(Debug, Clone)]
pub struct MarkerDependencies {
    pub libraries: Arc<dyn LibraryRepository>,
    pub files: Arc<dyn FileRepository>,
    pub shows: Arc<dyn ShowRepository>,
    pub chapters: Arc<dyn ChapterRepository>,
    pub markers: Arc<dyn MarkerRepository>,
    pub fingerprinter: Arc<dyn AudioFingerprinter>,
    pub clock: Arc<dyn Clock>,
}

/// What one detection pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassReport {
    /// Files given at least one marker.
    pub with_markers: u32,
    /// Files heard, and recorded as sharing nothing -- or as having no
    /// audio that decodes.
    pub without_markers: u32,
    /// Files left for the next pass: they, or every episode they could be
    /// compared with, could not be read just now.
    pub deferred: u32,
}

/// Why an administrator's marker operation failed.
#[derive(Debug, thiserror::Error)]
pub enum MarkerError {
    /// No present file has that id.
    #[error("no file has that id")]
    FileNotFound,
    /// The span does not end after it starts, or ends past the file's end.
    #[error("{0}")]
    InvalidSpan(String),
    #[error("reading or writing markers failed: {0}")]
    Db(#[from] DbErr),
}

/// Keeps, resolves and detects files' skip markers.
#[derive(Debug)]
pub struct MarkerService {
    spec: FingerprintSpec,
    libraries: Arc<dyn LibraryRepository>,
    files: Arc<dyn FileRepository>,
    shows: Arc<dyn ShowRepository>,
    chapters: Arc<dyn ChapterRepository>,
    markers: Arc<dyn MarkerRepository>,
    fingerprinter: Arc<dyn AudioFingerprinter>,
    clock: Arc<dyn Clock>,
}

/// The episode a file holds, when it is an episode's file with a duration
/// to listen to.
fn episode_of(file: &MediaFile) -> Option<Uuid> {
    match file.content {
        Some(MediaFileContent::Episode { episode_id, .. })
            if file.duration.is_some_and(|duration| !duration.is_zero()) =>
        {
            Some(episode_id)
        }
        _ => None,
    }
}

/// What listening to one file came to.
#[derive(Clone)]
enum Heard {
    Print(Arc<AudioFingerprint>),
    /// Nothing to listen to that will change until the content does: no
    /// audio track, or audio that does not decode.
    Nothing,
    /// Not read just now; worth another try.
    Later,
}

impl MarkerService {
    pub fn new(dependencies: MarkerDependencies) -> Self {
        let MarkerDependencies {
            libraries,
            files,
            shows,
            chapters,
            markers,
            fingerprinter,
            clock,
        } = dependencies;
        Self {
            spec: FingerprintSpec::default(),
            libraries,
            files,
            shows,
            chapters,
            markers,
            fingerprinter,
            clock,
        }
    }

    /// The file `file_id`'s markers -- each kept one, else the one its
    /// chapters name -- and where each comes from.
    pub async fn markers(&self, file_id: Uuid) -> Result<Vec<ResolvedMarker>, MarkerError> {
        self.files
            .find_by_id(file_id)
            .await?
            .ok_or(MarkerError::FileNotFound)?;
        self.resolved(file_id).await
    }

    async fn resolved(&self, file_id: Uuid) -> Result<Vec<ResolvedMarker>, MarkerError> {
        let chapters = self.chapters.find_by_file_id(file_id).await?;
        let kept = self.markers.find_by_file_id(file_id).await?;
        Ok(resolve_markers(&chapters, &kept))
    }

    /// Keep `marker` as the administrator's for its kind on the file
    /// `file_id`, replacing a detected one and overriding a chapter's; the
    /// file's markers after.
    pub async fn set(
        &self,
        file_id: Uuid,
        marker: PlaybackMarker,
    ) -> Result<Vec<ResolvedMarker>, MarkerError> {
        let file = self
            .files
            .find_by_id(file_id)
            .await?
            .ok_or(MarkerError::FileNotFound)?;
        if marker.end_ms <= marker.start_ms {
            return Err(MarkerError::InvalidSpan(format!(
                "a marker must end after it starts; this one starts at {} ms and ends at {} ms",
                marker.start_ms, marker.end_ms
            )));
        }
        if let Some(duration) = file.duration {
            let length_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
            if marker.end_ms > length_ms {
                return Err(MarkerError::InvalidSpan(format!(
                    "the file is {length_ms} ms long, so a marker cannot end at {} ms",
                    marker.end_ms
                )));
            }
        }
        self.markers.set_admin(file_id, marker).await?;
        self.resolved(file_id).await
    }

    /// Stop keeping the file's marker of `kind`, whoever set it, so its
    /// chapters decide that kind again; the file's markers after. Clearing a
    /// kind the file keeps none of changes nothing.
    pub async fn clear(
        &self,
        file_id: Uuid,
        kind: MarkerKind,
    ) -> Result<Vec<ResolvedMarker>, MarkerError> {
        self.files
            .find_by_id(file_id)
            .await?
            .ok_or(MarkerError::FileNotFound)?;
        self.markers.clear(file_id, kind).await?;
        self.resolved(file_id).await
    }

    /// Run passes forever, [`PASS_INTERVAL`] apart. A pass that fails to
    /// read the catalogue is logged and the next one tries again.
    pub async fn run(&self) {
        loop {
            match self.run_pass().await {
                Ok(report) if report == PassReport::default() => {
                    debug!("no episode needed listening to for markers");
                }
                Ok(report) => info!(?report, "marker detection pass finished"),
                Err(error) => warn!(%error, "marker detection pass failed"),
            }
            self.clock.sleep(PASS_INTERVAL).await;
        }
    }

    /// One pass: listen to every episode file not yet heard in its current
    /// version, season by season, and record what each shares.
    pub async fn run_pass(&self) -> Result<PassReport, DbErr> {
        let mut report = PassReport::default();
        for library in self.libraries.find_all().await? {
            let episodes: Vec<(MediaFile, Uuid)> = self
                .files
                .find_all_by_library(library.id)
                .await?
                .into_iter()
                .filter_map(|file| episode_of(&file).map(|episode| (file, episode)))
                .collect();
            let ids: Vec<Uuid> = episodes.iter().map(|(file, _)| file.id).collect();
            let heard = self.markers.detected_hashes(&ids).await?;
            let pending: Vec<&(MediaFile, Uuid)> = episodes
                .iter()
                .filter(|(file, _)| heard.get(&file.id) != Some(&file.hash))
                .collect();
            if pending.is_empty() {
                continue;
            }

            // The seasons the pending files are of, each with every
            // episode's number.
            let mut seasons: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
            for (file, episode_id) in &pending {
                match self.shows.find_episode_by_id(*episode_id).await? {
                    Some(episode) => seasons.entry(episode.season_id).or_default().push(file.id),
                    None => debug!(file = %file.id, "an episode file's episode is gone"),
                }
            }
            for (season_id, pending_ids) in seasons {
                let numbers: HashMap<Uuid, u32> = self
                    .shows
                    .find_episodes_by_season_id(season_id)
                    .await?
                    .into_iter()
                    .map(|episode| (episode.id, episode.episode_number))
                    .collect();
                let members: Vec<(&MediaFile, Uuid, u32)> = episodes
                    .iter()
                    .filter_map(|(file, episode_id)| {
                        numbers
                            .get(episode_id)
                            .map(|number| (file, *episode_id, *number))
                    })
                    .collect();
                self.detect_season(&library, &members, &pending_ids, &mut report)
                    .await?;
            }
        }
        Ok(report)
    }

    /// Listen to each of `pending` -- files among `members`, one season's
    /// episode files with their episode and its number -- and record what
    /// each shares with the nearest other episodes.
    async fn detect_season(
        &self,
        library: &Library,
        members: &[(&MediaFile, Uuid, u32)],
        pending: &[Uuid],
        report: &mut PassReport,
    ) -> Result<(), DbErr> {
        let mut prints: HashMap<Uuid, Heard> = HashMap::new();
        for &(file, episode_id, number) in members
            .iter()
            .filter(|(file, ..)| pending.contains(&file.id))
        {
            // Other episodes, nearest first; another file of this episode
            // is the same programme, not a season-mate.
            let mut others: Vec<&(&MediaFile, Uuid, u32)> = members
                .iter()
                .filter(|(_, other, _)| *other != episode_id)
                .collect();
            if others.is_empty() {
                debug!(file = %file.id, "no other episode of its season to compare with yet");
                continue;
            }
            others
                .sort_by_key(|(other, _, other_number)| (other_number.abs_diff(number), other.id));

            // The file is opened before any partner is decoded, so a file
            // that does not open costs one failed open each pass rather than
            // its partners' decodes; one already heard this pass as a
            // partner is not opened again.
            let opened = match prints.get(&file.id) {
                Some(Heard::Later) => {
                    report.deferred += 1;
                    continue;
                }
                Some(Heard::Nothing) => {
                    self.record(file, Vec::new()).await?;
                    report.without_markers += 1;
                    continue;
                }
                Some(Heard::Print(_)) => None,
                None => match Self::open(library, file) {
                    Some(opened) => Some(opened),
                    None => {
                        prints.insert(file.id, Heard::Later);
                        report.deferred += 1;
                        continue;
                    }
                },
            };

            // Then the partners: with none to compare with, the file is not
            // decoded at all, so a season whose other files never decode
            // costs no decode of this one.
            let mut partners: Vec<Arc<AudioFingerprint>> = Vec::new();
            for (other, ..) in others {
                if partners.len() == PARTNERS {
                    break;
                }
                if let Heard::Print(print) = self.listen(library, other, &mut prints).await {
                    partners.push(print);
                }
            }
            if partners.is_empty() {
                report.deferred += 1;
                continue;
            }
            let heard = match opened {
                Some(opened) => {
                    let heard = self.hear(file, opened).await;
                    prints.insert(file.id, heard.clone());
                    heard
                }
                None => self.listen(library, file, &mut prints).await,
            };
            let own = match heard {
                Heard::Print(print) => print,
                Heard::Nothing => {
                    self.record(file, Vec::new()).await?;
                    report.without_markers += 1;
                    continue;
                }
                Heard::Later => {
                    report.deferred += 1;
                    continue;
                }
            };
            // Matching compares every alignment of two fingerprints -- a
            // second or so of work per episode -- so it runs off the
            // runtime's threads, as the decoding does.
            let found = match tokio::task::spawn_blocking(move || {
                let partners: Vec<&AudioFingerprint> = partners.iter().map(Arc::as_ref).collect();
                detect_markers(&own, &partners)
            })
            .await
            {
                Ok(found) => found,
                Err(error) => {
                    warn!(file = %file.id, %error, "marker matching did not finish");
                    report.deferred += 1;
                    continue;
                }
            };
            if found.is_empty() {
                report.without_markers += 1;
            } else {
                report.with_markers += 1;
            }
            self.record(file, found).await?;
        }
        Ok(())
    }

    /// `file`'s fingerprint, heard once per pass.
    async fn listen(
        &self,
        library: &Library,
        file: &MediaFile,
        prints: &mut HashMap<Uuid, Heard>,
    ) -> Heard {
        if let Some(heard) = prints.get(&file.id) {
            return heard.clone();
        }
        let heard = match Self::open(library, file) {
            None => Heard::Later,
            Some(opened) => self.hear(file, opened).await,
        };
        prints.insert(file.id, heard.clone());
        heard
    }

    /// `file` opened beneath its library's root, or `None` -- logged --
    /// where it cannot be just now.
    fn open(library: &Library, file: &MediaFile) -> Option<LibraryFile> {
        match LibraryFile::open(&library.root_path, &file.path) {
            Ok(opened) => Some(opened),
            Err(error) => {
                debug!(path = %file.path.display(), %error, "file not opened for marker detection");
                None
            }
        }
    }

    /// The fingerprint of `file`, already `opened`.
    async fn hear(&self, file: &MediaFile, opened: LibraryFile) -> Heard {
        match self.fingerprinter.fingerprint(opened, self.spec).await {
            Ok(Some(print)) => Heard::Print(Arc::new(print)),
            Ok(None) => Heard::Nothing,
            Err(error) if error.is_content_fault() => {
                warn!(path = %file.path.display(), %error, "no audio to detect markers in");
                Heard::Nothing
            }
            Err(error) => {
                warn!(path = %file.path.display(), %error, "marker detection left for a later pass");
                Heard::Later
            }
        }
    }

    async fn record(&self, file: &MediaFile, markers: Vec<PlaybackMarker>) -> Result<(), DbErr> {
        self.markers
            .record_detection(MarkerDetection {
                file_id: file.id,
                file_hash: file.hash,
                markers,
                detected_at: self.clock.now(),
            })
            .await
    }
}

#[cfg(test)]
#[path = "markers_tests.rs"]
mod markers_tests;
