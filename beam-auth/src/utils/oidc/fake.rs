use super::{BeginAuth, DeviceAuthStart, DevicePoll, OidcClient, OidcError, OidcIdentity};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Debug)]
pub struct FakeOidcClient {
    last_begin: Mutex<Option<BeginAuth>>,
    response: Mutex<Result<OidcIdentity, String>>,
    begin_counter: Mutex<u64>,
    /// `false` models an IdP whose discovery names no device endpoint.
    device_flow: bool,
    /// Answers for successive device polls, front first. Once empty,
    /// every poll is `Pending` -- the user simply has not decided.
    device_script: Mutex<VecDeque<Result<DevicePoll, OidcError>>>,
    /// `(expires_in_secs, interval_secs)` every device authorization
    /// grants.
    device_grant: (u64, u64),
    device_polls: Mutex<u64>,
    device_begins: Mutex<u64>,
    /// Every device code the server polled with, in order.
    polled_codes: Mutex<Vec<String>>,
}

impl Default for FakeOidcClient {
    fn default() -> Self {
        Self {
            last_begin: Mutex::new(None),
            response: Mutex::new(Err("no identity configured".to_string())),
            begin_counter: Mutex::new(0),
            device_flow: true,
            device_script: Mutex::new(VecDeque::new()),
            device_grant: (FAKE_DEVICE_EXPIRES_IN_SECS, FAKE_DEVICE_INTERVAL_SECS),
            device_polls: Mutex::new(0),
            device_begins: Mutex::new(0),
            polled_codes: Mutex::new(Vec::new()),
        }
    }
}

/// The user code every fake device authorization shows.
pub const FAKE_USER_CODE: &str = "BCDF-GHJK";
/// The interval every fake device authorization asks for.
pub const FAKE_DEVICE_INTERVAL_SECS: u64 = 5;
/// The lifetime every fake device authorization grants.
pub const FAKE_DEVICE_EXPIRES_IN_SECS: u64 = 600;

impl FakeOidcClient {
    /// Configures the identity `exchange_code` returns on a successful,
    /// well-formed exchange.
    pub fn with_identity(identity: OidcIdentity) -> Self {
        let client = Self::default();
        *client.response.lock().unwrap() = Ok(identity);
        client
    }

    /// Configures `exchange_code` to fail as if the IdP itself rejected
    /// the exchange (e.g. expired code, IdP outage).
    pub fn with_exchange_error(message: impl Into<String>) -> Self {
        let client = Self::default();
        *client.response.lock().unwrap() = Err(message.into());
        client
    }

    /// The most recent state/nonce/PKCE verifier minted by `begin_auth`,
    /// for tests that need to simulate a caller presenting the "right"
    /// values back (or deliberately tampered ones).
    pub fn last_begin(&self) -> Option<BeginAuth> {
        self.last_begin.lock().unwrap().clone()
    }

    /// Scripts what successive device-token polls answer, errors
    /// included: `OidcError::Exchange` for an IdP that could not be
    /// asked, `MissingIdToken`/`ClaimsVerification` for an answer that
    /// did not verify. Polls past the end of the script are `Pending`.
    #[must_use]
    pub fn with_device_script(self, script: Vec<Result<DevicePoll, OidcError>>) -> Self {
        *self.device_script.lock().unwrap() = script.into();
        self
    }

    /// Makes every device authorization grant this lifetime and poll
    /// interval, in seconds, in place of the defaults.
    #[must_use]
    pub fn with_device_grant(mut self, expires_in_secs: u64, interval_secs: u64) -> Self {
        self.device_grant = (expires_in_secs, interval_secs);
        self
    }

    /// An IdP that does not offer the device authorization grant.
    #[must_use]
    pub fn without_device_flow(mut self) -> Self {
        self.device_flow = false;
        self
    }

    /// How many times the token endpoint was polled with a device code.
    pub fn device_poll_count(&self) -> u64 {
        *self.device_polls.lock().unwrap()
    }

    /// Every device code the token endpoint was polled with, in order.
    pub fn polled_device_codes(&self) -> Vec<String> {
        self.polled_codes.lock().unwrap().clone()
    }
}

#[async_trait]
impl OidcClient for FakeOidcClient {
    fn begin_auth(&self) -> Result<BeginAuth, OidcError> {
        let mut counter = self.begin_counter.lock().unwrap();
        *counter += 1;
        let begin = BeginAuth {
            auth_url: format!("https://fake-idp.test/authorize?n={counter}"),
            state: format!("fake-state-{counter}"),
            nonce: format!("fake-nonce-{counter}"),
            pkce_verifier: format!("fake-verifier-{counter}"),
        };
        *self.last_begin.lock().unwrap() = Some(begin.clone());
        Ok(begin)
    }

    async fn exchange_code(
        &self,
        _code: &str,
        pkce_verifier: &str,
        nonce: &str,
    ) -> Result<OidcIdentity, OidcError> {
        if let Some(begin) = self.last_begin.lock().unwrap().as_ref() {
            if begin.nonce != nonce {
                // What `DiscoveredOidcClient` answers: openidconnect's
                // verifier reports a wrong nonce as a claims failure like
                // any other, so `NonceMismatch` never comes from it.
                return Err(OidcError::ClaimsVerification(
                    "nonce does not match".to_string(),
                ));
            }
            if begin.pkce_verifier != pkce_verifier {
                return Err(OidcError::Exchange(
                    "pkce verifier does not match".to_string(),
                ));
            }
        }

        self.response
            .lock()
            .unwrap()
            .clone()
            .map_err(OidcError::Exchange)
    }

    async fn begin_device_auth(&self) -> Result<DeviceAuthStart, OidcError> {
        if !self.device_flow {
            return Err(OidcError::DeviceFlowUnsupported);
        }
        let mut begins = self.device_begins.lock().unwrap();
        *begins += 1;
        Ok(DeviceAuthStart {
            device_code: format!("fake-device-code-{begins}"),
            user_code: FAKE_USER_CODE.to_owned(),
            verification_uri: "https://fake-idp.test/device".to_owned(),
            verification_uri_complete: Some(format!(
                "https://fake-idp.test/device?user_code={FAKE_USER_CODE}"
            )),
            expires_in_secs: self.device_grant.0,
            interval_secs: self.device_grant.1,
        })
    }

    async fn poll_device_token(&self, device_code: &str) -> Result<DevicePoll, OidcError> {
        if !self.device_flow {
            return Err(OidcError::DeviceFlowUnsupported);
        }
        *self.device_polls.lock().unwrap() += 1;
        self.polled_codes
            .lock()
            .unwrap()
            .push(device_code.to_owned());
        self.device_script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(DevicePoll::Pending))
    }
}
