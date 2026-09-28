//! Operator-local playback telemetry (issue #143, ADR-0019).
//!
//! Clients report what failed to start, what rebuffered and what switched
//! source, naming the file involved. This service resolves each file to coarse
//! dimensions -- container, codecs, a height and a bitrate class -- counts the
//! event under the UTC day it arrived, and forgets the file. It never learns
//! who reported: the handler authenticates the session and passes no identity
//! on. Nothing here leaves the server; the counts are for the operator, read
//! through `GET /v1/admin/telemetry/playback`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::time::Duration;

use beam_domain::models::playback_telemetry::{
    NO_STREAM_LABEL, PlaybackTelemetryEvent, PlaybackTelemetrySummary, RebufferKey, StartKey,
    StartOutcome, SwitchKey,
};
use beam_domain::models::stream::{MediaStream, StreamMetadata};
use beam_domain::models::{BitrateClass, ClientKind, HeightClass, MediaFile};
use beam_domain::repositories::{
    FileRepository, MediaStreamRepository, PlaybackTelemetryRepository,
};
use beam_domain::services::Clock;
use beam_domain::utils::telemetry::{
    UNKNOWN_LABEL, bitrate_class, file_bitrate, height_class, normalize_label,
};
use chrono::{Days, NaiveDate};
use sea_orm::DbErr;
use thiserror::Error;
use uuid::Uuid;

use crate::models::playback_telemetry::{
    FieldError, MAX_BATCH_EVENTS, MAX_REBUFFER_MS, PlaybackRebufferBucketCount,
    PlaybackRebufferCount, PlaybackSourceSwitchCount, PlaybackStartCount,
    PlaybackStartFailureCount, PlaybackTelemetryBatch, PlaybackTelemetryReport,
    PlaybackTelemetryTotals,
};

/// How many days a report covers when the caller names no range: the last
/// thirty, today included.
pub const DEFAULT_REPORT_DAYS: u64 = 30;

/// The widest range one report may cover, in days.
pub const MAX_REPORT_DAYS: u64 = 366;

/// How often expired counts are pruned.
pub const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// What the service is configured with.
#[derive(Debug, Clone, Copy)]
pub struct PlaybackTelemetryConfig {
    /// Whether reports are counted (`BEAM_PLAYBACK_TELEMETRY_ENABLED`).
    pub enabled: bool,
    /// How many days counts are kept after the day they count.
    pub retention_days: u32,
}

/// Why a batch was not counted.
#[derive(Debug, Error)]
pub enum IngestError {
    #[error("playback telemetry is disabled on this server")]
    Disabled,
    #[error("the batch breaks {} rule(s)", .0.len())]
    Invalid(Vec<FieldError>),
    #[error("recording playback telemetry failed: {0}")]
    Db(#[from] DbErr),
}

/// Why a report could not be produced.
#[derive(Debug, Error)]
pub enum ReportError {
    #[error("{0}")]
    InvalidDateRange(String),
    #[error("reading playback telemetry failed: {0}")]
    Db(#[from] DbErr),
}

/// What happened to a counted batch's events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    /// Events counted.
    pub recorded: u32,
    /// Events whose file could not be resolved, and so were not counted.
    pub dropped: u32,
}

/// Every rule a batch must meet beyond what deserialising it checks.
///
/// Pure, so the rules are tested without a server: 1 to
/// [`MAX_BATCH_EVENTS`] events in total and in each list, a rebuffer of 1 ms
/// to [`MAX_REBUFFER_MS`], and a switch between two different files. Every
/// broken rule is reported, each at the JSON Pointer of what broke it.
pub fn validate_batch(batch: &PlaybackTelemetryBatch) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let lists = [
        ("starts", batch.starts.len()),
        ("start_failures", batch.start_failures.len()),
        ("rebuffers", batch.rebuffers.len()),
        ("source_switches", batch.source_switches.len()),
    ];
    for (name, len) in lists {
        if len > MAX_BATCH_EVENTS {
            errors.push(FieldError {
                pointer: format!("/{name}"),
                detail: format!("at most {MAX_BATCH_EVENTS} events, got {len}"),
            });
        }
    }
    let total = batch.event_count();
    if total == 0 {
        errors.push(FieldError {
            pointer: String::new(),
            detail: "a batch carries at least one event".to_owned(),
        });
    } else if total > MAX_BATCH_EVENTS && errors.is_empty() {
        errors.push(FieldError {
            pointer: String::new(),
            detail: format!("at most {MAX_BATCH_EVENTS} events in total, got {total}"),
        });
    }
    for (index, rebuffer) in batch.rebuffers.iter().enumerate() {
        if !(1..=MAX_REBUFFER_MS).contains(&rebuffer.duration_ms) {
            errors.push(FieldError {
                pointer: format!("/rebuffers/{index}/duration_ms"),
                detail: format!(
                    "must be from 1 to {MAX_REBUFFER_MS}, got {}",
                    rebuffer.duration_ms
                ),
            });
        }
    }
    for (index, switch) in batch.source_switches.iter().enumerate() {
        if switch.from_file_id == switch.to_file_id {
            errors.push(FieldError {
                pointer: format!("/source_switches/{index}/to_file_id"),
                detail: "must differ from from_file_id".to_owned(),
            });
        }
    }
    errors
}

// ── Resolving a file to its dimensions ──────────────────────────────────────
//
// The one place that reads the stream model: `ResolvedFile::of` and
// `PlaybackTelemetryService::resolve` change with it, and nothing that
// counts does.

/// The dimensions a file is counted under, with the file id already gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    pub container: String,
    pub video_codec: String,
    pub height_class: HeightClass,
    pub bitrate_class: BitrateClass,
    /// Audio codec labels, in the order `GET /v1/media/{id}/sources` lists a
    /// source's `audio_tracks` -- stream order.
    pub audio_codecs: Vec<String>,
    /// Which of `audio_codecs` is flagged default, if any.
    pub default_audio: Option<usize>,
}

impl ResolvedFile {
    /// A file's dimensions from its row and its streams. The first video
    /// stream (by stream index) supplies the video codec and height; the
    /// file's average bitrate, or failing that the video stream's, the
    /// bitrate class. Every name is normalised as the library report
    /// normalises it.
    pub fn of(file: &MediaFile, streams: &[MediaStream]) -> Self {
        let mut ordered: Vec<&MediaStream> = streams.iter().collect();
        ordered.sort_by_key(|stream| stream.index);

        let video = ordered.iter().find_map(|stream| match &stream.metadata {
            StreamMetadata::Video(video) => Some((stream.codec.as_str(), video)),
            _ => None,
        });
        let mut audio_codecs = Vec::new();
        let mut default_audio = None;
        for stream in &ordered {
            if let StreamMetadata::Audio(audio) = &stream.metadata {
                if audio.is_default && default_audio.is_none() {
                    default_audio = Some(audio_codecs.len());
                }
                audio_codecs.push(normalize_label(&stream.codec));
            }
        }

        let bitrate = file_bitrate(file.size_bytes, file.duration)
            .or_else(|| video.and_then(|(_, metadata)| metadata.bit_rate));
        Self {
            container: file
                .container_format
                .as_deref()
                .map_or_else(|| UNKNOWN_LABEL.to_owned(), normalize_label),
            video_codec: video.map_or_else(
                || NO_STREAM_LABEL.to_owned(),
                |(codec, _)| normalize_label(codec),
            ),
            height_class: video.map_or(HeightClass::Unknown, |(_, metadata)| {
                height_class(metadata.height)
            }),
            bitrate_class: bitrate_class(bitrate),
            audio_codecs,
            default_audio,
        }
    }

    /// The codec of the audio track the client chose: `index` when it names
    /// one, else the default track, else the first; [`NO_STREAM_LABEL`] when
    /// the file has no audio.
    pub fn audio_codec(&self, index: Option<u32>) -> String {
        index
            .and_then(|index| self.audio_codecs.get(index as usize))
            .or_else(|| {
                self.default_audio
                    .and_then(|default| self.audio_codecs.get(default))
            })
            .or_else(|| self.audio_codecs.first())
            .map_or_else(|| NO_STREAM_LABEL.to_owned(), Clone::clone)
    }

    fn start_key(
        &self,
        client_kind: ClientKind,
        outcome: StartOutcome,
        audio: Option<u32>,
    ) -> StartKey {
        StartKey {
            client_kind,
            outcome,
            container: self.container.clone(),
            video_codec: self.video_codec.clone(),
            audio_codec: self.audio_codec(audio),
            height_class: self.height_class,
        }
    }

    fn rebuffer_key(&self, client_kind: ClientKind) -> RebufferKey {
        RebufferKey {
            client_kind,
            container: self.container.clone(),
            video_codec: self.video_codec.clone(),
            height_class: self.height_class,
            bitrate_class: self.bitrate_class,
        }
    }
}

// ── The report ──────────────────────────────────────────────────────────────

/// The range a report covers: `to` defaults to today, `from` to the
/// [`DEFAULT_REPORT_DAYS`] ending at `to`. A range that runs backwards, or
/// spans more than [`MAX_REPORT_DAYS`], is refused.
pub fn report_range(
    today: NaiveDate,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<(NaiveDate, NaiveDate), ReportError> {
    let to = to.unwrap_or(today);
    let from = match from {
        Some(from) => from,
        None => to
            .checked_sub_days(Days::new(DEFAULT_REPORT_DAYS - 1))
            .unwrap_or(NaiveDate::MIN),
    };
    if from > to {
        return Err(ReportError::InvalidDateRange(format!(
            "from ({from}) is after to ({to})"
        )));
    }
    let days = (to - from).num_days() + 1;
    if days > MAX_REPORT_DAYS as i64 {
        return Err(ReportError::InvalidDateRange(format!(
            "the range spans {days} days; at most {MAX_REPORT_DAYS}"
        )));
    }
    Ok((from, to))
}

/// The report for a summary: every row converted, totals summed, and each
/// list sorted largest count first, then by dimensions.
pub fn build_report(
    summary: PlaybackTelemetrySummary,
    config: PlaybackTelemetryConfig,
    from: NaiveDate,
    to: NaiveDate,
) -> PlaybackTelemetryReport {
    let PlaybackTelemetrySummary {
        starts: start_rows,
        rebuffers: rebuffer_rows,
        switches: switch_rows,
    } = summary;
    let mut totals = PlaybackTelemetryTotals {
        started_count: 0,
        failed_count: 0,
        rebuffer_count: 0,
        rebuffer_total_ms: 0,
        source_switch_count: 0,
    };

    let mut starts = Vec::new();
    let mut start_failures = Vec::new();
    for row in start_rows {
        let StartKey {
            client_kind,
            outcome,
            container,
            video_codec,
            audio_codec,
            height_class,
        } = row.key;
        match outcome {
            StartOutcome::Started => {
                totals.started_count += row.count;
                starts.push(PlaybackStartCount {
                    client_kind: client_kind.into(),
                    container,
                    video_codec,
                    audio_codec,
                    height_class: height_class.into(),
                    count: row.count,
                });
            }
            StartOutcome::Failed { reason, stage } => {
                totals.failed_count += row.count;
                start_failures.push(PlaybackStartFailureCount {
                    client_kind: client_kind.into(),
                    reason: reason.into(),
                    stage: stage.into(),
                    container,
                    video_codec,
                    audio_codec,
                    height_class: height_class.into(),
                    count: row.count,
                });
            }
        }
    }

    let mut rebuffers: Vec<PlaybackRebufferCount> = rebuffer_rows
        .into_iter()
        .map(|row| {
            let RebufferKey {
                client_kind,
                container,
                video_codec,
                height_class,
                bitrate_class,
            } = row.key;
            totals.rebuffer_count += row.events;
            totals.rebuffer_total_ms += row.total_ms;
            PlaybackRebufferCount {
                client_kind: client_kind.into(),
                container,
                video_codec,
                height_class: height_class.into(),
                bitrate_class: bitrate_class.into(),
                event_count: row.events,
                total_ms: row.total_ms,
                histogram: row
                    .histogram
                    .iter()
                    .map(|(bucket, count)| PlaybackRebufferBucketCount {
                        bucket: bucket.into(),
                        count,
                    })
                    .collect(),
            }
        })
        .collect();

    let mut source_switches: Vec<PlaybackSourceSwitchCount> = switch_rows
        .into_iter()
        .map(|row| {
            totals.source_switch_count += row.count;
            PlaybackSourceSwitchCount {
                client_kind: row.key.client_kind.into(),
                trigger: row.key.trigger.into(),
                from_height_class: row.key.from_height_class.into(),
                to_height_class: row.key.to_height_class.into(),
                count: row.count,
            }
        })
        .collect();

    // Largest first; ties keep the summary's key order, which a stable sort
    // preserves -- the summary arrives sorted by key.
    starts.sort_by(|a, b| b.count.cmp(&a.count));
    start_failures.sort_by(|a, b| b.count.cmp(&a.count));
    rebuffers.sort_by(|a, b| b.event_count.cmp(&a.event_count));
    source_switches.sort_by(|a, b| b.count.cmp(&a.count));

    PlaybackTelemetryReport {
        enabled: config.enabled,
        from,
        to,
        retention_days: config.retention_days,
        totals,
        starts,
        start_failures,
        rebuffers,
        source_switches,
    }
}

/// The first day whose counts are kept on `today`: `retention_days` before
/// it. Everything earlier is pruned.
pub fn retention_cutoff(today: NaiveDate, retention_days: u32) -> NaiveDate {
    today
        .checked_sub_days(Days::new(u64::from(retention_days)))
        .unwrap_or(NaiveDate::MIN)
}

/// Counts playback reports and reads them back.
#[derive(Debug)]
pub struct PlaybackTelemetryService {
    config: PlaybackTelemetryConfig,
    repo: Arc<dyn PlaybackTelemetryRepository>,
    files: Arc<dyn FileRepository>,
    streams: Arc<dyn MediaStreamRepository>,
    clock: Arc<dyn Clock>,
}

impl PlaybackTelemetryService {
    pub fn new(
        config: PlaybackTelemetryConfig,
        repo: Arc<dyn PlaybackTelemetryRepository>,
        files: Arc<dyn FileRepository>,
        streams: Arc<dyn MediaStreamRepository>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            config,
            repo,
            files,
            streams,
            clock,
        }
    }

    /// A file's dimensions, or `None` for a file no user could open -- one
    /// that is unknown, or missing from disk (a visible read, issue #179).
    async fn resolve(&self, file_id: Uuid) -> Result<Option<ResolvedFile>, DbErr> {
        let Some(file) = self.files.find_by_id(file_id).await? else {
            return Ok(None);
        };
        let streams = self.streams.find_by_file_id(file_id).await?;
        Ok(Some(ResolvedFile::of(&file, &streams)))
    }

    /// Counts a batch's events under today's UTC day.
    ///
    /// Takes no user: who reported is never known here. Refused whole when
    /// telemetry is disabled or the batch breaks a rule; otherwise every
    /// event is counted except those whose file does not resolve, which are
    /// dropped. Each file is resolved once per batch however many events name
    /// it, and the counted events are recorded in one all-or-nothing write.
    pub async fn ingest(
        &self,
        batch: PlaybackTelemetryBatch,
    ) -> Result<IngestOutcome, IngestError> {
        if !self.config.enabled {
            return Err(IngestError::Disabled);
        }
        let errors = validate_batch(&batch);
        if !errors.is_empty() {
            return Err(IngestError::Invalid(errors));
        }

        let PlaybackTelemetryBatch {
            client_kind,
            starts,
            start_failures,
            rebuffers,
            source_switches,
        } = batch;
        let client_kind = ClientKind::from(client_kind);
        let day = self.clock.now().date_naive();

        let mut resolved: HashMap<Uuid, Option<ResolvedFile>> = HashMap::new();
        let mut ids: Vec<Uuid> = Vec::new();
        ids.extend(starts.iter().map(|e| e.file_id));
        ids.extend(start_failures.iter().map(|e| e.file_id));
        ids.extend(rebuffers.iter().map(|e| e.file_id));
        for switch in &source_switches {
            ids.push(switch.from_file_id);
            ids.push(switch.to_file_id);
        }
        for id in ids {
            if let Entry::Vacant(slot) = resolved.entry(id) {
                slot.insert(self.resolve(id).await?);
            }
        }
        let file = |id: &Uuid| resolved.get(id).and_then(Option::as_ref);

        let mut outcome = IngestOutcome::default();
        let mut events: Vec<PlaybackTelemetryEvent> = Vec::new();
        for event in starts {
            match file(&event.file_id) {
                Some(file) => {
                    let key =
                        file.start_key(client_kind, StartOutcome::Started, event.audio_track_index);
                    events.push(PlaybackTelemetryEvent::Start(key));
                    outcome.recorded += 1;
                }
                None => outcome.dropped += 1,
            }
        }
        for event in start_failures {
            match file(&event.file_id) {
                Some(file) => {
                    let failed = StartOutcome::Failed {
                        reason: event.reason.into(),
                        stage: event.stage.into(),
                    };
                    let key = file.start_key(client_kind, failed, event.audio_track_index);
                    events.push(PlaybackTelemetryEvent::Start(key));
                    outcome.recorded += 1;
                }
                None => outcome.dropped += 1,
            }
        }
        for event in rebuffers {
            match file(&event.file_id) {
                Some(file) => {
                    events.push(PlaybackTelemetryEvent::Rebuffer {
                        key: file.rebuffer_key(client_kind),
                        duration_ms: event.duration_ms,
                    });
                    outcome.recorded += 1;
                }
                None => outcome.dropped += 1,
            }
        }
        for event in source_switches {
            match (file(&event.from_file_id), file(&event.to_file_id)) {
                (Some(from), Some(to)) => {
                    events.push(PlaybackTelemetryEvent::Switch(SwitchKey {
                        client_kind,
                        trigger: event.trigger.into(),
                        from_height_class: from.height_class,
                        to_height_class: to.height_class,
                    }));
                    outcome.recorded += 1;
                }
                _ => outcome.dropped += 1,
            }
        }
        // One call, all or nothing: a store failure counts none of the batch,
        // so the client's retry of the 500 cannot count any of it twice.
        self.repo.record_batch(day, &events).await?;
        Ok(outcome)
    }

    /// The counts from `from` to `to`, defaulted and bounded by
    /// [`report_range`]. Readable whether or not collection is enabled.
    pub async fn report(
        &self,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
    ) -> Result<PlaybackTelemetryReport, ReportError> {
        let (from, to) = report_range(self.clock.now().date_naive(), from, to)?;
        let summary = self.repo.summarize(from, to).await?;
        Ok(build_report(summary, self.config, from, to))
    }

    /// Deletes every count older than the retention period, returning how
    /// many day-rows went.
    pub async fn prune_expired(&self) -> Result<u64, DbErr> {
        let cutoff = retention_cutoff(self.clock.now().date_naive(), self.config.retention_days);
        self.repo.prune_before(cutoff).await
    }

    /// Prunes now and then every [`PRUNE_INTERVAL`], until the process exits.
    /// Runs whether or not collection is enabled: turning collection off
    /// stops new counts, and the old ones still age out.
    pub async fn run_retention(&self) {
        loop {
            match self.prune_expired().await {
                Ok(removed) => tracing::debug!(removed, "pruned expired playback telemetry"),
                Err(error) => tracing::warn!(%error, "could not prune playback telemetry"),
            }
            self.clock.sleep(PRUNE_INTERVAL).await;
        }
    }
}

#[cfg(test)]
#[path = "playback_telemetry_tests.rs"]
mod playback_telemetry_tests;
