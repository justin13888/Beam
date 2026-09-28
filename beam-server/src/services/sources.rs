//! A title's sources, tracks and all (issue #189).
//!
//! One place builds a [`MediaSource`] from what the index holds -- the files,
//! their probed streams and the subtitle files beside them -- and one place
//! ranks a title's sources, so the detail route's `file_id` and the first
//! entry of `GET /v1/media/{id}/sources` are the same file by construction.
//!
//! A source is usually one file. The parts of a multi-part movie
//! (`Movie (2019) - CD1.avi`, `- CD2.avi`, issue #233) are one source that
//! plays them in order: the parts of one edition in one folder, stacked by
//! [`stack_parts`].

use std::path::PathBuf;
use std::sync::Arc;

use sea_orm::DbErr;
use uuid::Uuid;

use beam_domain::models::sidecar::{SidecarSubtitle, SubtitleFormat};
use beam_domain::models::{Episode, MediaFile, MediaFileContent, MediaStream, StreamMetadata};
use beam_domain::repositories::{
    FileRepository, MediaStreamRepository, MovieRepository, SidecarSubtitleRepository,
};
use beam_domain::utils::source_rank::{SourceRankKey, rank_sources, stack_parts};
use beam_domain::utils::subtitle::is_text_subtitle_codec;

use crate::models::{
    AudioTrack, EpisodeSpan, MediaSource, SourcePart, SubtitleOrigin, SubtitleTrack, VideoTrack,
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

/// One file of a source, with its probed streams.
#[derive(Debug)]
struct PartFile {
    file: MediaFile,
    streams: Vec<MediaStream>,
}

impl PartFile {
    /// The part of a multi-part movie this file is, if it is one.
    fn part_number(&self) -> Option<u32> {
        match self.file.content {
            Some(MediaFileContent::Movie { part_number, .. }) => part_number,
            _ => None,
        }
    }
}

/// The files behind one source, ranked but not yet built: one whole file, or
/// the parts of a multi-part movie in the order they play. Never empty.
#[derive(Debug)]
struct RankedSource {
    edition: Option<String>,
    parts: Vec<PartFile>,
}

impl RankedSource {
    /// The file the source starts with.
    fn lead(&self) -> &PartFile {
        self.parts.first().expect("a source has at least one file")
    }

    /// Every part's size, together.
    fn size_bytes(&self) -> u64 {
        self.parts.iter().map(|part| part.file.size_bytes).sum()
    }

    /// Every part's duration, together, in seconds; `None` while any part's
    /// is unknown.
    fn duration_secs(&self) -> Option<f64> {
        self.parts
            .iter()
            .map(|part| part.file.duration.map(|d| d.as_secs_f64()))
            .sum()
    }

    fn rank_key(&self) -> SourceRankKey {
        let lead = self.lead();
        let video = lead
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
            size_bytes: self.size_bytes(),
            file_id: lead.file.id,
        }
    }
}

/// What a title's detail shows of its files: the primary source and how many
/// there are.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrimarySource {
    /// The primary source's first file.
    pub file_id: Option<Uuid>,
    /// The primary source's duration in seconds: every part's, together.
    pub duration_secs: Option<f64>,
    /// The last episode of the run the primary file holds, when it holds
    /// more than the one episode.
    pub last_episode_number: Option<u32>,
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

    async fn part_file(&self, file: MediaFile) -> Result<PartFile, DbErr> {
        let streams = self.streams.find_by_file_id(file.id).await?;
        Ok(PartFile { file, streams })
    }

    /// Every source of the movie `movie_id`, in rank order: each present file
    /// of each edition, the parts of one edition in one folder stacked into
    /// one.
    async fn ranked_movie_sources(&self, movie_id: Uuid) -> Result<Vec<RankedSource>, DbErr> {
        let mut ranked = Vec::new();
        for entry in self.movies.find_entries_by_movie_id(movie_id).await? {
            let mut files = Vec::new();
            for file in self.files.find_by_movie_entry_id(entry.id).await? {
                files.push(self.part_file(file).await?);
            }
            // The entry is the edition, so only the folder tells two
            // stacks of one edition apart.
            let stacks = stack_parts(files, |part| {
                let folder: PathBuf = part.file.path.parent().map(PathBuf::from)?;
                part.part_number().map(|number| (folder, number))
            });
            ranked.extend(stacks.into_iter().map(|parts| RankedSource {
                edition: entry.edition.clone(),
                parts,
            }));
        }
        rank_sources(&mut ranked, RankedSource::rank_key);
        Ok(ranked)
    }

    /// Every present file of the episode `episode_id`, in rank order, each a
    /// source of its own. An episode has no editions and no parts.
    async fn ranked_episode_sources(&self, episode_id: Uuid) -> Result<Vec<RankedSource>, DbErr> {
        let mut ranked = Vec::new();
        for file in self.files.find_by_episode_id(episode_id).await? {
            ranked.push(RankedSource {
                edition: None,
                parts: vec![self.part_file(file).await?],
            });
        }
        rank_sources(&mut ranked, RankedSource::rank_key);
        Ok(ranked)
    }

    /// The movie's sources, the primary first.
    pub async fn movie_sources(&self, movie_id: Uuid) -> Result<Vec<MediaSource>, DbErr> {
        let ranked = self.ranked_movie_sources(movie_id).await?;
        self.build_all(ranked, None).await
    }

    /// The episode's sources, the primary first.
    pub async fn episode_sources(&self, episode: &Episode) -> Result<Vec<MediaSource>, DbErr> {
        let ranked = self.ranked_episode_sources(episode.id).await?;
        self.build_all(ranked, Some(episode.episode_number)).await
    }

    /// The movie's primary source and how many it has.
    pub async fn movie_primary(&self, movie_id: Uuid) -> Result<PrimarySource, DbErr> {
        Ok(primary(&self.ranked_movie_sources(movie_id).await?, None))
    }

    /// The episode's primary file and how many it has.
    pub async fn episode_primary(&self, episode: &Episode) -> Result<PrimarySource, DbErr> {
        Ok(primary(
            &self.ranked_episode_sources(episode.id).await?,
            Some(episode.episode_number),
        ))
    }

    /// The movie's present files, the primary first.
    pub async fn movie_files(&self, movie_id: Uuid) -> Result<Vec<PlayableFile>, DbErr> {
        Ok(playable(&self.ranked_movie_sources(movie_id).await?, None))
    }

    /// The episode's present files, the primary first.
    pub async fn episode_files(&self, episode: &Episode) -> Result<Vec<PlayableFile>, DbErr> {
        Ok(playable(
            &self.ranked_episode_sources(episode.id).await?,
            Some(episode.episode_number),
        ))
    }

    async fn build_all(
        &self,
        ranked: Vec<RankedSource>,
        episode_number: Option<u32>,
    ) -> Result<Vec<MediaSource>, DbErr> {
        let mut sources = Vec::with_capacity(ranked.len());
        for (rank, source) in ranked.into_iter().enumerate() {
            let mut sidecars = Vec::with_capacity(source.parts.len());
            for part in &source.parts {
                sidecars.push(self.sidecars.find_by_file_id(part.file.id).await?);
            }
            sources.push(build_source(source, rank == 0, episode_number, sidecars));
        }
        Ok(sources)
    }
}

/// One present file of a title, as continue-watching and history pick the
/// one to play.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayableFile {
    pub file_id: Uuid,
    /// This file's own duration: one part's, for a part of a multi-part
    /// movie, since a position is kept within the part it was reported in.
    pub duration_secs: Option<f64>,
    /// The file holds a run of episodes, so its duration is not one
    /// episode's.
    pub spans_episodes: bool,
}

impl PlayableFile {
    /// The file to play of `files` (primary first): `last` -- the one the
    /// viewer last played -- while it is still among them, else the primary.
    pub fn pick(files: &[Self], last: Option<Uuid>) -> Option<&Self> {
        last.and_then(|last| files.iter().find(|file| file.file_id == last))
            .or_else(|| files.first())
    }
}

/// Every file of every source, in rank order, a multi-part source's parts in
/// the order they play: the primary source's first file comes first, and a
/// viewer who stopped in part 2 is sent back to part 2.
fn playable(ranked: &[RankedSource], episode_number: Option<u32>) -> Vec<PlayableFile> {
    ranked
        .iter()
        .flat_map(|source| source.parts.iter())
        .map(|part| PlayableFile {
            file_id: part.file.id,
            duration_secs: part.file.duration.map(|d| d.as_secs_f64()),
            spans_episodes: episode_span(&part.file, episode_number).is_some(),
        })
        .collect()
}

fn primary(ranked: &[RankedSource], episode_number: Option<u32>) -> PrimarySource {
    let first = ranked.first();
    PrimarySource {
        file_id: first.map(|source| source.lead().file.id),
        duration_secs: first.and_then(RankedSource::duration_secs),
        last_episode_number: first
            .and_then(|source| episode_span(&source.lead().file, episode_number))
            .map(|span| span.last_episode_number),
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

/// `sidecars` holds each part's subtitle files, in `ranked.parts`' order.
fn build_source(
    ranked: RankedSource,
    is_primary: bool,
    episode_number: Option<u32>,
    sidecars: Vec<Vec<SidecarSubtitle>>,
) -> MediaSource {
    let size_bytes = ranked.size_bytes();
    let duration_secs = ranked.duration_secs();
    let RankedSource { edition, parts } = ranked;
    let episode_span = parts
        .first()
        .and_then(|lead| episode_span(&lead.file, episode_number));

    let mut lead = None;
    let mut source_parts = Vec::with_capacity(parts.len());
    for (part, sidecars) in parts.into_iter().zip(sidecars) {
        let part_number = part.part_number();
        let PartFile { file, streams } = part;
        let MediaFile {
            id: file_id,
            size_bytes,
            mime_type,
            duration,
            container_format,
            ..
        } = file;
        let tracks = tracks(file_id, streams, sidecars);
        source_parts.push(SourcePart {
            file_id,
            part_number,
            size_bytes,
            duration_secs: duration.map(|d| d.as_secs_f64()),
            subtitle_tracks: tracks.subtitle.clone(),
            stream_url: stream_path(file_id),
            download_url: download_path(file_id),
        });
        if lead.is_none() {
            lead = Some((file_id, mime_type, container_format, tracks));
        }
    }
    let (file_id, mime_type, container_format, tracks) =
        lead.expect("a source has at least one file");
    let Tracks {
        video: video_tracks,
        audio: audio_tracks,
        subtitle: subtitle_tracks,
    } = tracks;

    MediaSource {
        file_id,
        is_primary,
        edition,
        episode_span,
        parts: source_parts,
        size_bytes,
        mime_type,
        container_format,
        duration_secs,
        video_tracks,
        audio_tracks,
        subtitle_tracks,
        stream_url: stream_path(file_id),
        download_url: download_path(file_id),
    }
}

fn stream_path(file_id: Uuid) -> String {
    format!("/v1/files/{file_id}/stream")
}

fn download_path(file_id: Uuid) -> String {
    format!("/v1/files/{file_id}/download")
}

/// One file's tracks.
#[derive(Debug, Clone)]
struct Tracks {
    video: Vec<VideoTrack>,
    audio: Vec<AudioTrack>,
    subtitle: Vec<SubtitleTrack>,
}

/// The tracks of the file `file_id`: its own streams by stream index, then
/// the subtitle files beside it.
fn tracks(file_id: Uuid, streams: Vec<MediaStream>, mut sidecars: Vec<SidecarSubtitle>) -> Tracks {
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

    Tracks {
        video: video_tracks,
        audio: audio_tracks,
        subtitle: subtitle_tracks,
    }
}
