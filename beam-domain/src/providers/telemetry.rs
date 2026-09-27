//! Sending the anonymous library report to an operator-chosen collector
//! (issue #93, ADR-0019).
//!
//! The one outbound call the report makes, kept behind a trait so everything
//! above it -- building the report, encoding it, the weekly schedule -- runs
//! with no network. The reqwest-backed adapter lives in beam-index beside the
//! artwork fetcher.

use thiserror::Error;

/// Why a report was not delivered.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TelemetrySendError {
    /// The collector answered, but not with a success.
    #[error("collector returned status {status}")]
    Rejected { status: u16 },
    /// Nothing usable came back: a refused connection, a timeout, TLS.
    #[error("transport error: {0}")]
    Transport(String),
}

/// Delivers one encoded report.
///
/// Implementations must attach no Beam credential, cookie or identifying
/// header to the request (NFR-502, NFR-503): the body is the whole of what the
/// collector learns.
#[async_trait::async_trait]
pub trait TelemetrySink: Send + Sync + std::fmt::Debug {
    /// POST `body`, labelled `content_type`, to `url`.
    async fn post(
        &self,
        url: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<(), TelemetrySendError>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use std::sync::Mutex;

    use super::*;

    /// One request a [`RecordingTelemetrySink`] received.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SentReport {
        pub url: String,
        pub content_type: String,
        pub body: Vec<u8>,
    }

    /// Network-free sink: records every request, and answers each with the
    /// next queued result (success once the queue is empty).
    #[derive(Debug, Default)]
    pub struct RecordingTelemetrySink {
        sent: Mutex<Vec<SentReport>>,
        results: Mutex<std::collections::VecDeque<Result<(), TelemetrySendError>>>,
    }

    impl RecordingTelemetrySink {
        pub fn new() -> Self {
            Self::default()
        }

        /// Answers the next request with `error`.
        pub fn failing_next(self, error: TelemetrySendError) -> Self {
            self.results.lock().unwrap().push_back(Err(error));
            self
        }

        /// Every request received, in order.
        pub fn sent(&self) -> Vec<SentReport> {
            self.sent.lock().unwrap().clone()
        }

        /// How many requests were received.
        pub fn sent_count(&self) -> usize {
            self.sent.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl TelemetrySink for RecordingTelemetrySink {
        async fn post(
            &self,
            url: &str,
            content_type: &str,
            body: Vec<u8>,
        ) -> Result<(), TelemetrySendError> {
            self.sent.lock().unwrap().push(SentReport {
                url: url.to_string(),
                content_type: content_type.to_string(),
                body,
            });
            self.results.lock().unwrap().pop_front().unwrap_or(Ok(()))
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use test_utils::{RecordingTelemetrySink, SentReport};
