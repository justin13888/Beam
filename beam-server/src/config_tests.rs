use super::*;

/// A fully-populated config with known secret values, for asserting that
/// none of them survive into `Debug` output.
fn config_with_secrets() -> ServerConfig {
    ServerConfig {
        server_url: "https://beam.example.com".to_string(),
        video_dir: PathBuf::from("/videos"),
        data_dir: PathBuf::from("/cache"),
        database_url: "postgres://beam:db-secret-pw@localhost:5432/beam".to_string(),
        tmdb_api_token: Some("tmdb-secret-token".to_string()),
        tvdb_api_key: Some("tvdb-secret-key".to_string()),
        tvdb_pin: Some("tvdb-secret-pin".to_string()),
        oidc_issuer: Some("https://idp.example.com".to_string()),
        oidc_client_id: Some("beam-client".to_string()),
        oidc_client_secret: Some("oidc-secret-value".to_string()),
        oidc_admin_claim: Some("groups".to_string()),
        oidc_admin_value: Some("beam-admin".to_string()),
        web_url: "https://beam.example.com".to_string(),
        ..Default::default()
    }
}

#[test]
fn debug_output_redacts_every_secret() {
    let config = config_with_secrets();
    let output = format!("{config:?}");

    assert!(!output.contains("db-secret-pw"), "output: {output}");
    assert!(!output.contains("tmdb-secret-token"), "output: {output}");
    assert!(!output.contains("tvdb-secret"), "output: {output}");
    assert!(!output.contains("oidc-secret-value"), "output: {output}");
    assert!(output.contains("<redacted>"), "output: {output}");
    // Non-secret fields stay visible for operator debugging.
    assert!(output.contains("beam.example.com"), "output: {output}");
    assert!(output.contains("beam-client"), "output: {output}");
}

/// An ingest token in a collector URL's path or query is a secret; the
/// origin is what an operator needs to see which collector is configured.
#[test]
fn debug_output_shows_only_the_telemetry_origin() {
    let config = ServerConfig {
        telemetry_url: Some(
            "https://user:pw@collector.example:4318/v1/metrics?token=ingest-secret".to_string(),
        ),
        ..Default::default()
    };
    let output = format!("{config:?}");

    assert!(
        output.contains("https://collector.example:4318"),
        "output: {output}"
    );
    for secret in ["ingest-secret", "v1/metrics", "user:pw"] {
        assert!(!output.contains(secret), "{secret} leaked: {output}");
    }
}

#[test]
fn a_telemetry_url_must_be_http_or_https_with_a_host() {
    for accepted in [
        "https://collector.example/v1/metrics",
        "http://10.0.0.5:4318/v1/metrics",
    ] {
        let config = ServerConfig {
            telemetry_url: Some(accepted.to_string()),
            ..Default::default()
        };
        assert!(config.validate_values().is_ok(), "{accepted} is valid");
    }
    for refused in [
        "ftp://collector.example/v1/metrics",
        "collector.example/v1/metrics",
        "https://",
        "/v1/metrics",
        "not a url",
    ] {
        let config = ServerConfig {
            telemetry_url: Some(refused.to_string()),
            ..Default::default()
        };
        assert!(
            matches!(
                config.validate_values(),
                Err(ConfigError::InvalidValue(field, _)) if field == "BEAM_TELEMETRY_URL"
            ),
            "{refused} must be refused"
        );
    }
}

/// `BEAM_TELEMETRY_URL=` in a compose file is off, not an invalid URL.
#[test]
fn a_blank_telemetry_url_is_off() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("videos")).unwrap();

    let validated = ServerConfig {
        video_dir: temp.path().join("videos"),
        data_dir: temp.path().join("data"),
        telemetry_url: Some("  ".to_string()),
        ..Default::default()
    }
    .normalize_and_validate()
    .expect("a blank URL is unset");

    assert_eq!(validated.telemetry_url, None);
}

/// A retention of zero would prune today's counts as they arrive, and one
/// past the ceiling is a typo; both fail the load. The range's own ends
/// are accepted.
#[test]
fn playback_telemetry_retention_must_be_in_range() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("videos")).unwrap();
    let with = |days: u32| ServerConfig {
        video_dir: temp.path().join("videos"),
        data_dir: temp.path().join("data"),
        playback_telemetry_retention_days: days,
        ..Default::default()
    };

    for accepted in [
        *PLAYBACK_TELEMETRY_RETENTION_DAYS.start(),
        *PLAYBACK_TELEMETRY_RETENTION_DAYS.end(),
    ] {
        with(accepted)
            .normalize_and_validate()
            .unwrap_or_else(|e| panic!("{accepted} should be accepted: {e}"));
    }
    for refused in [
        PLAYBACK_TELEMETRY_RETENTION_DAYS.start() - 1,
        PLAYBACK_TELEMETRY_RETENTION_DAYS.end() + 1,
    ] {
        let err = with(refused)
            .normalize_and_validate()
            .expect_err("out of range");
        assert!(
            err.to_string()
                .contains("BEAM_PLAYBACK_TELEMETRY_RETENTION_DAYS"),
            "{err}"
        );
    }
}

/// `BEAM_OIDC_CLIENT_AUTH_METHOD` takes exactly RFC 7591's
/// `token_endpoint_auth_method` names for the two secret-based methods --
/// what an operator copies from the IdP's client registration. Anything
/// else, including a method Beam cannot speak, fails the load rather than
/// falling back to a method the IdP would refuse at the first login. The
/// empty string does not parse either, which is what makes confique treat
/// it as unset (pinned end to end below).
///
/// Parsed through `str`'s `IntoDeserializer`, the path confique's env
/// source takes for an enum (and its declared default).
#[test]
fn the_client_auth_method_setting_accepts_only_the_rfc_7591_names() {
    use serde::Deserialize as _;
    use serde::de::IntoDeserializer as _;
    use serde::de::value::{Error, StrDeserializer};

    let parse = |raw: &str| {
        let deserializer: StrDeserializer<'_, Error> = raw.into_deserializer();
        ClientAuthMethod::deserialize(deserializer).ok()
    };

    for (raw, expected) in [
        (
            "client_secret_basic",
            Some(ClientAuthMethod::ClientSecretBasic),
        ),
        (
            "client_secret_post",
            Some(ClientAuthMethod::ClientSecretPost),
        ),
        ("CLIENT_SECRET_POST", None),
        ("basic", None),
        ("post", None),
        ("client_secret_jwt", None),
        ("private_key_jwt", None),
        ("none", None),
        ("", None),
    ] {
        assert_eq!(parse(raw), expected, "{raw:?}");
    }
}

/// The child half of
/// [`an_empty_client_auth_method_resolves_to_the_default`]: loads the
/// configuration from the environment that test hands it. Ignored because
/// it means nothing outside that environment, and refuses to pass there.
#[test]
#[ignore = "run by an_empty_client_auth_method_resolves_to_the_default in a child process"]
fn client_auth_method_loaded_from_an_empty_variable() {
    assert_eq!(
        std::env::var("BEAM_OIDC_CLIENT_AUTH_METHOD").as_deref(),
        Ok(""),
        "run through an_empty_client_auth_method_resolves_to_the_default"
    );
    let config = ServerConfig::builder()
        .env()
        .load()
        .expect("an empty auth method loads");
    assert_eq!(
        config.oidc_client_auth_method,
        ClientAuthMethod::ClientSecretBasic
    );
}

/// A set-but-empty `BEAM_OIDC_CLIENT_AUTH_METHOD` is the default,
/// `client_secret_basic`, not a startup failure: confique treats an empty
/// value that does not deserialize as unset, the repo-wide rule for
/// optional settings that docs/operations/configuration.md states.
///
/// Proven through the real env source, which reads the process
/// environment. Setting a variable in-process is `unsafe` in Rust 2024
/// because the suite runs in parallel, so this re-executes the test binary
/// for the one ignored child test, with every inherited `BEAM_*` variable
/// removed and only this one set.
#[test]
fn an_empty_client_auth_method_resolves_to_the_default() {
    let mut child =
        std::process::Command::new(std::env::current_exe().expect("the test binary's own path"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("BEAM_") {
            child.env_remove(key);
        }
    }
    let output = child
        .env("BEAM_OIDC_CLIENT_AUTH_METHOD", "")
        .args([
            "--exact",
            "config::tests::client_auth_method_loaded_from_an_empty_variable",
            "--ignored",
            "--test-threads=1",
        ])
        .output()
        .expect("the test binary re-executes");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // An exact filter that matched nothing would also exit 0.
    assert!(stdout.contains("1 passed"), "stdout: {stdout}");
}

#[test]
fn debug_output_names_the_client_auth_method() {
    let config = ServerConfig {
        oidc_client_auth_method: ClientAuthMethod::ClientSecretPost,
        ..config_with_secrets()
    };
    let output = format!("{config:?}");
    assert!(
        output.contains("oidc_client_auth_method: ClientSecretPost"),
        "output: {output}"
    );
}

#[test]
fn redacted_database_url_hides_password_only() {
    let config = config_with_secrets();
    assert_eq!(
        config.redacted_database_url(),
        "postgres://beam:<redacted>@localhost:5432/beam"
    );
}

#[test]
fn cookie_security_verdict_covers_scheme_and_override_combinations() {
    // (server_url, web_url, extra_origins, explicit override, expected)
    let cases = [
        // Plain-HTTP local dev: nothing implies HTTPS.
        (
            "http://localhost:8000",
            "http://localhost:5173",
            None,
            None,
            CookieSecurityVerdict::Ok,
        ),
        // Fully-HTTPS deployment: heuristic resolves Secure.
        (
            "https://beam.example.com",
            "https://beam.example.com",
            None,
            None,
            CookieSecurityVerdict::Ok,
        ),
        // TLS-terminating proxy in front of a plain-HTTP origin, no
        // override: the classic footgun -- refuse to start.
        (
            "http://localhost:8000",
            "https://beam.example.com",
            None,
            None,
            CookieSecurityVerdict::ErrLikelyMisconfigured,
        ),
        // Same, implied only via an extra allowed origin.
        (
            "http://localhost:8000",
            "http://localhost:5173",
            Some("https://other.example.com"),
            None,
            CookieSecurityVerdict::ErrLikelyMisconfigured,
        ),
        // Proxy topology fixed by an explicit opt-in to Secure cookies.
        (
            "http://localhost:8000",
            "https://beam.example.com",
            None,
            Some(true),
            CookieSecurityVerdict::Ok,
        ),
        // Explicit opt-out is honored but flagged.
        (
            "http://localhost:8000",
            "https://beam.example.com",
            None,
            Some(false),
            CookieSecurityVerdict::WarnExplicitInsecure,
        ),
        // Explicit opt-out with nothing HTTPS-like at all: plain Ok.
        (
            "http://localhost:8000",
            "http://localhost:5173",
            None,
            Some(false),
            CookieSecurityVerdict::Ok,
        ),
    ];

    for (server_url, web_url, extra, cookie_secure, expected) in cases {
        let config = ServerConfig {
            server_url: server_url.to_string(),
            web_url: web_url.to_string(),
            extra_allowed_origins: extra.map(str::to_string),
            cookie_secure,
            ..config_with_secrets()
        };
        assert_eq!(
            config.cookie_security_verdict(),
            expected,
            "server_url={server_url}, web_url={web_url}, extra={extra:?}, \
             override={cookie_secure:?}"
        );
    }
}

#[test]
fn a_set_but_empty_optional_setting_means_the_same_as_unset() {
    // `BEAM_METADATA_LANGUAGE=` in a compose file arrives as `Some("")`.
    // Left that way it reaches cameo as a language tag and fails the
    // client build -- which, being explicit, refuses to start the server.
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("videos")).unwrap();

    let validated = ServerConfig {
        video_dir: temp.path().join("videos"),
        data_dir: temp.path().join("data"),
        metadata_language: Some("   ".to_string()),
        oidc_admin_claim: Some(String::new()),
        oidc_admin_value: None,
        ..Default::default()
    }
    .normalize_and_validate()
    .expect("blank optional settings are not a misconfiguration");

    assert_eq!(validated.metadata_language, None);
    assert_eq!(validated.oidc_admin_claim, None);
}

#[test]
fn a_value_is_normalised_before_it_is_validated() {
    // Order matters: `BEAM_OIDC_ADMIN_VALUE` set with `BEAM_OIDC_ADMIN_CLAIM`
    // blank is the half-configured combination `validate_values` rejects --
    // but only once the blank claim has been normalised to `None`.
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("videos")).unwrap();

    let error = ServerConfig {
        video_dir: temp.path().join("videos"),
        data_dir: temp.path().join("data"),
        oidc_admin_claim: Some("  ".to_string()),
        oidc_admin_value: Some("beam-admin".to_string()),
        ..Default::default()
    }
    .normalize_and_validate()
    .expect_err("a blank claim with a value set is still half-configured");

    assert!(
        matches!(&error, ConfigError::InvalidValue(field, _) if field == "BEAM_OIDC_ADMIN_VALUE"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_bad_scalar_fails_before_any_directory_is_created() {
    // `validate_values` runs before `validate_paths` so a typo'd number
    // does not leave a half-made data directory behind.
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("state").join("beam");

    let error = ServerConfig {
        video_dir: temp.path().join("videos"),
        data_dir: data_dir.clone(),
        enrich_batch_size: 0,
        ..Default::default()
    }
    .normalize_and_validate()
    .expect_err("a zero batch size is rejected");

    assert!(
        matches!(&error, ConfigError::InvalidValue(field, _) if field == "BEAM_ENRICH_BATCH_SIZE"),
        "unexpected error: {error}"
    );
    assert!(
        !temp.path().join("state").exists(),
        "no filesystem side effect before the values are known to be valid"
    );
}

#[test]
fn the_oidc_redirect_url_points_at_this_servers_own_callback() {
    // It is sent to the identity provider as the place to come back to and
    // is matched exactly against the client's registered redirect URIs, so
    // it must be the *server* origin -- not the web origin, and not empty.
    let config = ServerConfig {
        server_url: "https://beam.example.com".to_string(),
        web_url: "https://app.example.com".to_string(),
        ..Default::default()
    };
    assert_eq!(
        config.oidc_redirect_url(),
        "https://beam.example.com/v1/auth/callback"
    );
}

#[test]
fn session_days_are_converted_to_seconds() {
    // 14 days is 1_209_600 seconds. A mistyped conversion still produces a
    // plausible-looking number and signs everyone out within minutes.
    let config = ServerConfig {
        session_idle_days: 14,
        session_max_days: 60,
        ..Default::default()
    };
    assert_eq!(config.session_idle_ttl_secs(), 1_209_600);
    assert_eq!(config.session_absolute_ttl_secs(), 5_184_000);
    assert_eq!(
        ServerConfig {
            session_idle_days: 1,
            ..Default::default()
        }
        .session_idle_ttl_secs(),
        86_400
    );
}

#[test]
fn missing_file_grace_days_are_converted_to_a_duration() {
    // A mistyped conversion purges progress for a disk that is away for a
    // weekend; zero must stay zero ("purge at the next healthy scan").
    let grace = |days| {
        ServerConfig {
            missing_file_grace_days: days,
            ..Default::default()
        }
        .missing_file_grace()
    };
    assert_eq!(grace(30), std::time::Duration::from_secs(2_592_000));
    assert_eq!(grace(1), std::time::Duration::from_secs(86_400));
    assert_eq!(grace(0), std::time::Duration::ZERO);
}

#[test]
fn oidc_is_configured_only_when_all_three_settings_are_present() {
    // Any one of them missing means discovery cannot succeed; treating a
    // partial configuration as complete turns a clear "not configured"
    // startup message into a discovery failure at first login.
    let all = |issuer, client_id, secret| ServerConfig {
        oidc_issuer: issuer,
        oidc_client_id: client_id,
        oidc_client_secret: secret,
        ..Default::default()
    };
    let set = || Some("x".to_string());

    assert!(all(set(), set(), set()).oidc_configured());
    assert!(!all(None, set(), set()).oidc_configured());
    assert!(!all(set(), None, set()).oidc_configured());
    assert!(!all(set(), set(), None).oidc_configured());
    assert!(!all(None, None, None).oidc_configured());
}

#[test]
fn an_absent_secret_stays_absent_in_debug_output() {
    // `redact_option` must distinguish "not configured" from "configured
    // but hidden": rendering `None` as `<redacted>` would make an operator
    // debugging a missing token believe it was set.
    let unset = ServerConfig {
        tmdb_api_token: None,
        oidc_client_secret: None,
        ..Default::default()
    };
    let output = format!("{unset:?}");
    assert!(
        output.contains("tmdb_api_token: None"),
        "an unset secret must render as None, got: {output}"
    );

    let set = ServerConfig {
        tmdb_api_token: Some("tmdb-secret-token".to_string()),
        ..Default::default()
    };
    let output = format!("{set:?}");
    // The field itself, not merely a `<redacted>` somewhere in the struct:
    // `database_url` renders one too, so a looser assertion passes even
    // when the secret is dropped entirely.
    assert!(
        output.contains(r#"tmdb_api_token: Some("<redacted>")"#),
        "a set secret must render as redacted, not as absent: {output}"
    );
    assert!(!output.contains("tmdb-secret-token"), "output: {output}");
}

#[test]
fn the_compiled_in_defaults_are_internally_valid() {
    // A defaults-only deployment must start: every declared default has to
    // satisfy the validation the same struct enforces. Asserting each
    // default's *value* would just restate the attribute above it; this
    // asserts the property that actually matters.
    ServerConfig::default()
        .validate_values()
        .expect("a defaults-only configuration must pass its own validation");
}

#[test]
fn an_invalid_scan_ignore_pattern_fails_startup_and_names_the_pattern() {
    let config = ServerConfig {
        scan_ignore: vec!["Downloads".to_string(), "bad[glob".to_string()],
        ..Default::default()
    };
    let err = config.validate_values().unwrap_err();
    assert!(
        matches!(&err, ConfigError::InvalidValue(field, reason)
            if field == "BEAM_SCAN_IGNORE" && reason.contains("bad[glob")),
        "{err:?}"
    );
}

/// Who `config`'s trusted-proxy policy says sent a request that arrived
/// from `peer` carrying `X-Forwarded-For: forwarded_for`.
fn resolved_client(config: &ServerConfig, peer: &str, forwarded_for: &str) -> String {
    let mut headers = kynos::http::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        forwarded_for.parse().expect("a valid header value"),
    );
    let policy = config.trusted_proxy_policy().expect("the policy is valid");
    kynos::http::forwarded::Forwarded::resolve(
        &headers,
        Some(peer.parse().expect("a socket address")),
        &policy,
    )
    .client()
    .expect("a socket peer always names a client")
    .to_string()
}

/// Issue #200: with no proxy configured, a client cannot name its own
/// address -- the socket peer is the client whatever the header says.
#[test]
fn by_default_the_socket_peer_is_the_client() {
    let config = ServerConfig::default();
    assert_eq!(
        resolved_client(&config, "203.0.113.7:41000", "1.1.1.1"),
        "203.0.113.7"
    );
}

/// A peer inside a listed network is believed about the hop before it, and
/// a listed proxy behind another is walked through; a peer outside every
/// listed network is not believed at all.
#[test]
fn only_a_peer_in_a_listed_network_is_believed() {
    let config = ServerConfig {
        trusted_proxies: vec!["10.0.0.0/8".to_string(), "2001:db8::/32".to_string()],
        ..Default::default()
    };

    assert_eq!(
        resolved_client(&config, "10.1.2.3:443", "198.51.100.4"),
        "198.51.100.4",
        "a listed proxy names the client"
    );
    assert_eq!(
        resolved_client(&config, "10.1.2.3:443", "198.51.100.4, 10.9.9.9"),
        "198.51.100.4",
        "a chain of listed proxies is walked to the first unlisted hop"
    );
    assert_eq!(
        resolved_client(&config, "10.1.2.3:443", "6.6.6.6, 198.51.100.4"),
        "198.51.100.4",
        "the walk stops at the first unlisted hop, so an entry the client \
         wrote before it is never believed"
    );
    assert_eq!(
        resolved_client(&config, "1.2.3.4:443", "198.51.100.4"),
        "1.2.3.4",
        "a peer outside every listed network is the client"
    );
    assert_eq!(
        resolved_client(&config, "[2001:db8::1]:443", "198.51.100.4"),
        "198.51.100.4",
        "IPv6 networks are honoured"
    );
}

/// `BEAM_RATE_LIMIT_TRUST_FORWARDED_FOR` adds the nearest hop wherever it
/// is, and composes with the listed networks rather than replacing them.
#[test]
fn the_nearest_hop_flag_composes_with_the_listed_networks() {
    let flag_only = ServerConfig {
        rate_limit_trust_forwarded_for: true,
        ..Default::default()
    };
    assert_eq!(
        resolved_client(&flag_only, "203.0.113.7:443", "6.6.6.6, 198.51.100.4"),
        "198.51.100.4",
        "one hop is believed from any peer, and only one"
    );

    let both = ServerConfig {
        rate_limit_trust_forwarded_for: true,
        trusted_proxies: vec!["10.0.0.0/8".to_string()],
        ..Default::default()
    };
    assert_eq!(
        resolved_client(&both, "203.0.113.7:443", "198.51.100.4, 10.9.9.9"),
        "198.51.100.4",
        "the nearest hop, then a listed proxy behind it"
    );
}

#[test]
fn an_invalid_trusted_proxy_fails_startup_and_names_the_entry() {
    for entry in ["10.0.0.1", "10.0.0.0/33", "0.0.0.0/0", "proxy.internal/8"] {
        let config = ServerConfig {
            trusted_proxies: vec!["10.0.0.0/8".to_string(), entry.to_string()],
            ..Default::default()
        };
        let err = config.validate_values().unwrap_err();
        assert!(
            matches!(&err, ConfigError::InvalidValue(field, reason)
                if field == "BEAM_TRUSTED_PROXIES" && reason.contains(entry)),
            "{entry}: {err:?}"
        );
    }
}

#[test]
fn trusted_proxies_are_trimmed_and_empty_ones_dropped() {
    let mut config = ServerConfig {
        trusted_proxies: vec![
            "10.0.0.0/8".to_string(),
            " 192.0.2.1/32".to_string(),
            String::new(),
        ],
        ..Default::default()
    };
    config.normalize_values();
    assert_eq!(config.trusted_proxies, vec!["10.0.0.0/8", "192.0.2.1/32"]);
    assert!(config.validate_values().is_ok());
}

#[test]
fn scan_ignore_patterns_are_trimmed_and_empty_ones_dropped() {
    let mut config = ServerConfig {
        scan_ignore: vec![
            "Downloads".to_string(),
            " **/*.partial.mkv".to_string(),
            String::new(),
        ],
        ..Default::default()
    };
    config.normalize_values();
    assert_eq!(config.scan_ignore, vec!["Downloads", "**/*.partial.mkv"]);
    assert!(
        config
            .scan_path_policy()
            .unwrap()
            .excludes_directory(std::path::Path::new("downloads")),
        "the configured pattern reaches the policy"
    );
}

#[test]
fn zero_enrich_batch_size_is_rejected() {
    let config = ServerConfig {
        enrich_batch_size: 0,
        ..config_with_secrets()
    };
    let err = config
        .validate_values()
        .expect_err("batch size 0 must be rejected");
    assert!(
        matches!(&err, ConfigError::InvalidValue(field, _) if field == "BEAM_ENRICH_BATCH_SIZE"),
        "unexpected error: {err}"
    );
}

#[test]
fn out_of_range_enrich_min_confidence_is_rejected() {
    // (0.0, 1.0]: the open lower bound rejects 0.0, the closed upper bound
    // accepts exactly 1.0 but rejects anything above it.
    for bad in [0.0_f64, -0.1, 1.5] {
        let config = ServerConfig {
            enrich_min_confidence: bad,
            ..config_with_secrets()
        };
        let err = config
            .validate_values()
            .expect_err("min confidence outside (0.0, 1.0] must be rejected");
        assert!(
            matches!(&err, ConfigError::InvalidValue(field, _) if field == "BEAM_ENRICH_MIN_CONFIDENCE"),
            "value {bad} produced unexpected error: {err}"
        );
    }

    // Boundary that must be accepted.
    let config = ServerConfig {
        enrich_min_confidence: 1.0,
        ..config_with_secrets()
    };
    assert!(
        config.validate_values().is_ok(),
        "1.0 is a valid upper bound"
    );
}

#[test]
fn zero_scan_interval_is_rejected() {
    let config = ServerConfig {
        scan_interval_secs: 0,
        ..config_with_secrets()
    };
    let err = config
        .validate_values()
        .expect_err("a zero rescan interval must be rejected");
    assert!(
        matches!(&err, ConfigError::InvalidValue(f, _) if f == "BEAM_SCAN_INTERVAL_SECS"),
        "unexpected error: {err}"
    );
    let one = ServerConfig {
        scan_interval_secs: 1,
        ..config_with_secrets()
    };
    assert!(one.validate_values().is_ok(), "1 is the smallest interval");
}

#[test]
fn zero_watch_poll_interval_is_rejected() {
    let config = ServerConfig {
        watch_poll_interval_secs: 0,
        ..config_with_secrets()
    };
    let err = config
        .validate_values()
        .expect_err("a zero poll interval must be rejected");
    assert!(
        matches!(&err, ConfigError::InvalidValue(f, _) if f == "BEAM_WATCH_POLL_INTERVAL_SECS"),
        "unexpected error: {err}"
    );
}

#[test]
fn zero_rate_limit_per_minute_is_rejected() {
    for field in [
        "BEAM_RATE_LIMIT_AUTH_PER_MINUTE",
        "BEAM_RATE_LIMIT_SEARCH_PER_MINUTE",
        "BEAM_RATE_LIMIT_DEVICE_POLL_PER_MINUTE",
    ] {
        let config = ServerConfig {
            rate_limit_auth_per_minute: if field.contains("AUTH") { 0 } else { 10 },
            rate_limit_search_per_minute: if field.contains("SEARCH") { 0 } else { 60 },
            rate_limit_device_poll_per_minute: if field.contains("DEVICE") { 0 } else { 30 },
            ..config_with_secrets()
        };
        let err = config
            .validate_values()
            .expect_err("a zero per-minute rate must be rejected");
        assert!(
            matches!(&err, ConfigError::InvalidValue(f, _) if f == field),
            "unexpected error for {field}: {err}"
        );
    }
}

#[test]
fn admin_value_without_admin_claim_is_rejected() {
    let config = ServerConfig {
        oidc_admin_claim: None,
        oidc_admin_value: Some("beam-admin".to_string()),
        ..config_with_secrets()
    };
    let err = config
        .validate_values()
        .expect_err("BEAM_OIDC_ADMIN_VALUE without BEAM_OIDC_ADMIN_CLAIM must be rejected");
    assert!(
        matches!(&err, ConfigError::InvalidValue(field, _) if field == "BEAM_OIDC_ADMIN_VALUE"),
        "unexpected error: {err}"
    );
}

#[test]
fn admin_claim_configurations_pass_validation() {
    // Both unset (nobody admin), claim-only (boolean claim), and
    // claim+value (string/array claim) are all valid.
    for (claim, value) in [
        (None, None),
        (Some("is_admin"), None),
        (Some("groups"), Some("beam-admin")),
    ] {
        let config = ServerConfig {
            oidc_admin_claim: claim.map(str::to_string),
            oidc_admin_value: value.map(str::to_string),
            ..config_with_secrets()
        };
        assert!(
            config.validate_values().is_ok(),
            "claim={claim:?} value={value:?} should validate"
        );
    }
}

#[test]
fn admin_claim_and_value_normalization_maps_empty_to_none() {
    // Set-but-empty env vars (confique yields Some("")) must mean unset,
    // so a half-empty pair doesn't later trip the validation error.
    let mut config = ServerConfig {
        oidc_admin_claim: Some("  ".to_string()),
        oidc_admin_value: Some("".to_string()),
        ..config_with_secrets()
    };
    config.normalize_values();
    assert_eq!(config.oidc_admin_claim, None);
    assert_eq!(config.oidc_admin_value, None);

    // A real value is trimmed but preserved.
    let mut config = ServerConfig {
        oidc_admin_claim: Some(" groups ".to_string()),
        oidc_admin_value: Some(" beam-admin ".to_string()),
        ..config_with_secrets()
    };
    config.normalize_values();
    assert_eq!(config.oidc_admin_claim.as_deref(), Some("groups"));
    assert_eq!(config.oidc_admin_value.as_deref(), Some("beam-admin"));
}

#[test]
fn metadata_language_normalization_maps_empty_and_whitespace_to_none() {
    // (raw value as loaded, expected after normalization)
    let cases: [(Option<&str>, Option<&str>); 5] = [
        // Set-but-empty env var: confique yields Some("") -- must mean unset.
        (Some(""), None),
        (Some("   "), None),
        // Surrounding whitespace is trimmed off a real value.
        (Some("  en-US  "), Some("en-US")),
        // A clean value survives untouched.
        (Some("ja"), Some("ja")),
        (None, None),
    ];

    for (raw, expected) in cases {
        let mut config = ServerConfig {
            metadata_language: raw.map(str::to_string),
            ..config_with_secrets()
        };
        config.normalize_values();
        assert_eq!(
            config.metadata_language.as_deref(),
            expected,
            "raw value {raw:?} normalized incorrectly"
        );
    }
}

#[test]
fn a_blank_tvdb_credential_is_no_credential() {
    // `BEAM_TVDB_API_KEY=` in a compose file must leave TheTVDB
    // unconfigured, not configured with an empty key it refuses.
    for (raw, expected) in [
        (Some(""), None),
        (Some("  "), None),
        (Some(" key "), Some("key")),
        (None, None),
    ] {
        let mut config = ServerConfig {
            tvdb_api_key: raw.map(str::to_string),
            tvdb_pin: raw.map(str::to_string),
            ..config_with_secrets()
        };
        config.normalize_values();
        assert_eq!(config.tvdb_api_key.as_deref(), expected, "{raw:?}");
        assert_eq!(config.tvdb_pin.as_deref(), expected, "{raw:?}");
    }
}

#[test]
fn redact_url_password_handles_urls_without_credentials() {
    // No userinfo at all.
    assert_eq!(
        redact_url_password("postgres://localhost:5432/beam"),
        "postgres://localhost:5432/beam"
    );
    // User without password.
    assert_eq!(
        redact_url_password("postgres://beam@localhost/beam"),
        "postgres://beam@localhost/beam"
    );
    // Not a URL: returned unchanged rather than panicking.
    assert_eq!(redact_url_password("not a url"), "not a url");
    // An `@` in the path must not be mistaken for userinfo.
    assert_eq!(
        redact_url_password("postgres://localhost/db@name"),
        "postgres://localhost/db@name"
    );
}

mod validate_paths {
    use super::*;

    /// `validate_paths` reaches the real filesystem, and a `TempDir` is a
    /// real filesystem that needs no infrastructure -- so it is the subject
    /// here rather than a `FileSystem` double, which would only prove the
    /// double returns what it was configured to return.
    fn config_in(dir: &std::path::Path) -> ServerConfig {
        ServerConfig {
            video_dir: dir.join("videos"),
            data_dir: dir.join("data"),
            ..Default::default()
        }
    }

    #[test]
    fn a_missing_video_dir_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let config = config_in(temp.path());

        let err = config
            .validate_paths()
            .expect_err("a video dir that does not exist must be rejected");
        assert!(
            matches!(&err, ConfigError::DirNotFoundError(path) if path.contains("videos")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_video_dir_that_is_a_file_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("videos"), b"not a directory").unwrap();

        let err = config_in(temp.path())
            .validate_paths()
            .expect_err("a regular file is not a usable video directory");
        assert!(
            matches!(&err, ConfigError::DirNotFoundError(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn an_existing_video_dir_is_accepted_and_the_data_parent_is_created() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("videos")).unwrap();
        let config = ServerConfig {
            video_dir: temp.path().join("videos"),
            // Two levels deep: the parent does not exist yet.
            data_dir: temp.path().join("state").join("beam"),
            ..Default::default()
        };

        config
            .validate_paths()
            .expect("an existing video dir and a creatable data dir are valid");

        assert!(
            temp.path().join("state").is_dir(),
            "the data directory's parent must be created, not merely tolerated"
        );
        assert!(
            !temp.path().join("state").join("beam").exists(),
            "only the parent is created; the data dir itself is the server's to make"
        );
    }

    #[test]
    fn a_data_dir_at_the_filesystem_root_needs_no_parent_created() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("videos")).unwrap();
        let config = ServerConfig {
            video_dir: temp.path().join("videos"),
            // Parent is the temp dir, which already exists.
            data_dir: temp.path().join("data"),
            ..Default::default()
        };

        config.validate_paths().expect("an existing parent is fine");
    }

    /// A data directory whose parent cannot be made -- here because a
    /// regular file stands where a folder on its path must go -- is a
    /// `DirCreationError` naming `BEAM_DATA_DIR`, not a panic and not a
    /// server that starts and fails on its first write.
    #[test]
    fn a_data_dir_whose_parent_cannot_be_created_is_a_dir_creation_error() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("videos")).unwrap();
        std::fs::write(temp.path().join("blocker"), b"a file, not a folder").unwrap();
        let config = ServerConfig {
            video_dir: temp.path().join("videos"),
            data_dir: temp.path().join("blocker").join("state").join("beam"),
            ..Default::default()
        };

        let err = config
            .validate_paths()
            .expect_err("a parent beneath a regular file cannot be created");
        assert!(
            matches!(&err, ConfigError::DirCreationError(key, path, _)
                if key == "BEAM_DATA_DIR" && path.ends_with("beam")),
            "unexpected error: {err}"
        );
        assert!(
            temp.path().join("blocker").is_file(),
            "the file in the way is left as it was"
        );
    }
}

/// The child half of [`an_unparseable_variable_fails_to_load`]: loads the
/// configuration from the environment that test hands it. Ignored because
/// it means nothing outside that environment, and refuses to pass there.
#[test]
#[ignore = "run by an_unparseable_variable_fails_to_load in a child process"]
fn config_loaded_from_an_unparseable_variable() {
    assert_eq!(
        std::env::var("BEAM_SHUTDOWN_TIMEOUT_SECS").as_deref(),
        Ok("soon"),
        "run through an_unparseable_variable_fails_to_load"
    );
    let err = ServerConfig::builder()
        .env()
        .load()
        .map_err(ConfigError::from)
        .expect_err("`soon` is not a number of seconds");
    assert!(
        matches!(err, ConfigError::LoadError(_)),
        "unexpected error: {err}"
    );
    assert!(
        format!("{err:#}").contains("BEAM_SHUTDOWN_TIMEOUT_SECS")
            || format!("{err:?}").contains("BEAM_SHUTDOWN_TIMEOUT_SECS"),
        "the error names the variable an operator has to fix: {err:?}"
    );
}

/// A set variable that does not parse as its type stops the load with a
/// `LoadError` naming the variable, rather than falling back to the
/// default as an empty value does: an operator who wrote a value meant
/// it. Proven through the real env source in a child process, for the
/// reason `an_empty_client_auth_method_resolves_to_the_default` gives.
#[test]
fn an_unparseable_variable_fails_to_load() {
    let mut child =
        std::process::Command::new(std::env::current_exe().expect("the test binary's own path"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("BEAM_") {
            child.env_remove(key);
        }
    }
    let output = child
        .env("BEAM_SHUTDOWN_TIMEOUT_SECS", "soon")
        .args([
            "--exact",
            "config::tests::config_loaded_from_an_unparseable_variable",
            "--ignored",
            "--test-threads=1",
        ])
        .output()
        .expect("the test binary re-executes");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // An exact filter that matched nothing would also exit 0.
    assert!(stdout.contains("1 passed"), "stdout: {stdout}");
}

/// The indexer's own defaults -- what it does when nothing overrides it,
/// as its tests and any caller building it directly see -- are the
/// defaults this config documents to operators: one value each, not two
/// that drift. confique's `default` takes only a literal, so the config
/// cannot name beam-index's constants; this holds the two together.
#[test]
fn the_indexer_defaults_are_the_documented_defaults() {
    let config = ServerConfig::default();
    let policy = beam_index::services::EnrichmentPolicy::default();
    assert_eq!(config.enrich_batch_size, policy.batch_size);
    assert_eq!(config.enrich_min_confidence, policy.min_confidence);
    assert_eq!(
        std::time::Duration::from_secs(u64::from(config.missing_file_grace_days) * 24 * 60 * 60),
        beam_index::services::index::DEFAULT_MISSING_FILE_GRACE
    );
}
