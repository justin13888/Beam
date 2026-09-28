use kynos::Schema;
use serde::Serialize;
use uuid::Uuid;

use crate::models::search::PageInfo;

/// One playable/downloadable version of a media item. Beam models multiple
/// deliverable qualities/editions as distinct source files rather than
/// transcoding on demand (see ADR-0004: never live-transcode); this is how a
/// client picks among them for constrained-bandwidth playback.
///
/// The one track model Beam has (issue #189): every video, audio and
/// subtitle track here is tied to this file, addressed by its stream index,
/// and names its codec as FFmpeg does (`h264`, `eac3`, `subrip`).
///
/// A movie split across files (`Movie (2019) - CD1.avi`, `- CD2.avi`) is one
/// source whose `parts` play in sequence (issue #233): a client plays each
/// part's `stream_url` in turn -- a concatenated playlist -- and Beam never
/// joins them (ADR-0004). A whole file is a source of one part. The fields
/// that name one file -- `file_id`, the tracks, `stream_url`,
/// `download_url`, `mime_type` and `container_format` -- are the first
/// part's; `size_bytes` and `duration_secs` are the whole source's.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct MediaSource {
    /// The file this source starts with: its only file, or its first part.
    pub file_id: Uuid,
    /// Whether this is the source a client plays when the viewer has not
    /// chosen. Exactly one source of a title is primary, and it is listed
    /// first: the default edition, then the tallest picture, the highest
    /// video bit rate and the largest file.
    pub is_primary: bool,
    /// The edition the filename names (`Director's Cut`); absent for the
    /// default edition, and always for an episode.
    pub edition: Option<String>,
    /// Present when this file holds a run of episodes (`S01E01-E03`): its
    /// duration is then the whole run's, not one episode's.
    pub episode_span: Option<EpisodeSpan>,
    /// The files this source plays, in order: one for a whole file, each
    /// part of a multi-part movie otherwise. Never empty.
    pub parts: Vec<SourcePart>,
    /// Every part's size, together.
    pub size_bytes: u64,
    pub mime_type: Option<String>,
    pub container_format: Option<String>,
    /// Every part's duration, together; absent while any part's is unknown.
    pub duration_secs: Option<f64>,
    /// Video tracks, by stream index.
    pub video_tracks: Vec<VideoTrack>,
    /// Audio tracks, by stream index.
    pub audio_tracks: Vec<AudioTrack>,
    /// Subtitle tracks: the file's own, by stream index, then the subtitle
    /// files beside it.
    pub subtitle_tracks: Vec<SubtitleTrack>,
    /// Direct-play stream URL for this file (Range-request capable, inline).
    pub stream_url: String,
    /// Download URL for this file (Range-request capable, `Content-Disposition: attachment`).
    pub download_url: String,
}

/// Every source of a movie or an episode, primary first: what
/// `GET /v1/media/{id}/sources` returns.
///
/// A connection, as `GET /v1/media` answers, though a title's sources
/// are few enough that it is never paged: `items` is the whole list, both of
/// `page_info`'s `has_*_page` flags are `false`, and it carries no cursors.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct MediaSourceConnection {
    /// The title's sources, primary first. Empty for an episode with no
    /// file yet.
    pub items: Vec<MediaSource>,
    /// Always a single, complete page.
    pub page_info: PageInfo,
}

impl MediaSourceConnection {
    /// All of `sources`, as the one page they fill.
    pub fn complete(sources: Vec<MediaSource>) -> Self {
        Self {
            items: sources,
            page_info: PageInfo {
                has_next_page: false,
                has_previous_page: false,
                start_cursor: None,
                end_cursor: None,
            },
        }
    }
}

/// One file of a source, played in the order the source's `parts` lists it.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct SourcePart {
    pub file_id: Uuid,
    /// The part the filename names (`CD2` is `2`); absent for a whole file.
    pub part_number: Option<u32>,
    pub size_bytes: u64,
    pub duration_secs: Option<f64>,
    /// This file's subtitle tracks, as the source's `subtitle_tracks` lists
    /// the first part's: a subtitle file beside a part times that part.
    pub subtitle_tracks: Vec<SubtitleTrack>,
    /// Direct-play stream URL for this file.
    pub stream_url: String,
    /// Download URL for this file.
    pub download_url: String,
}

/// The episodes one file holds, first and last inclusive.
#[derive(Clone, Copy, Debug, Serialize, serde::Deserialize, Schema)]
pub struct EpisodeSpan {
    pub first_episode_number: u32,
    pub last_episode_number: u32,
}

/// A video stream of a source file.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct VideoTrack {
    /// The stream's index in its file.
    pub index: u32,
    /// FFmpeg's codec name: `h264`, `hevc`, `av1`, `vp9`, `mpeg2video`.
    pub codec: String,
    pub width: u32,
    pub height: u32,
    /// Frames per second; absent when the file does not say.
    pub frame_rate: Option<f64>,
    /// Average bit rate in bits per second; absent when the file does not
    /// say.
    pub bit_rate: Option<u64>,
    /// `HDR10` or `HLG`; absent for standard dynamic range.
    pub hdr_format: Option<String>,
}

/// An audio stream of a source file.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct AudioTrack {
    /// The stream's index in its file.
    pub index: u32,
    /// FFmpeg's codec name: `aac`, `ac3`, `eac3`, `truehd`, `dts`, `flac`,
    /// `opus`.
    pub codec: String,
    /// The ISO 639-2/B code the file tags the track with (`eng`, `jpn`).
    pub language: Option<String>,
    /// The track's own title (`Commentary`).
    pub title: Option<String>,
    pub channels: u16,
    /// A description of the channel layout (`5.1(side)`).
    pub channel_layout: Option<String>,
    /// Samples per second; absent when the file does not say.
    pub sample_rate: Option<u32>,
    /// Average bit rate in bits per second; absent when the file does not
    /// say.
    pub bit_rate: Option<u64>,
    /// The file marks this track as the one to play by default.
    pub is_default: bool,
    pub is_forced: bool,
}

/// Where a subtitle track lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleOrigin {
    /// A stream inside the video file. Beam never extracts one (ADR-0004):
    /// a client reads it from the stream itself.
    Embedded,
    /// A subtitle file beside the video, served on its own.
    Sidecar,
}

/// A subtitle track of a source file.
#[derive(Clone, Debug, Serialize, serde::Deserialize, Schema)]
pub struct SubtitleTrack {
    pub origin: SubtitleOrigin,
    /// The stream's index in its file; present exactly for an embedded
    /// track.
    pub index: Option<u32>,
    /// The subtitle file's identifier; present exactly for a sidecar.
    pub sidecar_id: Option<Uuid>,
    /// FFmpeg's codec name: `subrip`, `webvtt`, `ass`, `ssa`, `mov_text`, or
    /// an image format such as `hdmv_pgs_subtitle`.
    pub codec: String,
    /// The ISO 639-2/B language code (`eng`, `spa`).
    pub language: Option<String>,
    /// The track's own title (`Commentary`, `pt-BR`).
    pub title: Option<String>,
    pub is_default: bool,
    /// Subtitles only for foreign-language dialogue.
    pub is_forced: bool,
    /// Subtitles for the deaf and hard of hearing (SDH).
    pub is_hearing_impaired: bool,
    /// Text a client can render itself, as opposed to an image format it
    /// must decode.
    pub is_text: bool,
    /// Where to fetch the track as stored; present exactly for a sidecar.
    pub url: Option<String>,
    /// Where to fetch the track as WebVTT, which a browser's `<track>`
    /// element reads; present for a SubRip or WebVTT sidecar small enough to
    /// convert.
    pub webvtt_url: Option<String>,
}
