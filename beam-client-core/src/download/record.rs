//! The download vocabulary both clients render.

use serde::{Deserialize, Serialize};

/// Where one download has got to.
///
/// The union of what the two clients modelled separately. `WaitingForNetwork`
/// existed only on Android and `Paused` carried a fraction only on Apple; both
/// are real states a viewer can be in on either platform, so both are here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum DownloadState {
    /// Accepted, nothing transferred yet.
    Queued,
    /// Bytes are moving.
    Downloading,
    /// Stopped by the viewer. Resumable, and the fetched bytes are kept.
    Paused,
    /// Stopped by policy rather than by the viewer -- no connection, or a
    /// metered one while the viewer asked for unmetered only.
    ///
    /// Distinct from `Paused` because the remedy is different: this one
    /// resumes on its own, and telling a viewer their download is "paused"
    /// when it is waiting for Wi-Fi invites them to go looking for a button
    /// that will not help.
    WaitingForNetwork,
    /// On disk and playable with no network at all.
    Completed,
    /// Stopped by an error, with something worth showing a person.
    Failed {
        /// Why it stopped, phrased for a viewer rather than a log.
        message: String,
    },
}

impl DownloadState {
    /// Whether the file can be played with no network.
    #[must_use]
    pub fn is_playable_offline(&self) -> bool {
        matches!(self, Self::Completed)
    }

    /// Whether bytes are expected to move without the viewer doing anything.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Downloading | Self::WaitingForNetwork
        )
    }
}

/// One offline download, as a downloads screen renders it.
///
/// Carries its own display fields rather than identifiers to resolve. The
/// screen has to render with no network at all, so a title that had to be
/// fetched to be shown would defeat the feature it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct DownloadRecord {
    /// The file being downloaded. Also the download's identity: a file is
    /// downloaded at most once.
    pub file_id: String,
    /// The server it came from, so removing a server can take its downloads
    /// with it.
    pub server_id: String,
    /// The title it belongs to.
    pub media_id: String,
    /// The episode, when the title is a series.
    pub episode_id: Option<String>,
    /// First line.
    pub title: String,
    /// Second line, such as "S2 E4 - Return".
    pub subtitle: Option<String>,
    /// Artwork.
    pub poster_url: Option<String>,
    /// The server's filename, which the container is read from when the
    /// catalogue did not record one.
    pub file_name: Option<String>,
    /// Where it has got to.
    pub state: DownloadState,
    /// Bytes fetched so far.
    pub downloaded_bytes: u64,
    /// Total bytes, or `None` while the server has not said.
    pub total_bytes: Option<u64>,
}

impl DownloadRecord {
    /// Fraction complete in `0.0..=1.0`, or `None` while the total is unknown.
    ///
    /// Stays `None` rather than guessing: a determinate progress bar that
    /// starts at a made-up value reads as broken, where an indeterminate one
    /// reads as honest.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        match self.total_bytes {
            Some(total) if total > 0 => {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a ratio for a progress bar; f64 holds every byte count \
                              below 2^53, which is 8 PiB"
                )]
                let fraction = self.downloaded_bytes as f64 / total as f64;
                Some(fraction.clamp(0.0, 1.0))
            }
            _ => None,
        }
    }

    /// Whether the file can be played with no network.
    #[must_use]
    pub fn is_playable_offline(&self) -> bool {
        self.state.is_playable_offline()
    }

    /// The name to store the file under.
    ///
    /// Keyed by file id rather than by the server's filename, because two
    /// titles can share one. The extension is preserved: both Apple engines and
    /// `EngineSelector` read it when the catalogue did not record a container.
    #[must_use]
    pub fn stored_name(&self) -> String {
        let extension = self
            .file_name
            .as_deref()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, ext)| ext)
            .filter(|ext| !ext.is_empty() && !ext.contains('/'));

        match extension {
            Some(ext) => format!("{}.{ext}", self.file_id),
            None => self.file_id.clone(),
        }
    }
}

/// One download as a platform engine currently sees it.
///
/// Exists so a whole engine sweep is applied in a single pass. Media3 reports
/// every download on every callback, and calling a per-file method for each
/// would reload and rewrite the entire catalogue twice per download per tick --
/// quadratic in the number of downloads, on the hot path of a progress bar.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DownloadReport {
    /// Which download.
    pub file_id: String,
    /// Bytes fetched so far.
    pub downloaded_bytes: u64,
    /// Total bytes, or `None` where the engine has not been told yet.
    pub total_bytes: Option<u64>,
    /// The engine's own view of where the transfer has got to.
    pub state: DownloadState,
}

/// Fraction complete in `0.0..=1.0`, or `None` while the total is unknown.
///
/// A free function because UniFFI records are plain data on the foreign side:
/// a method on `DownloadRecord` would not cross the boundary, and both clients
/// would then compute this themselves -- which is how they came to disagree in
/// the first place.
#[uniffi::export]
#[must_use]
pub fn download_fraction(record: &DownloadRecord) -> Option<f64> {
    record.fraction()
}

/// Whether the file can be played with no network.
///
/// See [`download_fraction`] for why this is a free function.
#[uniffi::export]
#[must_use]
pub fn download_is_playable_offline(record: &DownloadRecord) -> bool {
    record.is_playable_offline()
}

/// The name the file is stored under on disk.
///
/// See [`download_fraction`] for why this is a free function.
#[uniffi::export]
#[must_use]
pub fn download_stored_name(record: &DownloadRecord) -> String {
    record.stored_name()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(downloaded: u64, total: Option<u64>) -> DownloadRecord {
        DownloadRecord {
            file_id: "file-1".to_owned(),
            server_id: "server-1".to_owned(),
            media_id: "media-1".to_owned(),
            episode_id: None,
            title: "The Title".to_owned(),
            subtitle: None,
            poster_url: None,
            file_name: None,
            state: DownloadState::Downloading,
            downloaded_bytes: downloaded,
            total_bytes: total,
        }
    }

    #[test]
    fn fraction_is_absent_until_the_server_declares_a_total() {
        assert_eq!(record(1_024, None).fraction(), None);
        assert_eq!(record(1_024, Some(0)).fraction(), None);
    }

    #[test]
    fn fraction_is_the_ratio_of_fetched_to_total() {
        assert_eq!(record(512, Some(2_048)).fraction(), Some(0.25));
    }

    /// Both engines can report more bytes than they declared -- a redirect that
    /// changes the body, or a `Content-Length` that excluded a trailer. A
    /// progress bar past its end reads as a bug in Beam.
    #[test]
    fn fraction_never_exceeds_one_even_when_more_bytes_arrive_than_promised() {
        assert_eq!(record(4_096, Some(2_048)).fraction(), Some(1.0));
    }

    #[test]
    fn only_a_completed_download_is_playable_offline() {
        for state in [
            DownloadState::Queued,
            DownloadState::Downloading,
            DownloadState::Paused,
            DownloadState::WaitingForNetwork,
            DownloadState::Failed {
                message: "gone".to_owned(),
            },
        ] {
            assert!(
                !state.is_playable_offline(),
                "{state:?} must not claim to be playable offline"
            );
        }
        assert!(DownloadState::Completed.is_playable_offline());
    }

    /// `is_active` is what decides whether an arriving byte count means the
    /// transfer has started. A completed or failed download must not be
    /// resurrected by a late callback, and a paused one must not restart
    /// itself.
    #[test]
    fn only_states_that_expect_bytes_are_active() {
        assert!(DownloadState::Queued.is_active());
        assert!(DownloadState::Downloading.is_active());
        assert!(DownloadState::WaitingForNetwork.is_active());

        assert!(!DownloadState::Paused.is_active());
        assert!(!DownloadState::Completed.is_active());
        assert!(
            !DownloadState::Failed {
                message: "gone".to_owned()
            }
            .is_active()
        );
    }

    #[test]
    fn the_stored_name_keeps_the_container_extension() {
        let mut r = record(0, None);
        r.file_name = Some("Show.S01E01.1080p.mkv".to_owned());
        assert_eq!(r.stored_name(), "file-1.mkv");
    }

    /// Two titles can share a filename, so the name on disk is keyed by file id
    /// rather than by what the server called it.
    #[test]
    fn the_stored_name_does_not_use_the_servers_filename() {
        let mut r = record(0, None);
        r.file_name = Some("Show.S01E01.1080p.mkv".to_owned());
        assert!(!r.stored_name().contains("Show"));
    }

    #[test]
    fn a_file_name_without_a_usable_extension_stores_under_the_bare_id() {
        for name in [None, Some("movie".to_owned()), Some("trailing.".to_owned())] {
            let mut r = record(0, None);
            r.file_name = name.clone();
            assert_eq!(r.stored_name(), "file-1", "for {name:?}");
        }
    }

    /// A filename that is really a path must not turn into a name with a
    /// separator in it, which would either escape the download directory or
    /// fail to open.
    #[test]
    fn a_path_like_file_name_does_not_leak_a_separator_into_the_stored_name() {
        let mut r = record(0, None);
        r.file_name = Some("archive.tar/../../etc/passwd".to_owned());
        assert_eq!(r.stored_name(), "file-1");
    }
}
