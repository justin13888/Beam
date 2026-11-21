//! Metrics of every request Beam makes to a metadata provider (issue #457).
//!
//! Emitted where a request reaches one named upstream -- each of cameo's
//! TMDB and AniList clients, and TheTVDB -- rather than at the
//! [`EnrichmentProvider`](beam_domain::providers::enrichment::EnrichmentProvider)
//! seam, whose search fans out to several providers in one call and so has
//! no single provider to label it with.

use std::future::Future;

use beam_domain::providers::enrichment::EnrichmentError;
use beam_domain::services::Clock;

/// The `result` label of `beam_provider_requests_total`: `ok`, or which
/// [`EnrichmentError`] the request ended in.
pub(crate) fn result_label<T>(result: &Result<T, EnrichmentError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(EnrichmentError::NotFound) => "not_found",
        Err(EnrichmentError::RateLimited { .. }) => "rate_limited",
        Err(EnrichmentError::Transport(_)) => "transport_error",
        Err(EnrichmentError::Provider(_)) => "provider_error",
        Err(EnrichmentError::NotConfigured) => "not_configured",
    }
}

/// Run `request` to `provider`, and record it: one
/// `beam_provider_requests_total{provider,result}`, its time in
/// `beam_provider_request_duration_seconds{provider}` on `clock`'s monotonic
/// clock, and a `beam_provider_rate_limited_total{provider}` when the
/// provider refused it for its rate limit.
pub(crate) async fn observed<T>(
    provider: &str,
    clock: &dyn Clock,
    request: impl Future<Output = Result<T, EnrichmentError>>,
) -> Result<T, EnrichmentError> {
    let started = clock.monotonic();
    let result = request.await;
    let elapsed = clock.monotonic().saturating_duration_since(started);
    let provider = provider.to_owned();
    metrics::counter!(
        "beam_provider_requests_total",
        "provider" => provider.clone(),
        "result" => result_label(&result),
    )
    .increment(1);
    metrics::histogram!("beam_provider_request_duration_seconds", "provider" => provider.clone())
        .record(elapsed.as_secs_f64());
    if matches!(result, Err(EnrichmentError::RateLimited { .. })) {
        metrics::counter!("beam_provider_rate_limited_total", "provider" => provider).increment(1);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use beam_domain::services::TestClock;

    use super::*;
    use crate::test_metrics::MetricsCapture;

    /// Every outcome gets its own label, so a dashboard can tell a rate
    /// limit from an outage from a title the provider does not have.
    #[test]
    fn every_outcome_has_a_distinct_result_label() {
        let outcomes: Vec<Result<(), EnrichmentError>> = vec![
            Ok(()),
            Err(EnrichmentError::NotFound),
            Err(EnrichmentError::RateLimited { retry_after: None }),
            Err(EnrichmentError::Transport("reset".into())),
            Err(EnrichmentError::Provider("500".into())),
            Err(EnrichmentError::NotConfigured),
        ];
        let labels: std::collections::HashSet<&str> = outcomes.iter().map(result_label).collect();
        assert_eq!(labels.len(), outcomes.len());
    }

    /// A request is timed on the injected clock from when it is started to
    /// when it answers, and its answer is passed through untouched.
    #[tokio::test]
    async fn a_request_is_counted_and_timed_on_the_injected_clock() {
        let capture = MetricsCapture::new();
        let _recorder = capture.install();
        let clock = Arc::new(TestClock::new());

        let answer = observed("tvdb", &*clock, async {
            clock.advance(Duration::from_millis(250));
            Ok::<_, EnrichmentError>(7)
        })
        .await;
        assert_eq!(answer.unwrap(), 7);

        let recorded = capture.snapshot();
        assert_eq!(
            recorded.counter(
                "beam_provider_requests_total",
                &[("provider", "tvdb"), ("result", "ok")]
            ),
            Some(1)
        );
        assert_eq!(
            recorded.histogram(
                "beam_provider_request_duration_seconds",
                &[("provider", "tvdb")]
            ),
            Some(vec![0.25])
        );
        assert!(!recorded.has("beam_provider_rate_limited_total"));
    }
}
