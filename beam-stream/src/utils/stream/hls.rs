use m3u8_rs::{
    AlternativeMedia, AlternativeMediaType, MasterPlaylist, MediaPlaylist, MediaPlaylistType,
    VariantStream,
};
use num::ToPrimitive;
use std::collections::HashMap;
use tracing::debug;

use crate::utils::stream::config::{OutputStream, SubtitleStream};

use super::config::StreamConfiguration;

const HLS_VERSION: usize = 6; // HLS 6 is a good minimum for fMP4, CMAF, low-latency streaming, segment-related features

// TODO: There are still inaccuracies with the generated playlists

/// Generates compliant HLS playlists and segments from a stream configuration
pub struct HlsStreamGenerator {
    configuration: StreamConfiguration,
}

impl HlsStreamGenerator {
    fn new(configuration: StreamConfiguration) -> Self {
        Self { configuration }
    }

    /// Get all streams deterministically with unique variant names.
    /// Returns Vec<(variant_name, &OutputStream)>.
    fn get_streams(&self) -> Vec<(String, &OutputStream)> {
        // Hashmap to track variant name occurrences
        let mut variant_names: HashMap<String, usize> = HashMap::new();

        // Collect all streams with unique variant names
        self.configuration
            .streams
            .iter()
            .map(|stream| {
                // Determine variant name and ensure uniqueness
                let variant_name = Self::get_variant_name(stream);
                let occurrence_count = *variant_names
                    .entry(variant_name.clone())
                    .and_modify(|count| *count += 1)
                    .or_insert(0);
                let variant_name = if occurrence_count > 0 {
                    format!("{}_{}", variant_name, occurrence_count)
                } else {
                    variant_name
                };

                (variant_name, stream)
            })
            .collect()
    }

    /// Get master playlist
    pub fn get_master_playlist(&self) -> MasterPlaylist {
        // #EXT-X-STREAM-INF
        let mut variants: Vec<VariantStream> = vec![];
        // #EXT-X-MEDIA:<attribute-list>
        let mut alternatives: Vec<AlternativeMedia> = vec![];

        for (variant_name, stream) in self.get_streams().into_iter() {
            match stream {
                OutputStream::Video(stream) => {
                    let playlist_uri =
                        Self::get_playlist_uri(HlsPlaylistType::Video, &variant_name);
                    let variant = VariantStream {
                        is_i_frame: false, // TODO: I think we need another variant for I-frames only
                        uri: playlist_uri.clone(),
                        bandwidth: stream.max_rate as u64,
                        average_bandwidth: Some(stream.bit_rate as u64),
                        codecs: Some(stream.codec.to_string()),
                        resolution: Some(m3u8_rs::Resolution {
                            width: stream.resolution.width as u64,
                            height: stream.resolution.height as u64,
                        }),
                        frame_rate: Some(stream.frame_rate.to_f64().unwrap_or_else(|| {
                            panic!(
                                "Frame rate ({:?}) should be a valid rational number.",
                                stream.frame_rate
                            )
                        })), // TODO: Check that it really is the number of frames per second
                        hdcp_level: None, // We don't set HDCP as we do not use DRM.
                        audio: None,
                        video: None,
                        subtitles: None,
                        closed_captions: None,
                        other_attributes: None,
                    };
                    variants.push(variant);
                }
                OutputStream::Audio(stream) => {
                    let playlist_uri =
                        Self::get_playlist_uri(HlsPlaylistType::Audio, &variant_name);
                    let group_id = {
                        let codec_id = stream.codec.to_string();
                        let language = stream.language.as_ref();

                        let mut s = format!("audio_{codec_id}");
                        if let Some(language) = language {
                            s += &format!("_{language}");
                        }

                        s
                    };

                    let alternative = AlternativeMedia {
                        media_type: m3u8_rs::AlternativeMediaType::Audio,
                        uri: Some(playlist_uri.clone()),
                        group_id,
                        language: stream.language.clone(),
                        assoc_language: None,
                        name: stream.title.clone(),
                        default: stream.is_default,
                        autoselect: stream.is_autoselect,
                        forced: false,     // TODO: Detect forced subtitles
                        instream_id: None, // We do not use in-stream captions
                        characteristics: None,
                        channels: stream.channel_layout.clone(),
                        other_attributes: None,
                    };
                    alternatives.push(alternative);
                }
                OutputStream::Subtitle(stream) => {
                    let subtitle_uri = Self::get_subtitle_uri(stream, &variant_name);
                    let group_id = stream.codec.to_string(); // TODO: This is probably still wrong...

                    let alternative = AlternativeMedia {
                        media_type: AlternativeMediaType::Subtitles,
                        uri: Some(subtitle_uri.clone()),
                        group_id,
                        language: stream.language.clone(),
                        assoc_language: None,
                        name: stream.title.as_ref().cloned().unwrap_or_default(),
                        default: stream.is_default,
                        autoselect: stream.is_autoselect,
                        forced: stream.is_forced,
                        instream_id: None, // We do not use in-stream captions
                        characteristics: None,
                        channels: None,
                        other_attributes: None,
                    };
                    alternatives.push(alternative);
                }
            }
        }

        MasterPlaylist {
            version: Some(HLS_VERSION),
            variants,
            session_data: vec![], // We store metadata elsewhere instead of using this tag
            session_key: vec![],  // We do not use encryption for now
            start: None,
            independent_segments: true, // Set to true because we assume segments are encoded to be independently decodable
            alternatives,
            unknown_tags: vec![],
        }
        // TODO: Write tests for correctness
    }

    /// Get media playlists for all variant streams
    /// Returns (playlist_uri, MediaPlaylist) where `playlist_uri` is the URI to be used in the master playlist
    pub fn get_media_playlists(&self) -> Vec<(String, MediaPlaylist)> {
        let playlists: Vec<(String, MediaPlaylist)> = self
            .get_streams()
            .into_iter()
            .map(|(variant_name, stream)| {
                // Map streams to media playlists
                match stream {
                    OutputStream::Video(_stream) => {
                        let playlist_uri =
                            Self::get_playlist_uri(HlsPlaylistType::Video, &variant_name);
                        let playlist = MediaPlaylist {
                            version: Some(HLS_VERSION),
                            target_duration: self.configuration.target_duration,
                            media_sequence: 0,
                            segments: vec![], // TODO
                            discontinuity_sequence: 0,
                            end_list: true,
                            playlist_type: Some(MediaPlaylistType::Vod),
                            i_frames_only: false, // TODO: Set #EXT-X-I-FRAMES-ONLY to true after i-frame extraction is implemented for fast seeking
                            start: None, // This is typically only used if you client to start on a live edge or the beginning of a VOD stream
                            independent_segments: true, // Set to true because we assume segments are encoded to be independently decodable
                            unknown_tags: vec![],
                        };

                        (playlist_uri, playlist)
                    }

                    OutputStream::Audio(_stream) => {
                        let playlist_uri =
                            Self::get_playlist_uri(HlsPlaylistType::Audio, &variant_name);
                        let playlist = MediaPlaylist {
                            version: Some(HLS_VERSION),
                            target_duration: self.configuration.target_duration,
                            media_sequence: 0,
                            segments: vec![], // TODO
                            discontinuity_sequence: 0,
                            end_list: true,
                            playlist_type: Some(MediaPlaylistType::Vod),
                            i_frames_only: false, // TODO: Set #EXT-X-I-FRAMES-ONLY to true after i-frame extraction is implemented for fast seeking
                            start: None, // This is typically only used if you client to start on a live edge or the beginning of a VOD stream
                            independent_segments: true, // Set to true because we assume segments are encoded to be independently decodable
                            unknown_tags: vec![],
                        };

                        (playlist_uri, playlist)
                    }
                    OutputStream::Subtitle(stream) => {
                        let subtitle_uri = Self::get_subtitle_uri(stream, &variant_name);
                        let playlist = MediaPlaylist {
                            version: Some(HLS_VERSION),
                            target_duration: self.configuration.target_duration,
                            media_sequence: 0,
                            segments: vec![], // TODO
                            discontinuity_sequence: 0,
                            end_list: true,
                            playlist_type: Some(MediaPlaylistType::Vod),
                            i_frames_only: false, // TODO: Set #EXT-X-I-FRAMES-ONLY to true after i-frame extraction is implemented for fast seeking
                            start: None, // This is typically only used if you client to start on a live edge or the beginning of a VOD stream
                            independent_segments: true, // Set to true because we assume segments are encoded to be independently decodable
                            unknown_tags: vec![],
                        };

                        (subtitle_uri, playlist)
                    }
                }
            })
            .collect();

        debug!("Generated {} media playlists", playlists.len());

        playlists
        // TODO: Write tests for correctness
    }

    // Get variant name from stream
    pub fn get_variant_name(stream: &OutputStream) -> String {
        match stream {
            OutputStream::Video(stream) => {
                let pixel_height: u32 = stream.resolution.height;
                format!("{pixel_height}p")
            }
            OutputStream::Audio(stream) => {
                let codec = stream.codec.to_string();
                let language = stream.language.as_ref();
                let mut n = codec;
                if let Some(language) = language {
                    n += &format!("-{language}");
                }

                n
            }
            OutputStream::Subtitle(stream) => stream
                .title
                .as_ref()
                .unwrap_or(
                    stream
                        .language
                        .as_ref()
                        .unwrap_or(&stream.source_stream_index.to_string()),
                )
                .to_ascii_lowercase(),
        }
    }

    /// Get playlist URI for a given variant name
    pub fn get_playlist_uri(playlist_type: HlsPlaylistType, variant_name: &str) -> String {
        match playlist_type {
            HlsPlaylistType::Video => format!("video/{variant_name}/index.m3u8"),
            HlsPlaylistType::Audio => format!("audio/{variant_name}/index.m3u8"),
        }
    }

    /// Get subtitle URI for a given variant name
    pub fn get_subtitle_uri(stream: &SubtitleStream, variant_name: &str) -> String {
        format!("subtitles/{variant_name}.{}", stream.codec.file_extension())
    }

    // TODO: Add methods for generating necessary segments in bulk, just-in-time
}

impl From<StreamConfiguration> for HlsStreamGenerator {
    fn from(configuration: StreamConfiguration) -> Self {
        Self::new(configuration)
    }
}

pub enum HlsPlaylistType {
    Video,
    Audio,
}
