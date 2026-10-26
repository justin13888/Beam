//! The wire client the suite drives the server with.
//!
//! The generated client in `beam-client-core` -- the one the Android app
//! links -- rather than hand-written requests, so the suite exercises the
//! contract as a native client consumes it: a response the document does not
//! describe fails to decode here exactly as it would on a phone.
//!
//! Only what the generated client deliberately omits (the byte-range media
//! endpoints, which a platform player fetches itself) is requested raw, with
//! the same session cookie.

pub use beam_client_core::api;

/// The cookie the server issues and reads.
pub const SESSION_COOKIE: &str = beam_client_core::transport::SESSION_COOKIE;

/// The security scheme `api/openapi.json` declares for that cookie.
const SESSION_SCHEME: &str = "BeamSession";

/// A client of `base_url` presenting no session.
pub fn anonymous(base_url: &str) -> api::Client {
    beam_client_core::install_crypto_provider();
    api::Client::new(base_url).expect("a client for the server")
}

/// A client of `base_url` presenting `session` as its `beam_session` cookie.
pub fn signed_in(base_url: &str, session: &str) -> api::Client {
    anonymous(base_url).with_credential(
        SESSION_SCHEME,
        api::Credential::ApiKey(session.to_owned().into()),
    )
}

/// The HTTP status a failed call was answered with, if it was answered.
pub fn status_of<E>(error: &api::Error<E>) -> Option<u16> {
    match error {
        api::Error::Api(value) => Some(value.status().as_u16()),
        api::Error::UnexpectedStatus { status, .. } => Some(status.as_u16()),
        _ => None,
    }
}

/// The status `result` failed with.
///
/// # Panics
///
/// When it succeeded, or failed without a response.
pub fn refused<T: std::fmt::Debug, E: std::fmt::Debug>(
    what: &str,
    result: Result<T, api::Error<E>>,
) -> u16 {
    match result {
        Ok(value) => panic!("{what} was expected to be refused, and answered {value:?}"),
        Err(error) => status_of(&error)
            .unwrap_or_else(|| panic!("{what} failed without a response: {error:?}")),
    }
}
