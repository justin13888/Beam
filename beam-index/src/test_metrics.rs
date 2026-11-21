//! A thread-local metrics recorder for tests (issue #457).
//!
//! Never a global install, which would collide across parallel tests: a
//! [`MetricsCapture`] is made current on the test's own thread only, so a
//! test on a current-thread runtime sees every sample its code emits and
//! nothing another test does.
//!
//! Unlike `metrics_util`'s `DebuggingRecorder`, whose snapshot zeroes every
//! counter and gauge it reads, reading this one changes nothing: a counter
//! reads as its total so far and a gauge as its last value, however often a
//! test looks.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use metrics::{
    Counter, Gauge, Histogram, Key, KeyName, LocalRecorderGuard, Metadata, Recorder, SharedString,
    Unit,
};
use metrics_util::registry::{AtomicStorage, Registry};

/// A local recorder, and what it has recorded.
pub(crate) struct MetricsCapture {
    recorder: CaptureRecorder,
}

struct CaptureRecorder {
    registry: Arc<Registry<Key, AtomicStorage>>,
}

impl Recorder for CaptureRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        self.registry
            .get_or_create_counter(key, |counter| Counter::from_arc(counter.clone()))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        self.registry
            .get_or_create_gauge(key, |gauge| Gauge::from_arc(gauge.clone()))
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        self.registry
            .get_or_create_histogram(key, |histogram| Histogram::from_arc(histogram.clone()))
    }
}

/// Whether `key` is `name` with exactly `labels`, in any order.
fn matches(key: &Key, name: &str, labels: &[(&str, &str)]) -> bool {
    if key.name() != name {
        return false;
    }
    let mut wanted: Vec<(&str, &str)> = labels.to_vec();
    wanted.sort_unstable();
    let mut have: Vec<(&str, &str)> = key
        .labels()
        .map(|label| (label.key(), label.value()))
        .collect();
    have.sort_unstable();
    have == wanted
}

impl MetricsCapture {
    pub(crate) fn new() -> Self {
        Self {
            recorder: CaptureRecorder {
                registry: Arc::new(Registry::atomic()),
            },
        }
    }

    /// Make this the current thread's recorder until the guard is dropped.
    pub(crate) fn install(&self) -> LocalRecorderGuard<'_> {
        metrics::set_default_local_recorder(&self.recorder)
    }

    /// What has been recorded so far. Reading it changes nothing.
    pub(crate) fn snapshot(&self) -> &Self {
        self
    }

    fn registry(&self) -> &Registry<Key, AtomicStorage> {
        &self.recorder.registry
    }

    /// The total of the counter `name` with exactly `labels`, if it was
    /// ever touched.
    pub(crate) fn counter(&self, name: &str, labels: &[(&str, &str)]) -> Option<u64> {
        self.registry()
            .get_counter_handles()
            .into_iter()
            .find(|(key, _)| matches(key, name, labels))
            .map(|(_, counter)| counter.load(Ordering::SeqCst))
    }

    /// The last value of the gauge `name` with exactly `labels`, if it was
    /// ever set.
    pub(crate) fn gauge(&self, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
        self.registry()
            .get_gauge_handles()
            .into_iter()
            .find(|(key, _)| matches(key, name, labels))
            .map(|(_, gauge)| f64::from_bits(gauge.load(Ordering::SeqCst)))
    }

    /// Every sample of the histogram `name` with exactly `labels`, if it
    /// was ever recorded.
    pub(crate) fn histogram(&self, name: &str, labels: &[(&str, &str)]) -> Option<Vec<f64>> {
        self.registry()
            .get_histogram_handles()
            .into_iter()
            .find(|(key, _)| matches(key, name, labels))
            .map(|(_, histogram)| histogram.data())
    }

    /// Whether any metric named `name` was recorded, whatever its labels.
    pub(crate) fn has(&self, name: &str) -> bool {
        let registry = self.registry();
        registry
            .get_counter_handles()
            .keys()
            .chain(registry.get_gauge_handles().keys())
            .chain(registry.get_histogram_handles().keys())
            .any(|key| key.name() == name)
    }
}
