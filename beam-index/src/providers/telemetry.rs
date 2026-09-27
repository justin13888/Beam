//! `reqwest`-backed [`TelemetrySink`] adapter (issue #93, ADR-0019).
//!
//! Lives beside the artwork fetcher: this crate owns every outbound HTTP call
//! Beam makes. The request is built by [`telemetry_request`], a pure function,
//! so what goes on the wire is asserted without a collector to send it to.

use std::time::Duration;

use beam_domain::providers::telemetry::{TelemetrySendError, TelemetrySink};
use reqwest::{Client, Request};
use tracing::warn;

/// How long one delivery may take before it counts as failed and is retried
/// on the report's backoff.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// The outbound request, headers and all: a `POST` carrying the body and its
/// content type, and nothing else -- no cookie, no authorization, no
/// identifying header (NFR-503).
pub(crate) fn telemetry_request(
    client: &Client,
    url: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Result<Request, TelemetrySendError> {
    client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(body)
        .build()
        .map_err(|err| TelemetrySendError::Transport(err.to_string()))
}

/// [`TelemetrySink`] over a `reqwest` client.
#[derive(Debug)]
pub struct ReqwestTelemetrySink {
    client: Client,
}

impl ReqwestTelemetrySink {
    /// Builds the client the report is sent with.
    ///
    /// No cookie store (the `cookies` feature is off in the manifest), and no
    /// redirects: the operator named the collector's URL, and a redirect is
    /// the collector choosing a different recipient for the report than the
    /// one the operator agreed to.
    pub fn new() -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .timeout(SEND_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Beam/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl TelemetrySink for ReqwestTelemetrySink {
    async fn post(
        &self,
        url: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<(), TelemetrySendError> {
        let request = telemetry_request(&self.client, url, content_type, body)?;
        let response = self.client.execute(request).await.map_err(|err| {
            warn!(%err, "library report delivery failed");
            TelemetrySendError::Transport(err.to_string())
        })?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(TelemetrySendError::Rejected {
                status: status.as_u16(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NFR-503: the collector learns the body and nothing else. Asserted on
    /// the request Beam would send.
    #[test]
    fn the_request_carries_the_body_and_its_content_type_only() {
        let request = telemetry_request(
            &Client::new(),
            "https://collector.example/v1/metrics",
            "application/json",
            b"{}".to_vec(),
        )
        .expect("request builds");

        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(
            request.url().as_str(),
            "https://collector.example/v1/metrics"
        );
        let names: Vec<_> = request
            .headers()
            .keys()
            .map(|name| name.as_str().to_ascii_lowercase())
            .collect();
        assert_eq!(names, vec!["content-type".to_string()]);
        assert_eq!(
            request.headers()[reqwest::header::CONTENT_TYPE],
            "application/json"
        );
        assert_eq!(
            request.body().and_then(|body| body.as_bytes()),
            Some(&b"{}"[..])
        );
    }

    #[test]
    fn an_unparseable_url_is_a_transport_error_not_a_panic() {
        assert!(matches!(
            telemetry_request(&Client::new(), "not a url", "application/json", Vec::new()),
            Err(TelemetrySendError::Transport(_))
        ));
    }
}
