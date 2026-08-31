//! Choosing a file to play, and explaining every file that was not chosen.
//!
//! Rejected sources are returned alongside the choice rather than filtered
//! away. A source picker that silently hides the 4K remux leaves the user
//! wondering where it went; one that greys it out with "no AV1 decoder on
//! this device" has told them something true and actionable -- and, given
//! Beam's answer to an unplayable file is "index a compatible version", it
//! tells the operator exactly what to do about it.

use super::{AudioCodec, DeviceProfile, VideoCodec};

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

    #[test]
    fn ranking_does_not_depend_on_the_order_sources_arrive_in() {
        let profile = DeviceProfileBuilder::phone_h264_hevc().build();
        let forward = [uhd_hevc(), hd_h264(), sd_h264()];
        let reversed = [sd_h264(), hd_h264(), uhd_hevc()];

        let a = select_source(&forward, &profile, &QualityPolicy::Highest).expect("a source");
        let b = select_source(&reversed, &profile, &QualityPolicy::Highest).expect("a source");
        assert_eq!(a.source.file_id, b.source.file_id);
    }
}
