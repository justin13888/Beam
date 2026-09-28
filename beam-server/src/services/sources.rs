//! A title's sources, tracks and all (issue #189).
//!
//! One place builds a [`MediaSource`] from what the index holds -- the file,
//! its probed streams and the subtitle files beside it -- and one place ranks
//! a title's files, so the detail route's `file_id` and the first entry of
//! `GET /v1/media/{id}/sources` are the same file by construction.

use std::sync::Arc;

use sea_orm::DbErr;
use uuid::Uuid;

use beam_domain::models::sidecar::{SidecarSubtitle, SubtitleFormat};
use beam_domain::models::{Episode, MediaFile, MediaFileContent, MediaStream, StreamMetadata};
use beam_domain::repositories::{
    FileRepository, MediaStreamRepository, MovieRepository, SidecarSubtitleRepository,
};
use beam_domain::utils::source_rank::{SourceRankKey, rank_sources};
use beam_domain::utils::subtitle::is_text_subtitle_codec;

use crate::models::{
    AudioTrack, EpisodeSpan, MediaSource, SubtitleOrigin, SubtitleTrack, VideoTrack,
};

/// The largest subtitle file Beam converts to WebVTT: 8 MiB, about a hundred
/// times a feature film's SubRip. A larger file is served as stored only, so
/// one request cannot make the server hold and rewrite an arbitrarily large
/// file.
pub const SUBTITLE_CONVERT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The path a sidecar subtitle is served from, as stored.
pub fn subtitle_path(file_id: Uuid, subtitle_id: Uuid) -> String {
    format!("/v1/files/{file_id}/subtitles/{subtitle_id}")
}

/// The path a sidecar subtitle is served from as WebVTT.
pub fn subtitle_webvtt_path(file_id: Uuid, subtitle_id: Uuid) -> String {
    format!("{}/webvtt", subtitle_path(file_id, subtitle_id))
}

/// Whether Beam serves `sidecar` as WebVTT: a SubRip or WebVTT file no larger
/// than [`SUBTITLE_CONVERT_MAX_BYTES`]. ASS and SSA carry styling and
/// positioning WebVTT cannot express, so a client that reads them is handed
/// them as stored and one that cannot is not handed a lossy copy.
pub fn has_webvtt_rendition(format: SubtitleFormat, size_bytes: u64) -> bool {
    matches!(format, SubtitleFormat::Srt | SubtitleFormat::Vtt)
        && size_bytes <= SUBTITLE_CONVERT_MAX_BYTES
}

/// The file behind one source, ranked but not yet built.
#[derive(Debug)]
struct RankedFile {
    file: MediaFile,
    edition: Option<String>,
    streams: Vec<MediaStream>,
}

impl RankedFile {
    fn rank_key(&self) -> SourceRankKey {
        let video = self
            .streams
            .iter()
            .find_map(|stream| match &stream.metadata {
                StreamMetadata::Video(video) => Some(video),
                _ => None,
            });
        SourceRankKey {
            is_default_edition: self.edition.is_none(),
            height: video.map_or(0, |video| video.height),
            video_bit_rate: video.and_then(|video| video.bit_rate).unwrap_or(0),
            size_bytes: self.file.size_bytes,
            file_id: self.file.id,
        }
    }
}

/// What a title's detail shows of its files: the primary one and how many
/// there are.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrimarySource {
    pub file_id: Option<Uuid>,
    /// The primary file's duration in seconds.
    pub duration_secs: Option<f64>,
    /// Whether the primary file holds a run of episodes.
    pub spans_episodes: bool,
    pub source_count: u32,
}

/// Reads a title's sources from the index.
#[derive(Debug)]
pub struct SourceCatalog {
    movies: Arc<dyn MovieRepository>,
    files: Arc<dyn FileRepository>,
    streams: Arc<dyn MediaStreamRepository>,
    sidecars: Arc<dyn SidecarSubtitleRepository>,
}

impl SourceCatalog {
    pub fn new(
        movies: Arc<dyn MovieRepository>,
        files: Arc<dyn FileRepository>,
        streams: Arc<dyn MediaStreamRepository>,
        sidecars: Arc<dyn SidecarSubtitleRepository>,
    ) -> Self {
        Self {
            movies,
            files,
            streams,
            sidecars,
        }
    }

    /// Every present file of the movie `movie_id`, in rank order.
    async fn ranked_movie_files(&self, movie_id: Uuid) -> Result<Vec<RankedFile>, DbErr> {
        let mut ranked = Vec::new();
        for entry in self.movies.find_entries_by_movie_id(movie_id).await? {
            for file in self.files.find_by_movie_entry_id(entry.id).await? {
                let streams = self.streams.find_by_file_id(file.id).await?;
                ranked.push(RankedFile {
                    file,
                    edition: entry.edition.clone(),
                    streams,
                });
            }
        }
        rank_sources(&mut ranked, RankedFile::rank_key);
        Ok(ranked)
    }

    /// Every present file of the episode `episode_id`, in rank order. An
    /// episode has no editions.
    async fn ranked_episode_files(&self, episode_id: Uuid) -> Result<Vec<RankedFile>, DbErr> {
        let mut ranked = Vec::new();
        for file in self.files.find_by_episode_id(episode_id).await? {
            let streams = self.streams.find_by_file_id(file.id).await?;
            ranked.push(RankedFile {
                file,
                edition: None,
                streams,
            });
        }
        rank_sources(&mut ranked, RankedFile::rank_key);
        Ok(ranked)
    }

    /// The movie's sources, the primary first.
    pub async fn movie_sources(&self, movie_id: Uuid) -> Result<Vec<MediaSource>, DbErr> {
        let ranked = self.ranked_movie_files(movie_id).await?;
        self.build_all(ranked, None).await
    }

    /// The episode's sources, the primary first.
    pub async fn episode_sources(&self, episode: &Episode) -> Result<Vec<MediaSource>, DbErr> {
        let ranked = self.ranked_episode_files(episode.id).await?;
        self.build_all(ranked, Some(episode.episode_number)).await
    }

    /// The movie's primary file and how many it has.
    pub async fn movie_primary(&self, movie_id: Uuid) -> Result<PrimarySource, DbErr> {
        Ok(primary(&self.ranked_movie_files(movie_id).await?, None))
    }

    /// The episode's primary file and how many it has.
    pub async fn episode_primary(&self, episode: &Episode) -> Result<PrimarySource, DbErr> {
        Ok(primary(
            &self.ranked_episode_files(episode.id).await?,
            Some(episode.episode_number),
        ))
    }

    async fn build_all(
        &self,
        ranked: Vec<RankedFile>,
        episode_number: Option<u32>,
    ) -> Result<Vec<MediaSource>, DbErr> {
        let mut sources = Vec::with_capacity(ranked.len());
        for (rank, file) in ranked.into_iter().enumerate() {
            let sidecars = self.sidecars.find_by_file_id(file.file.id).await?;
            sources.push(build_source(file, rank == 0, episode_number, sidecars));
        }
        Ok(sources)
    }
}

fn primary(ranked: &[RankedFile], episode_number: Option<u32>) -> PrimarySource {
    let first = ranked.first();
    PrimarySource {
        file_id: first.map(|ranked| ranked.file.id),
        duration_secs: first.and_then(|ranked| ranked.file.duration.map(|d| d.as_secs_f64())),
        spans_episodes: first
            .is_some_and(|ranked| episode_span(&ranked.file, episode_number).is_some()),
        source_count: u32::try_from(ranked.len()).unwrap_or(u32::MAX),
    }
}

/// The run of episodes `file` holds when it is a file of the episode
/// numbered `episode_number` holding more than that one.
fn episode_span(file: &MediaFile, episode_number: Option<u32>) -> Option<EpisodeSpan> {
    let last = match file.content {
        Some(MediaFileContent::Episode {
            last_episode_number: Some(last),
            ..
        }) => last,
        _ => return None,
    };
    let first = episode_number?;
    (last > first).then_some(EpisodeSpan {
        first_episode_number: first,
        last_episode_number: last,
    })
}

fn build_source(
    ranked: RankedFile,
    is_primary: bool,
    episode_number: Option<u32>,
    mut sidecars: Vec<SidecarSubtitle>,
) -> MediaSource {
    let RankedFile {
        file,
        edition,
        streams,
    } = ranked;
    let episode_span = episode_span(&file, episode_number);
    let MediaFile {
        id: file_id,
        size_bytes,
        mime_type,
        duration,
        container_format,
        ..
    } = file;

    let mut video_tracks = Vec::new();
    let mut audio_tracks = Vec::new();
    let mut subtitle_tracks = Vec::new();
    for stream in streams {
        let MediaStream {
            index,
            codec,
            metadata,
            ..
        } = stream;
        match metadata {
            StreamMetadata::Video(video) => video_tracks.push(VideoTrack {
                index,
                codec,
                width: video.width,
                height: video.height,
                frame_rate: video
                    .frame_rate
                    .filter(|rate| rate.is_finite() && *rate > 0.0),
                bit_rate: video.bit_rate.filter(|rate| *rate > 0),
                hdr_format: video.hdr_format,
            }),
            StreamMetadata::Audio(audio) => audio_tracks.push(AudioTrack {
                index,
                codec,
                language: audio.language,
                title: audio.title,
                channels: audio.channels,
                channel_layout: audio.channel_layout,
                sample_rate: Some(audio.sample_rate).filter(|rate| *rate > 0),
                bit_rate: audio.bit_rate.filter(|rate| *rate > 0),
                is_default: audio.is_default,
                is_forced: audio.is_forced,
            }),
            StreamMetadata::Subtitle(subtitle) => subtitle_tracks.push(SubtitleTrack {
                origin: SubtitleOrigin::Embedded,
                index: Some(index),
                sidecar_id: None,
                is_text: is_text_subtitle_codec(&codec),
                codec,
                language: subtitle.language,
                title: subtitle.title,
                is_default: subtitle.is_default,
                is_forced: subtitle.is_forced,
                is_hearing_impaired: subtitle.is_hearing_impaired,
                url: None,
                webvtt_url: None,
            }),
        }
    }
    video_tracks.sort_by_key(|track| track.index);
    audio_tracks.sort_by_key(|track| track.index);
    subtitle_tracks.sort_by_key(|track| track.index);

    // Sidecars after the file's own tracks, grouped by language (untagged
    // last), full subtitles before forced ones, then by title and id so the
    // order never depends on how the store returned them.
    sidecars.sort_by(|a, b| {
        (
            a.info.language.is_none(),
            &a.info.language,
            a.info.is_forced,
            &a.info.title,
            a.id,
        )
            .cmp(&(
                b.info.language.is_none(),
                &b.info.language,
                b.info.is_forced,
                &b.info.title,
                b.id,
            ))
    });
    subtitle_tracks.extend(sidecars.into_iter().map(|sidecar| {
        let SidecarSubtitle {
            id,
            info,
            size_bytes,
            ..
        } = sidecar;
        SubtitleTrack {
            origin: SubtitleOrigin::Sidecar,
            index: None,
            sidecar_id: Some(id),
            codec: info.format.codec_name().to_string(),
            language: info.language,
            title: info.title,
            is_default: info.is_default,
            is_forced: info.is_forced,
            is_hearing_impaired: info.is_sdh,
            is_text: true,
            url: Some(subtitle_path(file_id, id)),
            webvtt_url: has_webvtt_rendition(info.format, size_bytes)
                .then(|| subtitle_webvtt_path(file_id, id)),
        }
    }));

    MediaSource {
        file_id,
        is_primary,
        edition,
        episode_span,
        size_bytes,
        mime_type,
        container_format,
        duration_secs: duration.map(|d| d.as_secs_f64()),
        video_tracks,
        audio_tracks,
        subtitle_tracks,
        stream_url: format!("/v1/files/{file_id}/stream"),
        download_url: format!("/v1/files/{file_id}/download"),
    }
}
