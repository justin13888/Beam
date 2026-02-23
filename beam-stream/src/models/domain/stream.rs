use uuid::Uuid;

/// A media stream (video, audio, or subtitle) within a file
#[derive(Debug, Clone)]
pub struct MediaStream {
    pub id: Uuid,
    pub file_id: Uuid,
    pub index: u32,
    pub stream_type: StreamType,
    pub codec: String,
    pub metadata: StreamMetadata,
}

/// Type of media stream
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamType {
    Video,
    Audio,
    Subtitle,
}

/// Stream-specific metadata
#[derive(Debug, Clone)]
pub enum StreamMetadata {
    Video(VideoStreamMetadata),
    Audio(AudioStreamMetadata),
    Subtitle(SubtitleStreamMetadata),
}

/// Metadata specific to video streams
#[derive(Debug, Clone)]
pub struct VideoStreamMetadata {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<f64>,
    pub bit_rate: Option<u64>,
    pub color_space: Option<String>,
    pub color_range: Option<String>,
    pub hdr_format: Option<String>,
}

/// Metadata specific to audio streams
#[derive(Debug, Clone)]
pub struct AudioStreamMetadata {
    pub language: Option<String>,
    pub title: Option<String>,
    pub channels: u16,
    pub sample_rate: u32,
    pub channel_layout: Option<String>,
    pub bit_rate: Option<u64>,
    pub is_default: bool,
    pub is_forced: bool,
}

/// Metadata specific to subtitle streams
#[derive(Debug, Clone)]
pub struct SubtitleStreamMetadata {
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
}

/// Parameters for creating a media stream
#[derive(Debug, Clone)]
pub struct CreateMediaStream {
    pub file_id: Uuid,
    pub index: u32,
    pub stream_type: StreamType,
    pub codec: String,
    pub metadata: StreamMetadata,
}

impl From<beam_entity::media_stream::Model> for MediaStream {
    fn from(model: beam_entity::media_stream::Model) -> Self {
        use beam_entity::media_stream::StreamType as DbStreamType;

        let stream_type = match model.stream_type {
            DbStreamType::Video => StreamType::Video,
            DbStreamType::Audio => StreamType::Audio,
            DbStreamType::Subtitle => StreamType::Subtitle,
        };

        let metadata = match model.stream_type {
            DbStreamType::Video => StreamMetadata::Video(VideoStreamMetadata {
                width: model.width.unwrap_or(0) as u32,
                height: model.height.unwrap_or(0) as u32,
                frame_rate: model.frame_rate,
                bit_rate: model.bit_rate.map(|b| b as u64),
                color_space: model.color_space,
                color_range: model.color_range,
                hdr_format: model.hdr_format,
            }),
            DbStreamType::Audio => StreamMetadata::Audio(AudioStreamMetadata {
                language: model.language,
                title: model.title,
                channels: model.channels.unwrap_or(0) as u16,
                sample_rate: model.sample_rate.unwrap_or(0) as u32,
                channel_layout: model.channel_layout,
                bit_rate: model.bit_rate.map(|b| b as u64),
                is_default: model.is_default,
                is_forced: model.is_forced,
            }),
            DbStreamType::Subtitle => StreamMetadata::Subtitle(SubtitleStreamMetadata {
                language: model.language,
                title: model.title,
                is_default: model.is_default,
                is_forced: model.is_forced,
            }),
        };

        Self {
            id: model.id,
            file_id: model.file_id,
            index: model.stream_index as u32,
            stream_type,
            codec: model.codec,
            metadata,
        }
    }
}
