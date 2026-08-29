//! The authentication state machine.
//!
//! The core never speaks OIDC. `beam-server` is a confidential client in the
//! backend-for-frontend pattern ([ADR-0003]) and FR-107 is explicit that a
//! client must not invoke an OIDC library of its own, so the whole exchange
//! happens server-side and the client's only credential is the opaque
//! `beam_session` cookie.
//!
//! Getting that cookie onto a phone is the awkward part. `sanitize_redirect_path`
//! in `beam-auth` accepts only same-origin relative paths, so the server
//! cannot be asked to redirect to a custom scheme, and there is no native
//! token endpoint. The flow therefore runs in an in-app browser and the
//! cookie is lifted from its jar -- the foreign side's job. Everything after
//! that point is here, and is a pure state machine so it can be tested
//! exhaustively without a network or a WebView.
//!
//! [ADR-0003]: ../../../docs/architecture/decisions/ADR-0003-oidc-bff-auth.md

use crate::trust::CertificateDetails;

/// Who is signed in to one server.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UserSummary {
    /// The user's identifier.
    pub id: String,
    /// Display name, as the identity provider supplied it.
    pub display_name: String,
    /// Email, where the provider supplied one.
    pub email: Option<String>,
    /// Whether this user holds the admin role. Resolved by the server from an
    /// ID-token claim on every login and never settable by the client.
    pub is_admin: bool,
    /// Avatar URL, where the provider supplied one.
    pub avatar_url: Option<String>,
}

/// The authentication state of one server.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SessionState {
    /// No credential, and none being obtained.
    LoggedOut,
    /// The in-app browser is open and the flow is in progress.
    AwaitingLogin,
    /// A cookie was captured and the server confirmed it.
    Authenticated {
        /// Who the server says this is.
        user: UserSummary,
    },
    /// A credential existed and the server has since rejected it.
    Expired,
    /// The server's certificate is not trusted and the user has not decided.
    Untrusted {
        /// What to show in the trust prompt.
        details: CertificateDetails,
    },
}

/// Something that happened to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// The user began signing in.
    LoginStarted,
    /// A `beam_session` cookie was lifted from the in-app browser.
    CookieCaptured,
    /// `GET /v1/me` confirmed the cookie.
    IdentityConfirmed(Box<UserSummary>),
    /// `GET /v1/me` rejected the cookie.
    IdentityRejected,
    /// Any request observed a 401 with a session in place.
    UnauthorizedObserved,
    /// A handshake failed certificate verification.
    CertificateRejected(Box<CertificateDetails>),
    /// The user accepted a certificate.
    CertificateTrusted,
    /// The user signed out.
    LogoutRequested,
    /// A stored cookie was found at startup.
    StoredSessionRestored(Box<UserSummary>),
}

/// A side effect the caller must perform after a transition.
///
/// Returned rather than performed so the machine stays pure: every transition
/// is a value comparison in a test, with no storage or network involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEffect {
    /// Write the captured cookie to secret storage.
    PersistCookie,
    /// Delete the stored cookie.
    ClearCookie,
    /// Install the cookie into the transport, or remove it when `None`.
    InstallCookie(bool),
    /// Call `GET /v1/me`.
    VerifyIdentity,
    /// Best-effort `POST /v1/logout`.
    RevokeRemoteSession,
    /// Tell the foreign side the state changed.
    NotifyObserver,
}

/// Apply an event to a state.
///
/// Total and side-effect free: every state/event pair has an answer, and an
/// event that does not apply leaves the state untouched rather than panicking
/// -- a 401 racing a sign-out is ordinary, not exceptional.
#[must_use]
pub fn transition(state: &SessionState, event: SessionEvent) -> (SessionState, Vec<SessionEffect>) {
    use SessionEffect as Effect;
    use SessionEvent as Event;
    use SessionState as State;

    match (state, event) {
        // Starting a login is valid from anywhere: the user may be retrying
        // after an expiry, a rejection, or a certificate refusal.
        (_, Event::LoginStarted) => (State::AwaitingLogin, vec![Effect::NotifyObserver]),

        (State::AwaitingLogin, Event::CookieCaptured) => (
            State::AwaitingLogin,
            vec![Effect::InstallCookie(true), Effect::VerifyIdentity],
        ),

        (_, Event::IdentityConfirmed(user)) => (
            State::Authenticated { user: *user },
            vec![Effect::PersistCookie, Effect::NotifyObserver],
        ),

        // A cookie the server will not accept is worse than no cookie: it
        // would make every later request fail in a way the user cannot see.
        (_, Event::IdentityRejected) => (
            State::LoggedOut,
            vec![
                Effect::InstallCookie(false),
                Effect::ClearCookie,
                Effect::NotifyObserver,
            ],
        ),

        // Only meaningful while authenticated. Observing a 401 when already
        // logged out is a late response from a cancelled request.
        (State::Authenticated { .. }, Event::UnauthorizedObserved) => (
            State::Expired,
            vec![
                Effect::InstallCookie(false),
                Effect::ClearCookie,
                Effect::NotifyObserver,
            ],
        ),

        (_, Event::CertificateRejected(details)) => (
            State::Untrusted { details: *details },
            vec![Effect::NotifyObserver],
        ),

        // Accepting a certificate resumes whatever was interrupted, which is
        // a fresh login attempt rather than an assumed-good session.
        (State::Untrusted { .. }, Event::CertificateTrusted) => {
            (State::LoggedOut, vec![Effect::NotifyObserver])
        }

        (_, Event::LogoutRequested) => (
            State::LoggedOut,
            vec![
                Effect::RevokeRemoteSession,
                Effect::InstallCookie(false),
                Effect::ClearCookie,
                Effect::NotifyObserver,
            ],
        ),

        // Restoring optimistically avoids a blocking /me on every cold start;
        // the first real request either confirms it or drives Expired.
        (State::LoggedOut | State::Expired, Event::StoredSessionRestored(user)) => (
            State::Authenticated { user: *user },
            vec![Effect::InstallCookie(true), Effect::NotifyObserver],
        ),

        // Anything else is a stale or out-of-order event; ignore it.
        (current, _) => (current.clone(), Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> UserSummary {
        UserSummary {
            id: "u1".to_owned(),
            display_name: "Viewer".to_owned(),
            email: Some("viewer@beam.localhost".to_owned()),
            is_admin: false,
            avatar_url: None,
        }
    }

    fn certificate() -> CertificateDetails {
        CertificateDetails {
            sha256_fingerprint: "AA:BB".to_owned(),
            spki_sha256_base64: "c3BraQ==".to_owned(),
            subject: "CN=beam.local".to_owned(),
            issuer: "CN=beam.local".to_owned(),
            not_before_unix: 0,
            not_after_unix: i64::MAX,
            subject_alt_names: vec!["beam.local".to_owned()],
            serial_hex: "01".to_owned(),
            is_self_signed: true,
            is_expired: false,
        }
    }

    #[test]
    fn the_happy_path_runs_logged_out_to_authenticated() {
        let (state, _) = transition(&SessionState::LoggedOut, SessionEvent::LoginStarted);
        assert_eq!(state, SessionState::AwaitingLogin);

        let (state, effects) = transition(&state, SessionEvent::CookieCaptured);
        assert_eq!(state, SessionState::AwaitingLogin);
        assert!(effects.contains(&SessionEffect::VerifyIdentity));
        assert!(effects.contains(&SessionEffect::InstallCookie(true)));

        let (state, effects) =
            transition(&state, SessionEvent::IdentityConfirmed(Box::new(user())));
        assert_eq!(state, SessionState::Authenticated { user: user() });
        assert!(
            effects.contains(&SessionEffect::PersistCookie),
            "the cookie is only persisted once the server has confirmed it"
        );
    }

    #[test]
    fn a_cookie_the_server_rejects_is_discarded_rather_than_kept() {
        // Keeping it would make every later request fail invisibly.
        let (state, effects) =
            transition(&SessionState::AwaitingLogin, SessionEvent::IdentityRejected);
        assert_eq!(state, SessionState::LoggedOut);
        assert!(effects.contains(&SessionEffect::ClearCookie));
        assert!(effects.contains(&SessionEffect::InstallCookie(false)));
    }

    #[test]
    fn a_401_while_authenticated_expires_the_session() {
        let authenticated = SessionState::Authenticated { user: user() };
        let (state, effects) = transition(&authenticated, SessionEvent::UnauthorizedObserved);
        assert_eq!(state, SessionState::Expired);
        assert!(effects.contains(&SessionEffect::ClearCookie));
        assert!(effects.contains(&SessionEffect::NotifyObserver));
    }

    #[test]
    fn a_401_when_already_logged_out_changes_nothing() {
        // A late response from a request cancelled during sign-out is
        // ordinary, and must not clobber a sign-in already in progress.
        let (state, effects) = transition(
            &SessionState::AwaitingLogin,
            SessionEvent::UnauthorizedObserved,
        );
        assert_eq!(state, SessionState::AwaitingLogin);
        assert!(effects.is_empty());
    }

    #[test]
    fn logging_out_revokes_remotely_and_clears_locally() {
        let authenticated = SessionState::Authenticated { user: user() };
        let (state, effects) = transition(&authenticated, SessionEvent::LogoutRequested);
        assert_eq!(state, SessionState::LoggedOut);
        assert!(effects.contains(&SessionEffect::RevokeRemoteSession));
        assert!(effects.contains(&SessionEffect::ClearCookie));
    }

    #[test]
    fn local_state_is_cleared_even_if_the_remote_revoke_is_only_best_effort() {
        // The effect is requested, but the state has already moved: a user who
        // taps sign out with no signal is still signed out on this device.
        let (state, _) = transition(
            &SessionState::Authenticated { user: user() },
            SessionEvent::LogoutRequested,
        );
        assert_eq!(state, SessionState::LoggedOut);
    }

    #[test]
    fn an_untrusted_certificate_is_reachable_from_any_state() {
        for start in [
            SessionState::LoggedOut,
            SessionState::AwaitingLogin,
            SessionState::Authenticated { user: user() },
            SessionState::Expired,
        ] {
            let (state, _) = transition(
                &start,
                SessionEvent::CertificateRejected(Box::new(certificate())),
            );
            assert!(matches!(state, SessionState::Untrusted { .. }));
        }
    }

    #[test]
    fn trusting_a_certificate_returns_to_logged_out_not_authenticated() {
        // Accepting a certificate says nothing about who the user is; it only
        // unblocks the connection so a login can be attempted.
        let untrusted = SessionState::Untrusted {
            details: certificate(),
        };
        let (state, _) = transition(&untrusted, SessionEvent::CertificateTrusted);
        assert_eq!(state, SessionState::LoggedOut);
    }

    #[test]
    fn a_stored_session_is_restored_optimistically_without_a_round_trip() {
        // Avoids a blocking /me on every cold start; the first real request
        // either confirms it or drives Expired.
        let (state, effects) = transition(
            &SessionState::LoggedOut,
            SessionEvent::StoredSessionRestored(Box::new(user())),
        );
        assert_eq!(state, SessionState::Authenticated { user: user() });
        assert!(effects.contains(&SessionEffect::InstallCookie(true)));
        assert!(
            !effects.contains(&SessionEffect::VerifyIdentity),
            "restoring must not block startup on a network round trip"
        );
    }

    #[test]
    fn a_login_can_be_started_from_every_state() {
        for start in [
            SessionState::LoggedOut,
            SessionState::AwaitingLogin,
            SessionState::Authenticated { user: user() },
            SessionState::Expired,
            SessionState::Untrusted {
                details: certificate(),
            },
        ] {
            let (state, _) = transition(&start, SessionEvent::LoginStarted);
            assert_eq!(state, SessionState::AwaitingLogin);
        }
    }

    #[test]
    fn every_state_and_event_pair_terminates_without_panicking() {
        // The machine is total by construction; this pins that it stays so.
        let states = [
            SessionState::LoggedOut,
            SessionState::AwaitingLogin,
            SessionState::Authenticated { user: user() },
            SessionState::Expired,
            SessionState::Untrusted {
                details: certificate(),
            },
        ];
        let events = [
            SessionEvent::LoginStarted,
            SessionEvent::CookieCaptured,
            SessionEvent::IdentityConfirmed(Box::new(user())),
            SessionEvent::IdentityRejected,
            SessionEvent::UnauthorizedObserved,
            SessionEvent::CertificateRejected(Box::new(certificate())),
            SessionEvent::CertificateTrusted,
            SessionEvent::LogoutRequested,
            SessionEvent::StoredSessionRestored(Box::new(user())),
        ];
        for state in &states {
            for event in &events {
                let _ = transition(state, event.clone());
            }
        }
    }
}
