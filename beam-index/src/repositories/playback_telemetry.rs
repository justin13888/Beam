//! SQL implementation of [`PlaybackTelemetryRepository`] (issue #143).
//!
//! Every write is one `INSERT ... ON CONFLICT (<primary key>) DO UPDATE` that
//! increments the day's counter in place: concurrent reports for one key
//! cannot race a read-modify-write, and a day's counter is one row however
//! many reports it counts. Reads sum across days in SQL, so the report reads
//! one row per key rather than one per key per day.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use sea_orm::sea_query::{Expr, ExprTrait, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait, IdenStatic,
    QueryFilter, QueryResult, Set, Statement,
};

use beam_domain::models::playback_telemetry::{
    BitrateClass, ClientKind, HeightClass, PlaybackTelemetrySummary, RebufferBucket, RebufferCount,
    RebufferHistogram, RebufferKey, StartCount, StartKey, StartOutcome, SwitchCount, SwitchKey,
    SwitchTrigger,
};
use beam_domain::repositories::PlaybackTelemetryRepository;
use beam_domain::utils::telemetry::rebuffer_bucket;
use beam_entity::{playback_rebuffer_count, playback_start_count, playback_switch_count};

/// The rebuffer histogram's column for each bucket, in [`RebufferBucket::ALL`]
/// order. Written by `record_rebuffer`, summed by `summarize`.
fn bucket_column(bucket: RebufferBucket) -> playback_rebuffer_count::Column {
    match bucket {
        RebufferBucket::Under1Secs => playback_rebuffer_count::Column::Lt1s,
        RebufferBucket::From1To3Secs => playback_rebuffer_count::Column::S13,
        RebufferBucket::From3To10Secs => playback_rebuffer_count::Column::S310,
        RebufferBucket::From10To30Secs => playback_rebuffer_count::Column::S1030,
        RebufferBucket::AtLeast30Secs => playback_rebuffer_count::Column::Ge30s,
    }
}

const STARTS_SQL: &str = "\
SELECT client_kind, outcome, reason, stage, container, video_codec, audio_codec, height_class, \
       SUM(count)::BIGINT AS count \
FROM playback_start_counts WHERE day >= $1 AND day <= $2 \
GROUP BY client_kind, outcome, reason, stage, container, video_codec, audio_codec, height_class";

const REBUFFERS_SQL: &str = "\
SELECT client_kind, container, video_codec, height_class, bitrate_class, \
       SUM(events)::BIGINT AS events, SUM(total_ms)::BIGINT AS total_ms, \
       SUM(lt_1s)::BIGINT AS lt_1s, SUM(s1_3)::BIGINT AS s1_3, SUM(s3_10)::BIGINT AS s3_10, \
       SUM(s10_30)::BIGINT AS s10_30, SUM(ge_30s)::BIGINT AS ge_30s \
FROM playback_rebuffer_counts WHERE day >= $1 AND day <= $2 \
GROUP BY client_kind, container, video_codec, height_class, bitrate_class";

const SWITCHES_SQL: &str = "\
SELECT client_kind, trigger, from_height_class, to_height_class, SUM(count)::BIGINT AS count \
FROM playback_switch_counts WHERE day >= $1 AND day <= $2 \
GROUP BY client_kind, trigger, from_height_class, to_height_class";

/// A label column read back as its vocabulary value. A label no value has is
/// a row this build did not write, and an error rather than a guess.
fn label<T>(row: &QueryResult, column: &str, parse: fn(&str) -> Option<T>) -> Result<T, DbErr> {
    let raw: String = row.try_get("", column)?;
    parse(&raw).ok_or_else(|| DbErr::Type(format!("unexpected {column} {raw:?}")))
}

/// A non-negative sum column. Every counter is `CHECK (>= 0)`, so a negative
/// sum would be a driver fault.
fn sum(row: &QueryResult, column: &str) -> Result<u64, DbErr> {
    let value: i64 = row.try_get("", column)?;
    u64::try_from(value).map_err(|_| DbErr::Type(format!("negative {column} {value}")))
}

fn start_count(row: &QueryResult) -> Result<StartCount, DbErr> {
    let outcome: String = row.try_get("", "outcome")?;
    let reason: String = row.try_get("", "reason")?;
    let stage: String = row.try_get("", "stage")?;
    let outcome = StartOutcome::from_labels(&outcome, &reason, &stage).ok_or_else(|| {
        DbErr::Type(format!(
            "unexpected outcome {outcome:?}/{reason:?}/{stage:?}"
        ))
    })?;
    Ok(StartCount {
        key: StartKey {
            client_kind: label(row, "client_kind", ClientKind::parse)?,
            outcome,
            container: row.try_get("", "container")?,
            video_codec: row.try_get("", "video_codec")?,
            audio_codec: row.try_get("", "audio_codec")?,
            height_class: label(row, "height_class", HeightClass::parse)?,
        },
        count: sum(row, "count")?,
    })
}

fn rebuffer_count(row: &QueryResult) -> Result<RebufferCount, DbErr> {
    let mut histogram = [0u64; RebufferBucket::ALL.len()];
    for (slot, bucket) in histogram.iter_mut().zip(RebufferBucket::ALL) {
        *slot = sum(row, bucket_column(*bucket).as_str())?;
    }
    Ok(RebufferCount {
        key: RebufferKey {
            client_kind: label(row, "client_kind", ClientKind::parse)?,
            container: row.try_get("", "container")?,
            video_codec: row.try_get("", "video_codec")?,
            height_class: label(row, "height_class", HeightClass::parse)?,
            bitrate_class: label(row, "bitrate_class", BitrateClass::parse)?,
        },
        events: sum(row, "events")?,
        total_ms: sum(row, "total_ms")?,
        histogram: RebufferHistogram::from_counts(histogram),
    })
}

fn switch_count(row: &QueryResult) -> Result<SwitchCount, DbErr> {
    Ok(SwitchCount {
        key: SwitchKey {
            client_kind: label(row, "client_kind", ClientKind::parse)?,
            trigger: label(row, "trigger", SwitchTrigger::parse)?,
            from_height_class: label(row, "from_height_class", HeightClass::parse)?,
            to_height_class: label(row, "to_height_class", HeightClass::parse)?,
        },
        count: sum(row, "count")?,
    })
}

/// Sorted by key, as the trait promises -- in Rust rather than `ORDER BY`, so
/// the order is the keys' own and not the database collation's.
fn sorted_by<T, K: Ord>(mut items: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    items.sort_by_key(|item| key(item));
    items
}

/// SQL-based implementation of [`PlaybackTelemetryRepository`].
#[derive(Debug, Clone)]
pub struct SqlPlaybackTelemetryRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlPlaybackTelemetryRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    async fn sum_rows(
        &self,
        sql: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<QueryResult>, DbErr> {
        self.db
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                [from.into(), to.into()],
            ))
            .await
    }
}

#[async_trait]
impl PlaybackTelemetryRepository for SqlPlaybackTelemetryRepository {
    async fn record_start(&self, day: NaiveDate, key: StartKey) -> Result<(), DbErr> {
        use playback_start_count::{ActiveModel, Column, Entity};

        let StartKey {
            client_kind,
            outcome,
            container,
            video_codec,
            audio_codec,
            height_class,
        } = key;
        let row = ActiveModel {
            day: Set(day),
            client_kind: Set(client_kind.as_str().to_owned()),
            outcome: Set(outcome.outcome_label().to_owned()),
            reason: Set(outcome.reason_label().to_owned()),
            stage: Set(outcome.stage_label().to_owned()),
            container: Set(container),
            video_codec: Set(video_codec),
            audio_codec: Set(audio_codec),
            height_class: Set(height_class.as_str().to_owned()),
            count: Set(1),
        };
        Entity::insert(row)
            .on_conflict(
                OnConflict::columns([
                    Column::Day,
                    Column::ClientKind,
                    Column::Outcome,
                    Column::Reason,
                    Column::Stage,
                    Column::Container,
                    Column::VideoCodec,
                    Column::AudioCodec,
                    Column::HeightClass,
                ])
                .value(Column::Count, Expr::col((Entity, Column::Count)).add(1))
                .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;
        Ok(())
    }

    async fn record_rebuffer(
        &self,
        day: NaiveDate,
        key: RebufferKey,
        duration_ms: u32,
    ) -> Result<(), DbErr> {
        use playback_rebuffer_count::{ActiveModel, Column, Entity};

        let RebufferKey {
            client_kind,
            container,
            video_codec,
            height_class,
            bitrate_class,
        } = key;
        let bucket = rebuffer_bucket(duration_ms);
        let in_bucket = |candidate: RebufferBucket| Set(i64::from(candidate == bucket));
        let row = ActiveModel {
            day: Set(day),
            client_kind: Set(client_kind.as_str().to_owned()),
            container: Set(container),
            video_codec: Set(video_codec),
            height_class: Set(height_class.as_str().to_owned()),
            bitrate_class: Set(bitrate_class.as_str().to_owned()),
            events: Set(1),
            total_ms: Set(i64::from(duration_ms)),
            lt_1s: in_bucket(RebufferBucket::Under1Secs),
            s1_3: in_bucket(RebufferBucket::From1To3Secs),
            s3_10: in_bucket(RebufferBucket::From3To10Secs),
            s10_30: in_bucket(RebufferBucket::From10To30Secs),
            ge_30s: in_bucket(RebufferBucket::AtLeast30Secs),
        };
        let bucket_column = bucket_column(bucket);
        Entity::insert(row)
            .on_conflict(
                OnConflict::columns([
                    Column::Day,
                    Column::ClientKind,
                    Column::Container,
                    Column::VideoCodec,
                    Column::HeightClass,
                    Column::BitrateClass,
                ])
                .value(Column::Events, Expr::col((Entity, Column::Events)).add(1))
                .value(
                    Column::TotalMs,
                    Expr::col((Entity, Column::TotalMs)).add(i64::from(duration_ms)),
                )
                .value(bucket_column, Expr::col((Entity, bucket_column)).add(1))
                .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;
        Ok(())
    }

    async fn record_switch(&self, day: NaiveDate, key: SwitchKey) -> Result<(), DbErr> {
        use playback_switch_count::{ActiveModel, Column, Entity};

        let SwitchKey {
            client_kind,
            trigger,
            from_height_class,
            to_height_class,
        } = key;
        let row = ActiveModel {
            day: Set(day),
            client_kind: Set(client_kind.as_str().to_owned()),
            trigger: Set(trigger.as_str().to_owned()),
            from_height_class: Set(from_height_class.as_str().to_owned()),
            to_height_class: Set(to_height_class.as_str().to_owned()),
            count: Set(1),
        };
        Entity::insert(row)
            .on_conflict(
                OnConflict::columns([
                    Column::Day,
                    Column::ClientKind,
                    Column::Trigger,
                    Column::FromHeightClass,
                    Column::ToHeightClass,
                ])
                .value(Column::Count, Expr::col((Entity, Column::Count)).add(1))
                .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;
        Ok(())
    }

    async fn summarize(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<PlaybackTelemetrySummary, DbErr> {
        let starts = self
            .sum_rows(STARTS_SQL, from, to)
            .await?
            .iter()
            .map(start_count)
            .collect::<Result<Vec<_>, DbErr>>()?;
        let rebuffers = self
            .sum_rows(REBUFFERS_SQL, from, to)
            .await?
            .iter()
            .map(rebuffer_count)
            .collect::<Result<Vec<_>, DbErr>>()?;
        let switches = self
            .sum_rows(SWITCHES_SQL, from, to)
            .await?
            .iter()
            .map(switch_count)
            .collect::<Result<Vec<_>, DbErr>>()?;

        Ok(PlaybackTelemetrySummary {
            starts: sorted_by(starts, |s| s.key.clone()),
            rebuffers: sorted_by(rebuffers, |r| r.key.clone()),
            switches: sorted_by(switches, |s| s.key),
        })
    }

    async fn prune_before(&self, day: NaiveDate) -> Result<u64, DbErr> {
        let db = self.db.as_ref();
        let starts = playback_start_count::Entity::delete_many()
            .filter(playback_start_count::Column::Day.lt(day))
            .exec(db)
            .await?
            .rows_affected;
        let rebuffers = playback_rebuffer_count::Entity::delete_many()
            .filter(playback_rebuffer_count::Column::Day.lt(day))
            .exec(db)
            .await?
            .rows_affected;
        let switches = playback_switch_count::Entity::delete_many()
            .filter(playback_switch_count::Column::Day.lt(day))
            .exec(db)
            .await?
            .rows_affected;
        Ok(starts + rebuffers + switches)
    }
}
