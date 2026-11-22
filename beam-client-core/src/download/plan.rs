//! What to transfer for a download.
//!
//! A title is not always one file. A multi-part movie (#233) is several, and
//! a subtitle file beside a part (#189) is one more each; a download of the
//! first file alone plays the first disc and stops, with no subtitles. So the
//! core names every item once, at enqueue, and the platform transfers exactly
//! what it names.

use crate::capability::select::{MediaSourceView, SubtitleOrigin, SubtitleTrackView};
use crate::error::BeamError;
use serde::{Deserialize, Serialize};

/// Exactly what the platform transfers for one download.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct DownloadPlan {
    /// The server every item is fetched from, with its own session.
    pub server_id: String,
    /// The movie or episode whose sources the plan was built from; `None`
    /// when it was built for the file alone.
    pub title_id: Option<String>,
    /// The file the viewer chose.
    pub file_id: String,
    /// The episode, when the title is a series.
    pub episode_id: Option<String>,
    /// The files to fetch, in play order. Never empty.
    pub parts: Vec<DownloadPart>,
    /// The subtitle files beside them.
    pub subtitles: Vec<DownloadSubtitle>,
}

impl DownloadPlan {
    /// Every item's id, parts first: the ids the platform's engine keys its
    /// transfers by, and reports back in a [`super::TransferReport`].
    pub fn item_ids(&self) -> impl Iterator<Item = &str> {
        self.parts
            .iter()
            .map(|part| part.file_id.as_str())
            .chain(self.subtitles.iter().map(|sub| sub.sidecar_id.as_str()))
    }
}

/// One file of a download.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct DownloadPart {
    /// The file, which is also this item's id.
    pub file_id: String,
    /// The part the filename names (`CD2` is `2`); absent for a whole file.
    pub part_number: Option<u32>,
    /// Absolute URL to fetch it from: the server's download route, not its
    /// stream route, so the transfer is the file as stored.
    pub download_url: String,
    /// Its size as the catalog lists it; `None` when the plan was built
    /// without the catalog.
    pub size_bytes: Option<u64>,
    /// Its duration, where probing determined one.
    pub duration_secs: Option<f64>,
}

/// One subtitle file beside a part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct DownloadSubtitle {
    /// The subtitle file's identifier, which is also this item's id.
    pub sidecar_id: String,
    /// The part it times.
    pub part_file_id: String,
    /// Absolute URL of the file as stored.
    pub url: String,
    /// FFmpeg's codec name: `subrip`, `webvtt`, `ass`.
    pub codec: String,
    /// The ISO 639-2/B language code (`eng`, `spa`).
    pub language: Option<String>,
    /// The track's own title (`Commentary`, `pt-BR`).
    pub title: Option<String>,
    /// Whether the file marks this track as the default.
    pub is_default: bool,
    /// Subtitles only for foreign-language dialogue.
    pub is_forced: bool,
    /// Subtitles for the deaf and hard of hearing (SDH).
    pub is_hearing_impaired: bool,
}

/// The plan for `file_id`, from the source among `sources` that holds it.
///
/// Every part of that source, and every subtitle file beside one. Where no
/// source holds the file -- the lookup failed, or the title has no sources --
/// the plan is the file alone, with no size: what a download fetched before
/// the core knew a title's parts, so nothing that downloaded then stops
/// downloading now.
///
/// `download_url` resolves a file id to its absolute download URL on the
/// server's origin.
///
/// # Errors
///
/// Whatever `download_url` returns for an id it refuses.
pub fn plan_download(
    server_id: &str,
    title_id: Option<&str>,
    episode_id: Option<&str>,
    file_id: &str,
    sources: &[MediaSourceView],
    download_url: impl Fn(&str) -> Result<String, BeamError>,
) -> Result<DownloadPlan, BeamError> {
    let source = sources.iter().find(|source| source.contains_file(file_id));
    let mut parts = Vec::new();
    let mut subtitles = Vec::new();
    match source {
        None => parts.push(DownloadPart {
            file_id: file_id.to_owned(),
            part_number: None,
            download_url: download_url(file_id)?,
            size_bytes: None,
            duration_secs: None,
        }),
        Some(source) if source.parts.is_empty() => {
            parts.push(DownloadPart {
                file_id: source.file_id.clone(),
                part_number: None,
                download_url: download_url(&source.file_id)?,
                size_bytes: Some(source.size_bytes),
                duration_secs: source.duration_secs,
            });
            subtitles.extend(sidecars(&source.file_id, &source.subtitle_tracks));
        }
        Some(source) => {
            for part in &source.parts {
                parts.push(DownloadPart {
                    file_id: part.file_id.clone(),
                    part_number: part.part_number,
                    download_url: download_url(&part.file_id)?,
                    size_bytes: Some(part.size_bytes),
                    duration_secs: part.duration_secs,
                });
                subtitles.extend(sidecars(&part.file_id, &part.subtitle_tracks));
            }
        }
    }
    Ok(DownloadPlan {
        server_id: server_id.to_owned(),
        title_id: title_id.map(str::to_owned),
        file_id: file_id.to_owned(),
        episode_id: episode_id.map(str::to_owned),
        parts,
        subtitles,
    })
}

/// The subtitle files among `tracks`. An embedded track travels inside the
/// part's own bytes, so it is not an item of its own.
fn sidecars<'a>(
    part_file_id: &'a str,
    tracks: &'a [SubtitleTrackView],
) -> impl Iterator<Item = DownloadSubtitle> + 'a {
    tracks.iter().filter_map(move |track| {
        if track.origin != SubtitleOrigin::Sidecar {
            return None;
        }
        Some(DownloadSubtitle {
            sidecar_id: track.sidecar_id.clone()?,
            part_file_id: part_file_id.to_owned(),
            url: track.url.clone()?,
            codec: track.codec.clone(),
            language: track.language.clone(),
            title: track.title.clone(),
            is_default: track.is_default,
            is_forced: track.is_forced,
            is_hearing_impaired: track.is_hearing_impaired,
        })
    })
}
