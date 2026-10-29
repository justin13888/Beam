//! Choosing a file to play, and explaining every file that was not chosen.
//!
//! Rejected sources are returned alongside the choice rather than filtered
//! away. A source picker that silently hides the 4K remux leaves the user
//! wondering where it went; one that greys it out with "no AV1 decoder on
//! this device" has told them something true and actionable -- and, given
//! Beam's answer to an unplayable file is "index a compatible version", it
//! tells the operator exactly what to do about it.

use super::{AudioCodec, DeviceProfile, VideoCodec};
use crate::error::BeamError;
use crate::servers::ServerRecord;

/// The core's view of one playable file, normalised from the generated type.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MediaSourceView {
    /// Identifier used to stream or download this file.
    pub file_id: String,
    /// Size on disk.
    pub size_bytes: u64,
    /// Duration in seconds, where probing determined one.
    pub duration_secs: Option<f64>,
    /// Container format as probed (`mkv`, `mp4`, ...).
    pub container: Option<String>,
    /// MIME type as recorded by the indexer.
    pub mime_type: Option<String>,
    /// Video codec exactly as probing reported it.
    pub video_codec: Option<String>,
    /// Video width in pixels.
    pub width: Option<u32>,
    /// Video height in pixels.
    pub height: Option<u32>,
    /// Overall bit rate, where probing determined one.
    pub bit_rate: Option<u64>,
    /// HDR format (`HDR10`, `Dolby Vision`, ...) where the stream carries one.
    pub hdr_format: Option<String>,
    /// Every audio track in the file.
    pub audio_tracks: Vec<AudioTrackView>,
    /// Server-relative direct-play URL.
    pub stream_url: String,
    /// Server-relative download URL.
    pub download_url: String,
    /// The files this source plays, in order (issue #233): one for a whole
    /// file, each part of a multi-part movie otherwise.
    ///
    /// A player queues one item per part, and reports progress against the
    /// part that is playing, at the position within that part. Empty only for
    /// a view built without the server's list, which a player treats as the
    /// single part `file_id` names.
    #[uniffi(default)]
    pub parts: Vec<SourcePartView>,
    /// The disc structure this source was copied from whole (issue #234), when
    /// it is a DVD or Blu-ray folder rip's main title.
    #[uniffi(default)]
    pub disc_structure: Option<DiscStructure>,
    /// The first part's subtitle tracks: the file's own, by stream index, then
    /// the subtitle files beside it (issue #189).
    #[uniffi(default)]
    pub subtitle_tracks: Vec<SubtitleTrackView>,
}

impl MediaSourceView {
    /// Whether `file_id` names this source: its first file, or any of its
    /// parts.
    ///
    /// Continue-watching names the part last played, which for a multi-part
    /// movie need not be the first, so a lookup by the first file alone would
    /// lose the viewer's place.
    #[must_use]
    pub fn contains_file(&self, file_id: &str) -> bool {
        self.file_id == file_id || self.parts.iter().any(|part| part.file_id == file_id)
    }

    /// Normalise a generated `MediaSource` into the core's own view, resolving
    /// its relative URLs against the server that served it.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::InvalidServerUrl`] when one of its URLs cannot be
    /// resolved against `record`'s origin.
    pub fn from_generated(
        source: crate::api::types::MediaSource,
        record: &ServerRecord,
    ) -> Result<Self, BeamError> {
        // JSON Schema has no unsigned integer type, so kynos's `uint32`/`uint64`
        // formats reach the generated client as `i64`. Narrowing here rather than
        // widening the core's own types keeps "a size cannot be negative" true in
        // the one place that reasons about sizes; a nonsensical negative is
        // treated as absent rather than wrapping into an enormous positive.
        //
        // The view describes one picture: the source's first video track, which
        // the server lists by stream index. The server lists sources primary
        // first, and this keeps that order.
        let video = source.video_tracks.into_iter().next();
        let parts = source
            .parts
            .into_iter()
            .map(|part| {
                Ok(SourcePartView {
                    file_id: part.file_id.to_string(),
                    part_number: part
                        .part_number
                        .and_then(|number| u32::try_from(number).ok()),
                    size_bytes: u64::try_from(part.size_bytes).unwrap_or(0),
                    duration_secs: part.duration_secs,
                    stream_url: record.absolute_url(&part.stream_url)?,
                    subtitle_tracks: subtitle_views(part.subtitle_tracks, record)?,
                })
            })
            .collect::<Result<Vec<_>, BeamError>>()?;
        Ok(Self {
            file_id: source.file_id.to_string(),
            size_bytes: u64::try_from(source.size_bytes).unwrap_or(0),
            duration_secs: source.duration_secs,
            container: source.container_format,
            mime_type: source.mime_type,
            video_codec: video.as_ref().map(|v| v.codec.clone()),
            width: video.as_ref().and_then(|v| u32::try_from(v.width).ok()),
            height: video.as_ref().and_then(|v| u32::try_from(v.height).ok()),
            bit_rate: video
                .as_ref()
                .and_then(|v| v.bit_rate)
                .and_then(|rate| u64::try_from(rate).ok()),
            hdr_format: video.as_ref().and_then(|v| v.hdr_format.clone()),
            audio_tracks: source
                .audio_tracks
                .into_iter()
                .map(|track| AudioTrackView {
                    codec: track.codec,
                    language: track.language,
                    channels: u16::try_from(track.channels).unwrap_or(0),
                    is_default: track.is_default,
                })
                .collect(),
            stream_url: record.absolute_url(&source.stream_url)?,
            download_url: record.absolute_url(&source.download_url)?,
            parts,
            disc_structure: source.disc_structure.map(|structure| match structure {
                crate::api::types::MediaSourcediscStructure::Dvd => DiscStructure::Dvd,
                crate::api::types::MediaSourcediscStructure::BluRay => DiscStructure::BluRay,
            }),
            subtitle_tracks: subtitle_views(source.subtitle_tracks, record)?,
        })
    }
}

/// Normalise a file's subtitle tracks, resolving a sidecar's URLs against the
/// server that served them.
fn subtitle_views(
    tracks: Vec<crate::api::types::SubtitleTrack>,
    record: &ServerRecord,
) -> Result<Vec<SubtitleTrackView>, BeamError> {
    tracks
        .into_iter()
        .map(|track| {
            let crate::api::types::SubtitleTrack {
                origin,
                index,
                sidecar_id,
                codec,
                language,
                title,
                is_default,
                is_forced,
                is_hearing_impaired,
                is_text,
                url,
                webvtt_url,
            } = track;
            Ok(SubtitleTrackView {
                origin: match origin {
                    crate::api::types::SubtitleOrigin::Embedded => SubtitleOrigin::Embedded,
                    crate::api::types::SubtitleOrigin::Sidecar => SubtitleOrigin::Sidecar,
                },
                index: index.and_then(|value| u32::try_from(value).ok()),
                sidecar_id: sidecar_id.map(|id| id.to_string()),
                codec,
                language,
                title,
                is_default,
                is_forced,
                is_hearing_impaired,
                is_text,
                url: url.map(|path| record.absolute_url(&path)).transpose()?,
                webvtt_url: webvtt_url
                    .map(|path| record.absolute_url(&path))
                    .transpose()?,
            })
        })
        .collect()
}

/// One file of a playback plan: what the platform player queues as one item.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PlaybackPart {
    /// The file, which its progress is reported against.
    pub file_id: String,
    /// Absolute URL to stream it from.
    pub url: String,
    /// Its duration, where probing determined one.
    pub duration_secs: Option<f64>,
    /// The subtitle files beside it (issue #189), each with a URL to fetch it
    /// from. A file's own subtitle streams are not listed: the player's
    /// extractor finds those in the file.
    pub sidecar_subtitles: Vec<SubtitleTrackView>,
}

/// The parts to queue for `file_id`, and which of them to start in.
///
/// From the source that holds `file_id` as its first file or any part. Where
/// there is no such source -- the lookup failed, or the file is not among
/// them -- the plan is the one file alone, at `single_url`: exactly what
/// the player played before a source knew its parts, so nothing that played
/// then stops playing now.
#[must_use]
pub fn plan_parts(
    sources: &[MediaSourceView],
    file_id: &str,
    single_url: String,
) -> (Vec<PlaybackPart>, u32) {
    let sidecars = |tracks: &[SubtitleTrackView]| {
        tracks
            .iter()
            .filter(|track| track.origin == SubtitleOrigin::Sidecar && track.url.is_some())
            .cloned()
            .collect::<Vec<_>>()
    };
    let Some(source) = sources.iter().find(|source| source.contains_file(file_id)) else {
        return (
            vec![PlaybackPart {
                file_id: file_id.to_owned(),
                url: single_url,
                duration_secs: None,
                sidecar_subtitles: Vec::new(),
            }],
            0,
        );
    };
    if source.parts.is_empty() {
        return (
            vec![PlaybackPart {
                file_id: source.file_id.clone(),
                url: source.stream_url.clone(),
                duration_secs: source.duration_secs,
                sidecar_subtitles: sidecars(&source.subtitle_tracks),
            }],
            0,
        );
    }
    let parts: Vec<PlaybackPart> = source
        .parts
        .iter()
        .map(|part| PlaybackPart {
            file_id: part.file_id.clone(),
            url: part.stream_url.clone(),
            duration_secs: part.duration_secs,
            sidecar_subtitles: sidecars(&part.subtitle_tracks),
        })
        .collect();
    let start = parts
        .iter()
        .position(|part| part.file_id == file_id)
        .and_then(|index| u32::try_from(index).ok())
        .unwrap_or(0);
    (parts, start)
}

/// One file of a source, played in the order the source lists it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SourcePartView {
    /// The file this part plays.
    pub file_id: String,
    /// The part the filename names (`CD2` is `2`), from 1; absent for a whole
    /// file.
    pub part_number: Option<u32>,
    /// This part's size on disk.
    pub size_bytes: u64,
    /// This part's duration, where probing determined one.
    pub duration_secs: Option<f64>,
    /// Absolute direct-play URL for this part.
    pub stream_url: String,
    /// This part's own subtitle tracks: a subtitle file beside a part times
    /// that part.
    pub subtitle_tracks: Vec<SubtitleTrackView>,
}

/// The disc structure a source was copied from whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DiscStructure {
    /// A DVD's VOBs: MPEG program streams.
    Dvd,
    /// A Blu-ray's clips: BDAV MPEG transport streams (`.m2ts`).
    BluRay,
}

/// Where a subtitle track comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SubtitleOrigin {
    /// A stream inside the media file, which the player's extractor finds.
    Embedded,
    /// A subtitle file beside the media file, which the player fetches itself.
    Sidecar,
}

/// One subtitle track of a source file.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SubtitleTrackView {
    /// Embedded in the file, or a file beside it.
    pub origin: SubtitleOrigin,
    /// The stream's index in its file; present exactly for an embedded track.
    pub index: Option<u32>,
    /// The subtitle file's identifier; present exactly for a sidecar.
    pub sidecar_id: Option<String>,
    /// FFmpeg's codec name: `subrip`, `webvtt`, `ass`, `mov_text`, or an image
    /// format such as `hdmv_pgs_subtitle`.
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
    /// Text a client can render itself, as opposed to an image format it
    /// must decode.
    pub is_text: bool,
    /// Absolute URL of the track as stored; present exactly for a sidecar.
    pub url: Option<String>,
    /// Absolute URL of the track as WebVTT, for a SubRip or WebVTT sidecar
    /// small enough to convert.
    pub webvtt_url: Option<String>,
}

/// One audio track within a source.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AudioTrackView {
    /// Codec exactly as probing reported it.
    pub codec: String,
    /// ISO 639 language code, where the file declared one.
    pub language: Option<String>,
    /// Channel count.
    pub channels: u16,
    /// Whether the file marks this track as default.
    pub is_default: bool,
}

/// Why a source cannot be played on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RejectionReason {
    /// The player cannot demux the container.
    ContainerUnsupported,
    /// No decoder for the video codec.
    VideoCodecUnsupported,
    /// No decoder for any of the audio tracks.
    AudioCodecUnsupported,
    /// A decoder exists but not at this resolution.
    ResolutionExceedsDecoder,
    /// A decoder exists but not at this bit rate.
    BitrateExceedsDecoder,
    /// The stream is HDR and nothing here can present it.
    HdrUnsupported,
    /// Playable, but excluded by the caller's quality policy.
    ExcludedByPolicy,
    /// The source carries no video stream.
    NoVideoStream,
    /// Only a software decoder matched, and software decoding is disabled.
    SoftwareDecodeDisabled,
    /// A Blu-ray folder rip, whose BDAV transport streams (`.m2ts`, a 4-byte
    /// timestamp before every packet) no native player here is confirmed to
    /// demux.
    DiscStructureUnsupported,
}

impl RejectionReason {
    /// The playback-telemetry reason a rejection is counted under, or `None`
    /// for one that is not a failure at all.
    ///
    /// The telemetry vocabulary is coarser on purpose: it answers "which part
    /// of the file could this device not play", and every decoder-capability
    /// limit -- size, bit rate, HDR, a software-only match, a missing video
    /// stream -- is the video stream not playing. A policy exclusion is the
    /// viewer's or the caller's choice, not the device failing, so counting it
    /// would make a quality preference look like an incompatibility.
    ///
    /// Exhaustive with no wildcard arm, so a new reason cannot be added
    /// without deciding where it is counted.
    #[must_use]
    pub const fn failure_reason(self) -> Option<crate::telemetry::FailureReason> {
        use crate::telemetry::FailureReason;
        match self {
            Self::ContainerUnsupported | Self::DiscStructureUnsupported => {
                Some(FailureReason::Container)
            }
            Self::VideoCodecUnsupported
            | Self::ResolutionExceedsDecoder
            | Self::BitrateExceedsDecoder
            | Self::HdrUnsupported
            | Self::SoftwareDecodeDisabled
            | Self::NoVideoStream => Some(FailureReason::VideoCodec),
            Self::AudioCodecUnsupported => Some(FailureReason::AudioCodec),
            Self::ExcludedByPolicy => None,
        }
    }
}

/// A source that was considered and set aside, with the reason.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct RejectedSource {
    /// Which file.
    pub file_id: String,
    /// Why, as a machine-readable reason.
    pub reason: RejectionReason,
    /// Why, phrased for a person.
    pub detail: String,
}

/// Whether, and how well, a source plays here.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum Playability {
    /// Every stream has a hardware decoder.
    Hardware,
    /// Playable, but at least one stream falls to a software decoder. Worth
    /// surfacing: a 4K HEVC stream in software will stutter on most phones.
    Software {
        /// Which stream is the problem, phrased for a person.
        detail: String,
    },
    /// Cannot be played here.
    Unsupported {
        /// The machine-readable reason.
        reason: RejectionReason,
        /// The reason, phrased for a person.
        detail: String,
    },
}

/// How to choose among the playable sources.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum QualityPolicy {
    /// The highest-quality source this device can play (FR-704).
    Highest,
    /// The highest-quality source that does not exceed the display. On a
    /// 1080p phone this avoids spending 4K of bandwidth on pixels the panel
    /// cannot show, without ever picking something unplayable.
    MatchScreen,
    /// The smallest playable source, for a constrained connection.
    Smallest,
    /// A specific file, chosen by the user in the source picker.
    Specific {
        /// The file the user picked.
        file_id: String,
    },
}

/// The outcome of a successful selection.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SourceSelection {
    /// The chosen source.
    pub source: MediaSourceView,
    /// How well it will play.
    pub playability: Playability,
    /// Index into the chosen source's `audio_tracks`, where one was picked.
    pub audio_track_index: Option<u32>,
    /// Why this source won, phrased for a person.
    pub reason: String,
    /// Every source not chosen, each with its reason.
    pub rejected: Vec<RejectedSource>,
}

/// Judge one source against a device, in isolation.
#[must_use]
pub fn playability(source: &MediaSourceView, profile: &DeviceProfile) -> Playability {
    // Before the container check, because it would pass: a Blu-ray's clips
    // probe as `mpegts`, which ExoPlayer lists, but they are BDAV streams --
    // 192-byte packets, a timestamp before each -- and ExoPlayer's TS
    // extractor reads 188-byte packets only. Until playback of one is
    // confirmed on a device, saying so beats a decoder error mid-start. A DVD
    // is left to the ordinary checks: its VOBs are plain MPEG program streams,
    // which a device either demuxes and decodes or does not.
    if source.disc_structure == Some(DiscStructure::BluRay) {
        return Playability::Unsupported {
            reason: RejectionReason::DiscStructureUnsupported,
            detail: "Blu-ray disc folders cannot be played on this device".to_owned(),
        };
    }

    let container = source.container.as_deref().unwrap_or_default();
    if !profile.supports_container(container) {
        return Playability::Unsupported {
            reason: RejectionReason::ContainerUnsupported,
            detail: format!("This device cannot open {} files", container.to_uppercase()),
        };
    }

    let Some(raw_codec) = source.video_codec.as_deref() else {
        return Playability::Unsupported {
            reason: RejectionReason::NoVideoStream,
            detail: "This file has no video stream".to_owned(),
        };
    };

    let codec = VideoCodec::from_probe(raw_codec);
    let Some(decoder) = profile.video_decoder_for(codec) else {
        return Playability::Unsupported {
            reason: RejectionReason::VideoCodecUnsupported,
            detail: format!("No {} decoder on this device", raw_codec.to_uppercase()),
        };
    };

    let (width, height) = (source.width.unwrap_or(0), source.height.unwrap_or(0));
    if !decoder.accepts_size(width, height) {
        return Playability::Unsupported {
            reason: RejectionReason::ResolutionExceedsDecoder,
            detail: format!(
                "This device's {} decoder does not reach {width}x{height}",
                raw_codec.to_uppercase()
            ),
        };
    }

    if let Some(bit_rate) = source.bit_rate
        && !decoder.accepts_bitrate(bit_rate)
    {
        return Playability::Unsupported {
            reason: RejectionReason::BitrateExceedsDecoder,
            detail: format!(
                "This device's {} decoder does not reach {} Mbps",
                raw_codec.to_uppercase(),
                bit_rate / 1_000_000
            ),
        };
    }

    // HDR is only a hard failure when the decoder cannot handle the bit depth.
    // An HDR stream on an SDR panel tone-maps rather than failing, so it is
    // not grounds for rejection -- only for preferring an SDR alternative.
    if let Some(hdr) = source.hdr_format.as_deref() {
        let handled = if hdr.to_ascii_lowercase().contains("dolby") {
            decoder.supports_dolby_vision
        } else {
            decoder.supports_hdr10 || decoder.supports_10_bit
        };
        if !handled {
            return Playability::Unsupported {
                reason: RejectionReason::HdrUnsupported,
                detail: format!("This device cannot decode {hdr}"),
            };
        }
    }

    if !source.audio_tracks.is_empty() {
        let any_audio = source.audio_tracks.iter().any(|track| {
            profile
                .audio_decoder_for(AudioCodec::from_probe(&track.codec))
                .is_some()
        });
        if !any_audio {
            let names: Vec<_> = source
                .audio_tracks
                .iter()
                .map(|t| t.codec.to_uppercase())
                .collect();
            return Playability::Unsupported {
                reason: RejectionReason::AudioCodecUnsupported,
                detail: format!("No decoder for this file's audio ({})", names.join(", ")),
            };
        }
    }

    if !decoder.is_hardware_accelerated {
        if !profile.allow_software_decode {
            return Playability::Unsupported {
                reason: RejectionReason::SoftwareDecodeDisabled,
                detail: format!(
                    "{} on this device is software-only, which is turned off",
                    raw_codec.to_uppercase()
                ),
            };
        }
        return Playability::Software {
            detail: format!(
                "{} decodes in software here and may stutter",
                raw_codec.to_uppercase()
            ),
        };
    }

    Playability::Hardware
}

/// Choose a source, or explain why none can be played.
///
/// On failure the caller receives every rejection, which is what lets the UI
/// say "your library has this title, but not in a form this device can play"
/// rather than an unqualified error.
///
/// # Errors
///
/// Returns every [`RejectedSource`] when nothing satisfies the device and
/// policy, including when `sources` is empty.
pub fn select_source(
    sources: &[MediaSourceView],
    profile: &DeviceProfile,
    policy: &QualityPolicy,
) -> Result<SourceSelection, Vec<RejectedSource>> {
    let mut playable: Vec<(&MediaSourceView, Playability)> = Vec::new();
    let mut rejected: Vec<RejectedSource> = Vec::new();

    for source in sources {
        match playability(source, profile) {
            Playability::Unsupported { reason, detail } => rejected.push(RejectedSource {
                file_id: source.file_id.clone(),
                reason,
                detail,
            }),
            verdict => playable.push((source, verdict)),
        }
    }

    // A specific pick is the user's decision in the source picker; honour it
    // whenever it is playable at all, without applying any ranking.
    if let QualityPolicy::Specific { file_id } = policy {
        return match playable.iter().find(|(s, _)| &s.file_id == file_id) {
            Some((source, verdict)) => Ok(finish(
                source,
                verdict.clone(),
                profile,
                "You chose this version",
                rejected,
            )),
            None => {
                if !rejected.iter().any(|r| &r.file_id == file_id) {
                    rejected.push(RejectedSource {
                        file_id: file_id.clone(),
                        reason: RejectionReason::ExcludedByPolicy,
                        detail: "That version is no longer available".to_owned(),
                    });
                }
                Err(rejected)
            }
        };
    }

    if playable.is_empty() {
        return Err(rejected);
    }

    // Hardware before software, so a source that merely *can* play never
    // beats one that plays well.
    let hardware_only: Vec<_> = playable
        .iter()
        .filter(|(_, verdict)| matches!(verdict, Playability::Hardware))
        .cloned()
        .collect();
    let pool = if hardware_only.is_empty() {
        playable.clone()
    } else {
        hardware_only
    };

    let (chosen, verdict, reason) = match policy {
        QualityPolicy::Specific { .. } => unreachable!("handled above"),
        QualityPolicy::Highest => {
            let best = pool
                .iter()
                .max_by(|a, b| {
                    quality_key(a.0)
                        .partial_cmp(&quality_key(b.0))
                        .expect("total")
                })
                .expect("pool is non-empty");
            (
                best.0,
                best.1.clone(),
                "Highest quality this device can play",
            )
        }
        QualityPolicy::Smallest => {
            let best = pool
                .iter()
                .min_by_key(|(source, _)| source.size_bytes)
                .expect("pool is non-empty");
            (best.0, best.1.clone(), "Smallest version, to save data")
        }
        QualityPolicy::MatchScreen => {
            let cap = profile.display_height;
            // Prefer the largest source that fits the panel; if every source
            // is bigger than the panel, take the smallest of them rather than
            // failing -- the alternative is refusing to play a title the
            // device can decode perfectly well.
            let within: Vec<_> = pool
                .iter()
                .filter(|(source, _)| source.height.unwrap_or(0) <= cap)
                .cloned()
                .collect();
            if within.is_empty() {
                let best = pool
                    .iter()
                    .min_by(|a, b| {
                        quality_key(a.0)
                            .partial_cmp(&quality_key(b.0))
                            .expect("total")
                    })
                    .expect("pool is non-empty");
                (
                    best.0,
                    best.1.clone(),
                    "Lowest version above this screen's resolution",
                )
            } else {
                let best = within
                    .iter()
                    .max_by(|a, b| {
                        quality_key(a.0)
                            .partial_cmp(&quality_key(b.0))
                            .expect("total")
                    })
                    .expect("non-empty");
                (best.0, best.1.clone(), "Best match for this screen")
            }
        }
    };

    // Everything playable but not chosen is still reported, so the picker can
    // list it as an available alternative rather than dropping it.
    for (source, _) in &playable {
        if source.file_id != chosen.file_id {
            rejected.push(RejectedSource {
                file_id: source.file_id.clone(),
                reason: RejectionReason::ExcludedByPolicy,
                detail: "Available, but not the best match for this device".to_owned(),
            });
        }
    }

    Ok(finish(chosen, verdict, profile, reason, rejected))
}

/// Choose a smaller file to fall back to after sustained rebuffering.
///
/// The candidates are every source that demands less of the connection than
/// the one playing: by bit rate where both sides declare one, and by size
/// otherwise. They are then ranked by [`select_source`] with
/// [`QualityPolicy::MatchScreen`], so the result runs through
/// [`playability`] like any other choice -- a smaller file this device cannot
/// decode is never a fallback -- and is the *best* of the smaller files that
/// fits the screen rather than the smallest outright. A step down costs the
/// viewer as little picture as it can; if the connection still cannot keep
/// up, the next sustained rebuffer steps down again.
///
/// The selection's `rejected` list covers only the smaller candidates, and its
/// `reason` says why playback switched, phrased for the viewer.
///
/// Returns `None` when `current_file_id` is not among `sources`, or when no
/// smaller source plays here: the honest outcome then is to keep stalling on
/// the file that does play, not to switch to one that does not.
#[must_use]
pub fn select_downgrade(
    sources: &[MediaSourceView],
    current_file_id: &str,
    profile: &DeviceProfile,
) -> Option<SourceSelection> {
    // Any part: a multi-part movie that stalls in its second part is playing
    // the second part's file, not the one the source is named by.
    let current = sources
        .iter()
        .find(|source| source.contains_file(current_file_id))?;
    let cheaper: Vec<MediaSourceView> = sources
        .iter()
        .filter(|candidate| demands_less_than(candidate, current))
        .cloned()
        .collect();

    let mut selection = select_source(&cheaper, profile, &QualityPolicy::MatchScreen).ok()?;
    selection.reason = match selection.source.height {
        Some(height) if height > 0 => {
            format!("Switched to {height}p because playback kept stalling")
        }
        _ => "Switched to a smaller version because playback kept stalling".to_owned(),
    };
    Some(selection)
}

/// Whether `candidate` needs less bandwidth than `current`.
///
/// Bit rate is the measure that matters, and it is compared only when both
/// files declare one; a file missing it is judged by size instead, which is
/// the same measure for two versions of one title of about the same length.
fn demands_less_than(candidate: &MediaSourceView, current: &MediaSourceView) -> bool {
    match (candidate.bit_rate, current.bit_rate) {
        (Some(candidate_rate), Some(current_rate)) => candidate_rate < current_rate,
        _ => candidate.size_bytes < current.size_bytes,
    }
}

/// Ordering key for "quality": pixels, then bit rate, then size.
///
/// Returned as a tuple of floats so the comparison is total over the `f64`
/// bit rate without needing a custom `Ord`.
fn quality_key(source: &MediaSourceView) -> (u64, u64, u64) {
    let pixels = u64::from(source.width.unwrap_or(0)) * u64::from(source.height.unwrap_or(0));
    (pixels, source.bit_rate.unwrap_or(0), source.size_bytes)
}

fn finish(
    source: &MediaSourceView,
    playability: Playability,
    profile: &DeviceProfile,
    reason: &str,
    rejected: Vec<RejectedSource>,
) -> SourceSelection {
    SourceSelection {
        source: source.clone(),
        playability,
        audio_track_index: choose_audio_track(source, profile),
        reason: reason.to_owned(),
        rejected,
    }
}

/// Pick an audio track: preferred language first, then the file's default,
/// then the most channels, and only ever one the device can decode.
fn choose_audio_track(source: &MediaSourceView, profile: &DeviceProfile) -> Option<u32> {
    let decodable: Vec<(usize, &AudioTrackView)> = source
        .audio_tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| {
            profile
                .audio_decoder_for(AudioCodec::from_probe(&track.codec))
                .is_some()
        })
        .collect();

    let best = decodable.iter().max_by_key(|(index, track)| {
        let language_rank = track
            .language
            .as_deref()
            .and_then(|language| {
                profile
                    .preferred_audio_languages
                    .iter()
                    .position(|preferred| preferred.eq_ignore_ascii_case(language))
            })
            // Unlisted languages rank below every listed one; negating the
            // position makes "earlier in the list" larger.
            .map_or(0_i64, |position| {
                i64::try_from(profile.preferred_audio_languages.len() - position).unwrap_or(0)
            });
        // `index` is negated so an earlier track wins a tie, matching the
        // order the file itself declares.
        (
            language_rank,
            track.is_default,
            track.channels,
            -i64::try_from(*index).unwrap_or(0),
        )
    })?;

    u32::try_from(best.0).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::builders::{DeviceProfileBuilder, MediaSourceBuilder};

    fn uhd_hevc() -> MediaSourceView {
        MediaSourceBuilder::new("uhd")
            .video("hevc", 3840, 2160)
            .bitrate(60_000_000)
            .size(40_000_000_000)
            .container("mkv")
            .build()
    }

    fn hd_h264() -> MediaSourceView {
        MediaSourceBuilder::new("hd")
            .video("h264", 1920, 1080)
            .bitrate(8_000_000)
            .size(4_000_000_000)
            .build()
    }

    fn sd_h264() -> MediaSourceView {
        MediaSourceBuilder::new("sd")
            .video("h264", 854, 480)
            .bitrate(1_500_000)
            .size(700_000_000)
            .build()
    }

    #[test]
    fn highest_picks_the_best_source_the_device_can_actually_decode() {
        let sources = [uhd_hevc(), hd_h264(), sd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_source(&sources, &profile, &QualityPolicy::Highest).expect("a source");
        assert_eq!(chosen.source.file_id, "uhd");
        assert_eq!(chosen.playability, Playability::Hardware);
    }

    #[test]
    fn a_device_without_the_codec_falls_to_the_next_best_source() {
        // This is the case that justifies the whole module: the 4K remux is
        // present and the user can see it, but this device has no HEVC
        // decoder, so playback silently uses the H.264 rip instead of failing.
        let sources = [uhd_hevc(), hd_h264(), sd_h264()];
        let profile = DeviceProfileBuilder::budget_h264_only().build();

        let chosen = select_source(&sources, &profile, &QualityPolicy::Highest).expect("a source");
        assert_eq!(chosen.source.file_id, "hd");

        let uhd = chosen
            .rejected
            .iter()
            .find(|r| r.file_id == "uhd")
            .expect("the 4K source is reported, not hidden");
        assert_eq!(uhd.reason, RejectionReason::VideoCodecUnsupported);
        assert!(
            uhd.detail.contains("HEVC"),
            "detail names the codec: {}",
            uhd.detail
        );
    }

    #[test]
    fn every_playable_source_is_still_reported_so_the_picker_can_list_it() {
        let sources = [uhd_hevc(), hd_h264(), sd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_source(&sources, &profile, &QualityPolicy::Highest).expect("a source");
        let reported: Vec<_> = chosen.rejected.iter().map(|r| r.file_id.as_str()).collect();
        assert!(reported.contains(&"hd"));
        assert!(reported.contains(&"sd"));
    }

    #[test]
    fn match_screen_does_not_spend_4k_of_bandwidth_on_a_1080p_panel() {
        let sources = [uhd_hevc(), hd_h264(), sd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc()
            .display(1920, 1080)
            .build();

        let chosen =
            select_source(&sources, &profile, &QualityPolicy::MatchScreen).expect("a source");
        assert_eq!(chosen.source.file_id, "hd");
    }

    #[test]
    fn match_screen_still_plays_when_every_source_exceeds_the_panel() {
        // Refusing to play a title the device decodes perfectly well, purely
        // because the panel is small, would be a worse outcome than scaling.
        let sources = [uhd_hevc()];
        let profile = DeviceProfileBuilder::phone_h264_hevc()
            .display(1280, 720)
            .build();

        let chosen =
            select_source(&sources, &profile, &QualityPolicy::MatchScreen).expect("a source");
        assert_eq!(chosen.source.file_id, "uhd");
    }

    #[test]
    fn smallest_picks_the_least_data() {
        let sources = [uhd_hevc(), hd_h264(), sd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_source(&sources, &profile, &QualityPolicy::Smallest).expect("a source");
        assert_eq!(chosen.source.file_id, "sd");
    }

    #[test]
    fn a_specific_pick_overrides_ranking_entirely() {
        let sources = [uhd_hevc(), hd_h264(), sd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_source(
            &sources,
            &profile,
            &QualityPolicy::Specific {
                file_id: "sd".to_owned(),
            },
        )
        .expect("a source");
        assert_eq!(chosen.source.file_id, "sd");
    }

    #[test]
    fn a_specific_pick_that_cannot_play_is_an_error_not_a_substitution() {
        // Silently substituting would make the source picker lie about what
        // is playing.
        let sources = [uhd_hevc(), hd_h264()];
        let profile = DeviceProfileBuilder::budget_h264_only().build();

        let rejections = select_source(
            &sources,
            &profile,
            &QualityPolicy::Specific {
                file_id: "uhd".to_owned(),
            },
        )
        .expect_err("must not fall back");
        assert!(rejections.iter().any(|r| r.file_id == "uhd"));
    }

    #[test]
    fn hardware_beats_software_even_at_lower_quality() {
        let sources = [uhd_hevc(), hd_h264()];
        let profile = DeviceProfileBuilder::new()
            .software_video("video/hevc", 3840, 2160)
            .hardware_video("video/avc", 1920, 1080)
            .hardware_audio("audio/mp4a-latm")
            .allow_software()
            .build();

        let chosen = select_source(&sources, &profile, &QualityPolicy::Highest).expect("a source");
        assert_eq!(
            chosen.source.file_id, "hd",
            "a stuttering 4K software decode is not a better default than smooth 1080p"
        );
    }

    #[test]
    fn software_only_playback_is_flagged_rather_than_hidden() {
        let sources = [uhd_hevc()];
        let profile = DeviceProfileBuilder::new()
            .software_video("video/hevc", 3840, 2160)
            .hardware_audio("audio/mp4a-latm")
            .allow_software()
            .build();

        let chosen = select_source(&sources, &profile, &QualityPolicy::Highest).expect("a source");
        assert!(matches!(chosen.playability, Playability::Software { .. }));
    }

    #[test]
    fn software_decode_is_refused_when_the_user_turned_it_off() {
        let sources = [uhd_hevc()];
        let profile = DeviceProfileBuilder::new()
            .software_video("video/hevc", 3840, 2160)
            .hardware_audio("audio/mp4a-latm")
            .build();

        let rejections = select_source(&sources, &profile, &QualityPolicy::Highest)
            .expect_err("software decode is disabled");
        assert_eq!(
            rejections[0].reason,
            RejectionReason::SoftwareDecodeDisabled
        );
    }

    #[test]
    fn an_undemuxable_container_is_rejected_before_any_codec_is_considered() {
        let source = MediaSourceBuilder::new("avi").container("avi").build();
        let profile = DeviceProfileBuilder::phone_h264_hevc()
            .containers(&["mp4", "mkv"])
            .build();

        let rejections = select_source(&[source], &profile, &QualityPolicy::Highest)
            .expect_err("cannot demux avi");
        assert_eq!(rejections[0].reason, RejectionReason::ContainerUnsupported);
    }

    #[test]
    fn a_blu_ray_folder_is_rejected_even_where_its_container_would_pass() {
        // The clips probe as `mpegts`, which the profile lists, and as H.264,
        // which it decodes: every ordinary check passes, and ExoPlayer would
        // still fail on the 192-byte BDAV packets. The alternative edition
        // plays instead.
        let blu_ray = MediaSourceBuilder::new("bd")
            .container("mpegts")
            .video("h264", 1920, 1080)
            .disc(DiscStructure::BluRay)
            .build();
        let remux = MediaSourceBuilder::new("mkv").container("mkv").build();
        let profile = DeviceProfileBuilder::phone_h264_hevc()
            .containers(&["mkv", "mpegts"])
            .build();

        let chosen = select_source(&[blu_ray, remux], &profile, &QualityPolicy::Highest)
            .expect("the remux plays");
        assert_eq!(chosen.source.file_id, "mkv");
        let rejected = chosen
            .rejected
            .iter()
            .find(|r| r.file_id == "bd")
            .expect("the Blu-ray is listed, not hidden");
        assert_eq!(rejected.reason, RejectionReason::DiscStructureUnsupported);
    }

    #[test]
    fn a_dvd_folder_is_judged_by_the_ordinary_checks() {
        // A DVD's VOBs are plain MPEG program streams: a device that demuxes
        // and decodes them plays them, so the disc structure alone is no
        // reason to refuse one.
        let dvd = MediaSourceBuilder::new("dvd")
            .container("mpeg")
            .video("h264", 720, 480)
            .disc(DiscStructure::Dvd)
            .build();
        let can_demux = DeviceProfileBuilder::phone_h264_hevc()
            .containers(&["mpeg"])
            .build();
        let cannot_demux = DeviceProfileBuilder::phone_h264_hevc()
            .containers(&["mkv"])
            .build();

        let chosen = select_source(
            std::slice::from_ref(&dvd),
            &can_demux,
            &QualityPolicy::Highest,
        )
        .expect("a demuxable DVD plays");
        assert_eq!(chosen.source.file_id, "dvd");
        let rejections = select_source(&[dvd], &cannot_demux, &QualityPolicy::Highest)
            .expect_err("an undemuxable DVD does not");
        assert_eq!(rejections[0].reason, RejectionReason::ContainerUnsupported);
    }

    #[test]
    fn a_source_contains_every_one_of_its_parts_and_nothing_else() {
        let multi_part = MediaSourceBuilder::new("cd1")
            .parts(&[("cd1", 3000.0), ("cd2", 2800.0)])
            .build();
        assert!(multi_part.contains_file("cd1"));
        assert!(multi_part.contains_file("cd2"));
        assert!(!multi_part.contains_file("cd3"));

        // A view built without the server's parts still names its own file.
        let whole = MediaSourceBuilder::new("whole").build();
        assert!(whole.contains_file("whole"));
        assert!(!whole.contains_file("cd1"));
    }

    #[test]
    fn a_resolution_above_the_decoder_ceiling_is_rejected() {
        let sources = [uhd_hevc()];
        let profile = DeviceProfileBuilder::new()
            .hardware_video("video/hevc", 1920, 1080)
            .hardware_audio("audio/mp4a-latm")
            .build();

        let rejections =
            select_source(&sources, &profile, &QualityPolicy::Highest).expect_err("too large");
        assert_eq!(
            rejections[0].reason,
            RejectionReason::ResolutionExceedsDecoder
        );
    }

    #[test]
    fn a_file_whose_only_audio_is_undecodable_is_rejected() {
        // A DTS-HD track on a device with only AAC: the video would play in
        // silence, which is not "playable".
        let source = MediaSourceBuilder::new("dts")
            .audio(&[("dts", Some("eng"), 6, true)])
            .build();
        let profile = DeviceProfileBuilder::budget_h264_only().build();

        let rejections = select_source(&[source], &profile, &QualityPolicy::Highest)
            .expect_err("no decodable audio");
        assert_eq!(rejections[0].reason, RejectionReason::AudioCodecUnsupported);
    }

    #[test]
    fn a_file_with_one_decodable_track_among_several_still_plays() {
        let source = MediaSourceBuilder::new("mixed")
            .audio(&[
                ("truehd", Some("eng"), 8, true),
                ("aac", Some("eng"), 2, false),
            ])
            .build();
        let profile = DeviceProfileBuilder::budget_h264_only().build();

        let chosen =
            select_source(&[source], &profile, &QualityPolicy::Highest).expect("aac is decodable");
        assert_eq!(
            chosen.audio_track_index,
            Some(1),
            "the undecodable TrueHD track must not be selected"
        );
    }

    #[test]
    fn the_preferred_language_wins_over_the_files_default_track() {
        let source = MediaSourceBuilder::new("dual")
            .audio(&[
                ("aac", Some("jpn"), 2, true),
                ("aac", Some("eng"), 2, false),
            ])
            .build();
        let profile = DeviceProfileBuilder::budget_h264_only()
            .preferred_languages(&["eng"])
            .build();

        let chosen = select_source(&[source], &profile, &QualityPolicy::Highest).expect("a source");
        assert_eq!(chosen.audio_track_index, Some(1));
    }

    #[test]
    fn the_files_default_track_wins_when_no_language_is_preferred() {
        let source = MediaSourceBuilder::new("dual")
            .audio(&[
                ("aac", Some("jpn"), 2, false),
                ("aac", Some("eng"), 2, true),
            ])
            .build();
        let profile = DeviceProfileBuilder::budget_h264_only().build();

        let chosen = select_source(&[source], &profile, &QualityPolicy::Highest).expect("a source");
        assert_eq!(chosen.audio_track_index, Some(1));
    }

    #[test]
    fn hdr_is_rejected_only_when_the_decoder_cannot_carry_the_bit_depth() {
        let hdr = MediaSourceBuilder::new("hdr")
            .video("hevc", 3840, 2160)
            .hdr("HDR10")
            .build();

        // 8-bit-only HEVC decoder: genuinely cannot decode the stream.
        let sdr_only = DeviceProfileBuilder::phone_h264_hevc().build();
        let rejections = select_source(
            std::slice::from_ref(&hdr),
            &sdr_only,
            &QualityPolicy::Highest,
        )
        .expect_err("8-bit decoder cannot carry HDR10");
        assert_eq!(rejections[0].reason, RejectionReason::HdrUnsupported);

        // 10-bit decoder on an SDR panel: decodes and tone-maps, so it plays.
        let tone_mapping = DeviceProfileBuilder::flagship_av1_hdr()
            .display(1920, 1080)
            .build();
        assert!(select_source(&[hdr], &tone_mapping, &QualityPolicy::Highest).is_ok());
    }

    #[test]
    fn a_source_with_no_video_stream_is_rejected() {
        let source = MediaSourceBuilder::new("audio-only")
            .without_video()
            .build();
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let rejections = select_source(&[source], &profile, &QualityPolicy::Highest)
            .expect_err("no video stream");
        assert_eq!(rejections[0].reason, RejectionReason::NoVideoStream);
    }

    #[test]
    fn no_sources_at_all_is_an_empty_rejection_list_not_a_panic() {
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();
        let rejections =
            select_source(&[], &profile, &QualityPolicy::Highest).expect_err("nothing to play");
        assert!(rejections.is_empty());
    }

    fn hd_h264_small() -> MediaSourceView {
        MediaSourceBuilder::new("hd-small")
            .video("h264", 1920, 1080)
            .bitrate(4_000_000)
            .size(2_000_000_000)
            .build()
    }

    fn hd_av1() -> MediaSourceView {
        MediaSourceBuilder::new("hd-av1")
            .video("av1", 1920, 1080)
            .bitrate(3_000_000)
            .size(1_500_000_000)
            .build()
    }

    #[test]
    fn a_downgrade_steps_to_the_best_smaller_source_not_the_smallest() {
        // From 4K, the 1080p file is the step that costs the least picture;
        // jumping straight to 480p would punish a connection that only
        // narrowly failed to carry the remux.
        let sources = [uhd_hevc(), hd_h264(), sd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_downgrade(&sources, "uhd", &profile).expect("a smaller source");
        assert_eq!(chosen.source.file_id, "hd");
        assert_eq!(
            chosen.reason,
            "Switched to 1080p because playback kept stalling"
        );
    }

    #[test]
    fn a_downgrade_from_a_later_part_finds_the_source_it_belongs_to() {
        let multi_part_uhd = MediaSourceBuilder::new("uhd-cd1")
            .video("hevc", 3840, 2160)
            .bitrate(60_000_000)
            .size(40_000_000_000)
            .parts(&[("uhd-cd1", 3600.0), ("uhd-cd2", 3600.0)])
            .build();
        let sources = [multi_part_uhd, hd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_downgrade(&sources, "uhd-cd2", &profile).expect("a smaller source");
        assert_eq!(chosen.source.file_id, "hd");
    }

    #[test]
    fn a_second_downgrade_steps_down_again() {
        let sources = [uhd_hevc(), hd_h264(), sd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_downgrade(&sources, "hd", &profile).expect("a smaller source");
        assert_eq!(chosen.source.file_id, "sd");
    }

    #[test]
    fn the_smallest_source_has_nowhere_to_go() {
        let sources = [uhd_hevc(), hd_h264(), sd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        assert_eq!(select_downgrade(&sources, "sd", &profile), None);
    }

    #[test]
    fn a_smaller_source_this_device_cannot_decode_is_never_a_fallback() {
        // The AV1 file is the smaller one, but this phone has no AV1 decoder:
        // switching to it would turn a stall into a failure.
        let sources = [hd_h264(), hd_av1()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        assert_eq!(select_downgrade(&sources, "hd", &profile), None);
    }

    #[test]
    fn a_downgrade_skips_an_undecodable_rung_for_a_decodable_one() {
        let sources = [hd_h264(), hd_av1(), sd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_downgrade(&sources, "hd", &profile).expect("the SD file plays");
        assert_eq!(chosen.source.file_id, "sd");
        assert!(
            chosen.rejected.iter().any(
                |r| r.file_id == "hd-av1" && r.reason == RejectionReason::VideoCodecUnsupported
            ),
            "the skipped file is reported with its reason: {:?}",
            chosen.rejected
        );
    }

    #[test]
    fn bit_rate_decides_which_is_smaller_when_both_declare_one() {
        // The extended cut is the bigger file but the lighter stream; size
        // alone would call it "larger" and miss the fallback.
        let long_but_light = MediaSourceBuilder::new("extended")
            .video("h264", 1280, 720)
            .bitrate(2_000_000)
            .size(9_000_000_000)
            .build();
        let sources = [hd_h264(), long_but_light];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_downgrade(&sources, "hd", &profile).expect("a lighter stream");
        assert_eq!(chosen.source.file_id, "extended");
    }

    #[test]
    fn size_decides_when_a_bit_rate_is_missing() {
        let mut unprobed = MediaSourceBuilder::new("unprobed")
            .video("h264", 1280, 720)
            .size(1_000_000_000)
            .build();
        unprobed.bit_rate = None;
        let sources = [hd_h264(), unprobed];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_downgrade(&sources, "hd", &profile).expect("a smaller file");
        assert_eq!(chosen.source.file_id, "unprobed");
    }

    #[test]
    fn an_equally_heavy_source_is_not_a_downgrade() {
        let twin = MediaSourceBuilder::new("twin")
            .video("h264", 1280, 720)
            .bitrate(8_000_000)
            .size(1_000_000_000)
            .build();
        let sources = [hd_h264(), twin];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        assert_eq!(select_downgrade(&sources, "hd", &profile), None);
    }

    #[test]
    fn a_file_that_is_not_among_the_sources_has_no_downgrade() {
        // A later part of a multi-part movie, say, which is not itself a
        // source: nothing here says what it is smaller than.
        let sources = [uhd_hevc(), hd_h264()];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        assert_eq!(select_downgrade(&sources, "elsewhere", &profile), None);
    }

    #[test]
    fn a_downgrade_without_a_known_height_still_says_why() {
        let unknown = MediaSourceBuilder::new("unknown")
            .video("h264", 0, 0)
            .bitrate(1_000_000)
            .size(500_000_000)
            .build();
        let sources = [hd_h264(), unknown];
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();

        let chosen = select_downgrade(&sources, "hd", &profile).expect("a smaller file");
        assert_eq!(
            chosen.reason,
            "Switched to a smaller version because playback kept stalling"
        );
    }

    #[test]
    fn a_downgrade_prefers_a_hardware_decode_among_the_smaller_files() {
        let sources = [uhd_hevc(), hd_h264_small(), hd_av1()];
        let profile = DeviceProfileBuilder::phone_h264_hevc()
            .software_video("video/av01", 1920, 1080)
            .allow_software()
            .build();

        let chosen = select_downgrade(&sources, "uhd", &profile).expect("a smaller file");
        assert_eq!(chosen.source.file_id, "hd-small");
        assert_eq!(chosen.playability, Playability::Hardware);
    }

    #[test]
    fn ranking_does_not_depend_on_the_order_sources_arrive_in() {
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();
        let forward = [uhd_hevc(), hd_h264(), sd_h264()];
        let reversed = [sd_h264(), hd_h264(), uhd_hevc()];

        let a = select_source(&forward, &profile, &QualityPolicy::Highest).expect("a source");
        let b = select_source(&reversed, &profile, &QualityPolicy::Highest).expect("a source");
        assert_eq!(a.source.file_id, b.source.file_id);
    }

    /// A plan never lists a file's own streams as files to fetch: only a
    /// sidecar with a URL is something the player loads beside the media.
    #[test]
    fn only_fetchable_sidecars_reach_the_plan() {
        let track = |origin, url: Option<&str>| SubtitleTrackView {
            origin,
            index: None,
            sidecar_id: None,
            codec: "subrip".to_owned(),
            language: None,
            title: None,
            is_default: false,
            is_forced: false,
            is_hearing_impaired: false,
            is_text: true,
            url: url.map(str::to_owned),
            webvtt_url: None,
        };
        let mut source = MediaSourceBuilder::new("whole").build();
        source.subtitle_tracks = vec![
            track(SubtitleOrigin::Embedded, None),
            track(SubtitleOrigin::Sidecar, Some("https://beam.test/s.srt")),
            track(SubtitleOrigin::Sidecar, None),
        ];

        let (parts, start) = plan_parts(&[source], "whole", "unused".to_owned());

        assert_eq!(start, 0);
        assert_eq!(parts.len(), 1, "a whole file is a plan of one part");
        assert_eq!(parts[0].url, "/v1/files/whole/stream");
        let urls: Vec<Option<&str>> = parts[0]
            .sidecar_subtitles
            .iter()
            .map(|t| t.url.as_deref())
            .collect();
        assert_eq!(urls, vec![Some("https://beam.test/s.srt")]);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use crate::capability::builders::{DeviceProfileBuilder, MediaSourceBuilder};
    use proptest::prelude::*;

    /// A file as a library might hold it: a codec the phone may or may not
    /// decode, at some size, resolution and bit rate.
    fn source() -> impl Strategy<Value = (&'static str, u64, u32, u64)> {
        (
            prop_oneof![Just("h264"), Just("hevc"), Just("av1"), Just("vp6")],
            1_u64..50_000_000_000,
            prop_oneof![Just(480_u32), Just(720), Just(1080), Just(2160)],
            1_000_000_u64..80_000_000,
        )
    }

    fn policy() -> impl Strategy<Value = QualityPolicy> {
        prop_oneof![
            Just(QualityPolicy::Highest),
            Just(QualityPolicy::Smallest),
            Just(QualityPolicy::MatchScreen),
        ]
    }

    /// The part of a source a policy ranks on: two sources equal here are a
    /// tie the policy has no reason to break either way.
    fn rank(source: &MediaSourceView, policy: &QualityPolicy) -> (u64, u64, u64) {
        match policy {
            QualityPolicy::Smallest => (source.size_bytes, 0, 0),
            _ => quality_key(source),
        }
    }

    proptest! {
        /// The selection is one of the sources offered, every other source is
        /// accounted for as rejected, and the order the server listed them in
        /// changes what is chosen only between sources the policy ranks equal.
        #[test]
        fn a_selection_is_one_of_its_inputs_whatever_their_order(
            files in proptest::collection::vec(source(), 0..8),
            policy in policy(),
        ) {
            let sources: Vec<MediaSourceView> = files
                .iter()
                .enumerate()
                .map(|(index, (codec, size, height, bit_rate))| {
                    MediaSourceBuilder::new(&format!("s{index}"))
                        .video(codec, height * 16 / 9, *height)
                        .size(*size)
                        .bitrate(*bit_rate)
                        .build()
                })
                .collect();
            let mut reversed = sources.clone();
            reversed.reverse();
            let profile = DeviceProfileBuilder::phone_h264_hevc().build();

            let forward = select_source(&sources, &profile, &policy);
            let backward = select_source(&reversed, &profile, &policy);

            match (forward, backward) {
                (Ok(a), Ok(b)) => {
                    prop_assert!(sources.contains(&a.source));
                    prop_assert!(sources.contains(&b.source));
                    prop_assert_eq!(rank(&a.source, &policy), rank(&b.source, &policy));

                    let mut accounted: Vec<&str> =
                        a.rejected.iter().map(|r| r.file_id.as_str()).collect();
                    accounted.push(a.source.file_id.as_str());
                    accounted.sort_unstable();
                    let mut offered: Vec<&str> =
                        sources.iter().map(|s| s.file_id.as_str()).collect();
                    offered.sort_unstable();
                    prop_assert_eq!(accounted, offered);
                }
                (Err(a), Err(b)) => {
                    prop_assert_eq!(a.len(), sources.len(), "every source has a reason");
                    prop_assert_eq!(b.len(), sources.len());
                }
                (a, b) => prop_assert!(false, "order changed the verdict: {:?} / {:?}", a, b),
            }
        }
    }
}
