//! `reqwest`-backed [`TelemetrySink`] adapter (issue #93, ADR-0019).
//!
//! Lives beside the artwork fetcher: this crate owns every outbound HTTP call
//! Beam makes. The request is built by [`telemetry_request`], a pure function,
//! so what goes on the wire is asserted without a collector to send it to.

use std::time::Duration;

use std::error::Error as _;

use beam_domain::providers::telemetry::{TelemetrySendError, TelemetrySink};
use reqwest::{Client, Request};

/// How long one delivery may take before it counts as failed and is retried
/// on the report's backoff.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// A `reqwest` error as it may be logged or kept: without the URL, with the
/// causes that say why.
///
/// `reqwest::Error`'s `Display` ends `for url (<the full url>)`, and the
/// collector URL's path, query and userinfo are where an ingest token lives
/// (ADR-0019). Only the origin is ever shown, so every error leaving this
/// adapter passes through here first.
fn describe(err: reqwest::Error) -> String {
    let err = err.without_url();
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// The outbound request, headers and all: a `POST` carrying the body and its
/// content type -- no cookie and no identifying header (NFR-503).
///
/// The one other header is the operator's own: userinfo in the URL
/// (`https://user:secret@collector.example/...`) is sent as a Basic
/// `Authorization` header, which is how a collector that wants credentials is
/// given them -- alongside a token in the query. Never a Beam credential.
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
        .map_err(|err| TelemetrySendError::Transport(describe(err)))
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
        // Not logged here: the schedule logs the failure once, with its retry.
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|err| TelemetrySendError::Transport(describe(err)))?;
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

    /// The pieces of a collector URL that may carry a credential, each
    /// distinct enough that finding it in a message can only be a leak.
    const USER: &str = "report-user";
    const PASSWORD: &str = "basic-secret";
    const PATH: &str = "ingest-path";
    const TOKEN: &str = "query-secret";

    fn assert_no_url_secret(message: &str) {
        for secret in [USER, PASSWORD, PATH, TOKEN] {
            assert!(
                !message.contains(secret),
                "{secret} leaked into {message:?}"
            );
        }
    }

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

    /// Userinfo is the operator's way to give a collector credentials: it
    /// leaves the URL and becomes a Basic `Authorization` header, marked
    /// sensitive so reqwest never prints it.
    #[test]
    fn userinfo_in_the_url_is_sent_as_basic_authorization() {
        let request = telemetry_request(
            &Client::new(),
            &format!("https://{USER}:{PASSWORD}@collector.example/v1/metrics"),
            "application/json",
            Vec::new(),
        )
        .expect("request builds");

        assert_eq!(
            request.url().as_str(),
            "https://collector.example/v1/metrics"
        );
        let authorization = &request.headers()[reqwest::header::AUTHORIZATION];
        // base64("report-user:basic-secret"), computed outside reqwest.
        assert_eq!(
            authorization.to_str().unwrap(),
            "Basic cmVwb3J0LXVzZXI6YmFzaWMtc2VjcmV0"
        );
        assert!(authorization.is_sensitive());
    }

    #[test]
    fn an_unparseable_url_is_a_transport_error_not_a_panic() {
        assert!(matches!(
            telemetry_request(&Client::new(), "not a url", "application/json", Vec::new()),
            Err(TelemetrySendError::Transport(_))
        ));
    }

    /// A URL that parses but that reqwest refuses to build a request for
    /// (no host) is reported with the URL in reqwest's own message; the
    /// error Beam keeps must not have it.
    #[test]
    fn a_builder_error_does_not_carry_the_url() {
        let url = format!("mailto:{USER}@{PATH}?token={TOKEN}");

        let Err(TelemetrySendError::Transport(message)) =
            telemetry_request(&Client::new(), &url, "application/json", Vec::new())
        else {
            panic!("a host-less URL must not build");
        };

        assert!(!message.is_empty());
        assert_no_url_secret(&message);
    }

    /// A port on this host that nothing listens on: bound, then released.
    fn closed_local_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        drop(listener);
        port
    }

    /// D1: a refused connection is where reqwest appends `for url (...)`. The
    /// error the schedule logs names why the delivery failed and nothing of
    /// the path, query or userinfo.
    #[tokio::test]
    async fn a_transport_error_does_not_carry_the_url() {
        let sink = ReqwestTelemetrySink::new().expect("client builds");
        let url = format!(
            "http://{USER}:{PASSWORD}@127.0.0.1:{}/{PATH}/v1/metrics?token={TOKEN}",
            closed_local_port()
        );

        let Err(TelemetrySendError::Transport(message)) =
            sink.post(&url, "application/json", b"{}".to_vec()).await
        else {
            panic!("nothing listens on the port");
        };

        assert!(
            message.contains("error sending request"),
            "the failure is still described: {message:?}"
        );
        assert_no_url_secret(&message);
        assert_no_url_secret(&TelemetrySendError::Transport(message).to_string());
    }

    /// A scheme reqwest cannot send over fails at send time, again with the
    /// URL in reqwest's message.
    #[tokio::test]
    async fn an_unsendable_scheme_does_not_carry_the_url() {
        let sink = ReqwestTelemetrySink::new().expect("client builds");
        let url = format!("ftp://{USER}:{PASSWORD}@127.0.0.1/{PATH}?token={TOKEN}");

        let Err(TelemetrySendError::Transport(message)) =
            sink.post(&url, "application/json", Vec::new()).await
        else {
            panic!("ftp is not sent");
        };

        assert_no_url_secret(&message);
    }
}
