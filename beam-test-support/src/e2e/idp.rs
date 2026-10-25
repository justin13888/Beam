//! The person at the browser: signing in at Dex.
//!
//! Both sign-in flows end with somebody typing a password into the identity
//! provider's own page. Here that is done over plain HTTP against Dex's real
//! login form -- `GET` the page, `POST` the form it serves -- so what the
//! server receives is exactly what a browser would have produced. The
//! accounts are `dex/config.yaml`'s static users.

use reqwest::header::{COOKIE, LOCATION, SET_COOKIE};
use reqwest::{StatusCode, Url};

/// Environment variable naming the issuer, when it is not the one
/// `dex/config.yaml` declares.
pub const ISSUER_ENV: &str = "BEAM_E2E_OIDC_ISSUER";

/// The issuer `dex/config.yaml` declares, and `mise run dev:up` runs.
pub const DEFAULT_ISSUER: &str = "http://dex.beam.localhost:5556/dex";

/// The client `dex/config.yaml` registers for Beam.
pub const CLIENT_ID: &str = "beam";
/// Its secret: a development value, committed in `dex/config.yaml`.
pub const CLIENT_SECRET: &str = "beam-dev-secret";

/// The most redirects one sign-in may take before it is a loop.
const MAX_HOPS: usize = 16;

/// A Dex static user.
#[derive(Debug, Clone, Copy)]
pub struct Account {
    pub email: &'static str,
    pub password: &'static str,
}

/// The static user in the `beam-admin` group.
pub const ADMIN: Account = Account {
    email: "admin@beam.localhost",
    password: "password",
};

/// The static user in no group: a genuine non-administrator.
pub const VIEWER: Account = Account {
    email: "user@beam.localhost",
    password: "password",
};

/// The issuer the server and the suite both use.
pub fn issuer() -> String {
    std::env::var(ISSUER_ENV).unwrap_or_else(|_| DEFAULT_ISSUER.to_owned())
}

/// A browser that only ever talks to Dex and to one Beam server.
#[derive(Debug, Clone)]
pub struct Browser {
    http: reqwest::Client,
    issuer: Url,
}

impl Default for Browser {
    fn default() -> Self {
        Self::new()
    }
}

impl Browser {
    pub fn new() -> Self {
        Self {
            // Redirects are followed by hand: which hops happen, and the
            // cookie each one sets, are part of what is being tested.
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("build the browser's HTTP client"),
            issuer: Url::parse(&issuer()).expect("the issuer is a URL"),
        }
    }

    /// The browser sign-in (Authorization Code + PKCE): start at
    /// `GET {server}/v1/auth/login`, sign in at Dex as `account`, land on the
    /// server's callback with the state cookie the login set, and return the
    /// `beam_session` cookie the callback sets.
    pub async fn sign_in(&self, server: &str, account: Account) -> String {
        let login = self
            .http
            .get(format!("{server}/v1/auth/login"))
            .send()
            .await
            .expect("GET /v1/auth/login");
        assert_eq!(
            login.status(),
            StatusCode::FOUND,
            "the login redirects to the identity provider"
        );
        let state = cookie(&login, "beam_oidc_state").expect("the login sets the state cookie");
        let to_idp = location(&login, &Url::parse(server).expect("the server URL"));

        let callback = self.through_dex(to_idp, account).await;
        assert!(
            callback
                .as_str()
                .starts_with(&format!("{server}/v1/auth/callback")),
            "Dex sends the browser back to the server's callback, not to {callback}"
        );

        let landed = self
            .http
            .get(callback)
            .header(COOKIE, format!("beam_oidc_state={state}"))
            .send()
            .await
            .expect("GET /v1/auth/callback");
        assert_eq!(
            landed.status(),
            StatusCode::FOUND,
            "the callback redirects into the web app"
        );
        cookie(&landed, "beam_session").expect("the callback sets the session cookie")
    }

    /// Approve a device sign-in (RFC 8628) as `account`, the way its user
    /// would: open `verification_uri_complete`, confirm the code it shows,
    /// and sign in.
    pub async fn approve_device(&self, verification_uri_complete: &str, account: Account) {
        let page_url = Url::parse(verification_uri_complete).expect("a verification URI");
        let page = self.get(page_url.clone()).await;
        let (status, body) = (page.status(), page.text().await.expect("the device page"));
        assert_eq!(status, StatusCode::OK, "Dex serves the device page");
        let action = form_action(&body).expect("the device page has a form");
        let user_code = input_value(&body, "user_code").expect("the code is filled in");

        let submitted = self
            .http
            .post(page_url.join(&action).expect("the form's action"))
            .form(&[("user_code", user_code.as_str())])
            .send()
            .await
            .expect("submit the device code");
        let next = location(&submitted, &page_url);

        let finished = self.through_dex(next, account).await;
        assert!(
            self.is_device_callback(&finished),
            "Dex finishes a device approval at its device callback, not at {finished}"
        );
        let page = self.get(finished).await;
        assert_eq!(
            page.status(),
            StatusCode::OK,
            "Dex's device callback accepts the approval"
        );
    }

    /// Follow Dex's redirects from `start`, submitting `account` to the login
    /// form when it is served, until Dex sends the browser somewhere that is
    /// not Dex, or to its own device callback. Returns that address.
    async fn through_dex(&self, start: Url, account: Account) -> Url {
        let mut at = start;
        for _ in 0..MAX_HOPS {
            at = self.on_issuer(at);
            if !self.is_dex(&at) || self.is_device_callback(&at) {
                return at;
            }
            let response = self.get(at.clone()).await;
            match response.status() {
                status if status.is_redirection() => at = location(&response, &at),
                StatusCode::OK => {
                    let body = response.text().await.expect("a Dex page");
                    let action = form_action(&body)
                        .unwrap_or_else(|| panic!("Dex served {at} with no form to submit"));
                    let posted = self
                        .http
                        .post(at.join(&action).expect("the form's action"))
                        .form(&[("login", account.email), ("password", account.password)])
                        .send()
                        .await
                        .expect("submit the login form");
                    assert!(
                        posted.status().is_redirection(),
                        "Dex accepts {}'s password (answered {})",
                        account.email,
                        posted.status()
                    );
                    at = location(&posted, &at);
                }
                status => panic!("Dex answered {at} with {status}"),
            }
        }
        panic!("gave up after {MAX_HOPS} redirects inside Dex");
    }

    async fn get(&self, url: Url) -> reqwest::Response {
        self.http
            .get(url.clone())
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {url}: {e}"))
    }

    fn is_dex(&self, url: &Url) -> bool {
        url.origin() == self.issuer.origin() && url.path().starts_with(self.issuer.path())
    }

    fn device_callback_path(&self) -> String {
        format!(
            "{}/device/callback",
            self.issuer.path().trim_end_matches('/')
        )
    }

    fn is_device_callback(&self, url: &Url) -> bool {
        self.is_dex(url) && url.path() == self.device_callback_path()
    }

    /// `url`, with the issuer's path put back where Dex dropped it.
    ///
    /// Dex's device approval redirects to `/device/callback` without the
    /// issuer's `/dex` prefix when the issuer is on a sub-path
    /// (dexidp/dex#4571, documented in `dex/config.yaml`). A person at a
    /// browser edits the address bar once; this is that edit, and it is a
    /// no-op once Dex is fixed.
    fn on_issuer(&self, url: Url) -> Url {
        if url.origin() != self.issuer.origin() || url.path() != "/device/callback" {
            return url;
        }
        let mut fixed = url;
        fixed.set_path(&self.device_callback_path());
        fixed
    }
}

/// Where `response` redirects to, resolved against the URL it answered.
fn location(response: &reqwest::Response, base: &Url) -> Url {
    let raw = response
        .headers()
        .get(LOCATION)
        .unwrap_or_else(|| panic!("{} redirects with no Location", response.url()))
        .to_str()
        .expect("a printable Location");
    base.join(raw).expect("a resolvable Location")
}

/// The value of the cookie `name` that `response` sets, if it sets one.
pub fn cookie(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .find_map(|header| {
            let header = header.to_str().ok()?;
            let pair = header.split(';').next()?;
            let (key, value) = pair.split_once('=')?;
            (key.trim() == name).then(|| value.trim().to_owned())
        })
}

/// The `action` of the first form in `html`, with its entities decoded.
fn form_action(html: &str) -> Option<String> {
    let form = &html[html.find("<form")?..];
    attribute(form, "action")
}

/// The `value` of the input named `name` in `html`.
fn input_value(html: &str, name: &str) -> Option<String> {
    let at = html.find(&format!(r#"name="{name}""#))?;
    let start = html[..at].rfind("<input")?;
    let end = at + html[at..].find('>')?;
    attribute(&html[start..end], "value")
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let marker = format!(r#"{name}=""#);
    let start = tag.find(&marker)? + marker.len();
    let end = start + tag[start..].find('"')?;
    Some(tag[start..end].replace("&amp;", "&"))
}
