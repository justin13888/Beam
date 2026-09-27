use async_trait::async_trait;
use chrono::NaiveDate;
use sea_orm::DbErr;

use crate::models::playback_telemetry::{PlaybackTelemetryEvent, PlaybackTelemetrySummary};

/// The daily playback counters (issue #143, ADR-0019).
///
/// Every write increments counters for one UTC day; there is no write that
/// stores an event. Nothing a caller can pass names a user or a file, so
/// nothing this trait keeps can be read back as a viewing record.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait PlaybackTelemetryRepository: Send + Sync + std::fmt::Debug {
    /// Counts every event of one batch on `day`, all or nothing: when this
    /// returns an error, none of the batch is counted, so a client that
    /// retries a failed report does not count any of it twice.
    ///
    /// A start adds one to its key; a rebuffer adds one event, its duration
    /// to the total, and one to the duration range it falls into; a switch
    /// adds one to its key. An empty batch counts nothing.
    async fn record_batch(
        &self,
        day: NaiveDate,
        events: &[PlaybackTelemetryEvent],
    ) -> Result<(), DbErr>;

    /// Every counter from `from` to `to`, both inclusive, summed across the
    /// days: one entry per key, each list sorted by key. An empty range
    /// (`from` after `to`) sums nothing.
    async fn summarize(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<PlaybackTelemetrySummary, DbErr>;

    /// Deletes every counter kept for a day strictly before `day`, returning
    /// how many day-rows went.
    async fn prune_before(&self, day: NaiveDate) -> Result<u64, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::models::playback_telemetry::{
        RebufferCount, RebufferHistogram, RebufferKey, StartCount, StartKey, SwitchCount, SwitchKey,
    };

    /// One day-row of rebuffer counters.
    #[derive(Debug, Clone, Copy, Default)]
    struct RebufferTotals {
        events: u64,
        total_ms: u64,
        histogram: RebufferHistogram,
    }

    /// The in-memory double: one map per table, keyed exactly as the tables'
    /// primary keys are.
    #[derive(Debug, Default)]
    pub struct InMemoryPlaybackTelemetryRepository {
        starts: Mutex<BTreeMap<(NaiveDate, StartKey), u64>>,
        rebuffers: Mutex<BTreeMap<(NaiveDate, RebufferKey), RebufferTotals>>,
        switches: Mutex<BTreeMap<(NaiveDate, SwitchKey), u64>>,
    }

    fn in_range(day: NaiveDate, from: NaiveDate, to: NaiveDate) -> bool {
        from <= day && day <= to
    }

    #[async_trait]
    impl PlaybackTelemetryRepository for InMemoryPlaybackTelemetryRepository {
        async fn record_batch(
            &self,
            day: NaiveDate,
            events: &[PlaybackTelemetryEvent],
        ) -> Result<(), DbErr> {
            let PlaybackTelemetrySummary {
                starts: start_rows,
                rebuffers: rebuffer_rows,
                switches: switch_rows,
            } = PlaybackTelemetrySummary::tally(events);
            // Every map is locked before any is written, in the order every
            // other method takes them, so a batch lands whole.
            let mut starts = self.starts.lock().unwrap();
            let mut rebuffers = self.rebuffers.lock().unwrap();
            let mut switches = self.switches.lock().unwrap();
            for row in start_rows {
                *starts.entry((day, row.key)).or_insert(0) += row.count;
            }
            for row in rebuffer_rows {
                let totals = rebuffers.entry((day, row.key)).or_default();
                totals.events += row.events;
                totals.total_ms += row.total_ms;
                totals.histogram.merge(&row.histogram);
            }
            for row in switch_rows {
                *switches.entry((day, row.key)).or_insert(0) += row.count;
            }
            Ok(())
        }

        async fn summarize(
            &self,
            from: NaiveDate,
            to: NaiveDate,
        ) -> Result<PlaybackTelemetrySummary, DbErr> {
            let mut starts: BTreeMap<StartKey, u64> = BTreeMap::new();
            for ((day, key), count) in self.starts.lock().unwrap().iter() {
                if in_range(*day, from, to) {
                    *starts.entry(key.clone()).or_insert(0) += count;
                }
            }

            let mut rebuffers: BTreeMap<RebufferKey, RebufferTotals> = BTreeMap::new();
            for ((day, key), row) in self.rebuffers.lock().unwrap().iter() {
                if in_range(*day, from, to) {
                    let totals = rebuffers.entry(key.clone()).or_default();
                    totals.events += row.events;
                    totals.total_ms += row.total_ms;
                    totals.histogram.merge(&row.histogram);
                }
            }

            let mut switches: BTreeMap<SwitchKey, u64> = BTreeMap::new();
            for ((day, key), count) in self.switches.lock().unwrap().iter() {
                if in_range(*day, from, to) {
                    *switches.entry(*key).or_insert(0) += count;
                }
            }

            Ok(PlaybackTelemetrySummary {
                starts: starts
                    .into_iter()
                    .map(|(key, count)| StartCount { key, count })
                    .collect(),
                rebuffers: rebuffers
                    .into_iter()
                    .map(|(key, totals)| RebufferCount {
                        key,
                        events: totals.events,
                        total_ms: totals.total_ms,
                        histogram: totals.histogram,
                    })
                    .collect(),
                switches: switches
                    .into_iter()
                    .map(|(key, count)| SwitchCount { key, count })
                    .collect(),
            })
        }

        async fn prune_before(&self, day: NaiveDate) -> Result<u64, DbErr> {
            let mut removed = 0u64;
            {
                let mut starts = self.starts.lock().unwrap();
                let before = starts.len();
                starts.retain(|(d, _), _| *d >= day);
                removed += (before - starts.len()) as u64;
            }
            {
                let mut rebuffers = self.rebuffers.lock().unwrap();
                let before = rebuffers.len();
                rebuffers.retain(|(d, _), _| *d >= day);
                removed += (before - rebuffers.len()) as u64;
            }
            {
                let mut switches = self.switches.lock().unwrap();
                let before = switches.len();
                switches.retain(|(d, _), _| *d >= day);
                removed += (before - switches.len()) as u64;
            }
            Ok(removed)
        }
    }

    /// The hermetic instantiation of the shared contract.
    #[derive(Debug, Default)]
    pub struct InMemoryFixture {
        repo: InMemoryPlaybackTelemetryRepository,
    }

    impl crate::repositories::contract::fixture::PlaybackTelemetryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn PlaybackTelemetryRepository {
            &self.repo
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory::InMemoryFixture {
        super::in_memory::InMemoryFixture::default()
    }

    crate::playback_telemetry_repository_contract!(setup);
}
