//! Tests for the wiring decisions `AppServices::new` makes from configuration.

use super::*;
use crate::config::ServerConfig;

/// The providers the chosen implementation reports as available. Empty means
/// enrichment is effectively off, which is how `NoopEnrichmentProvider`
/// identifies itself.
fn providers_for(config: ServerConfig) -> Vec<String> {
    build_enrichment_provider(&config)
        .expect("this configuration must not fail startup")
        .available_providers()
}

#[test]
fn no_credentials_at_all_falls_back_to_a_disabled_provider() {
    let providers = providers_for(ServerConfig {
        tmdb_api_token: None,
        anilist_enabled: false,
        ..Default::default()
    });
    assert!(
        providers.is_empty(),
        "with nothing configured, enrichment must be a fast no-op, not an error"
    );
}

#[test]
fn an_empty_tmdb_token_counts_as_unconfigured() {
    // An unset environment variable and one set to the empty string are the
    // same intent; building a client from an empty token would fail at the
    // first request instead.
    let providers = providers_for(ServerConfig {
        tmdb_api_token: Some(String::new()),
        anilist_enabled: false,
        ..Default::default()
    });
    assert!(providers.is_empty());
}

#[test]
fn a_tmdb_token_alone_enables_tmdb_only() {
    let providers = providers_for(ServerConfig {
        tmdb_api_token: Some("tmdb-token".to_string()),
        anilist_enabled: false,
        ..Default::default()
    });
    assert_eq!(providers, vec!["tmdb".to_string()]);
}

#[test]
fn anilist_alone_enables_anilist_only() {
    // AniList needs no credential, so it is enabled by the flag alone.
    let providers = providers_for(ServerConfig {
        tmdb_api_token: None,
        anilist_enabled: true,
        ..Default::default()
    });
    assert_eq!(providers, vec!["anilist".to_string()]);
}

#[test]
fn both_configured_enables_both_with_tmdb_first() {
    // Priority order is meaningful: TMDB is the general-purpose source and
    // AniList the specialist, so a title both know about resolves via TMDB.
    let providers = providers_for(ServerConfig {
        tmdb_api_token: Some("tmdb-token".to_string()),
        anilist_enabled: true,
        ..Default::default()
    });
    assert_eq!(providers, vec!["tmdb".to_string(), "anilist".to_string()]);
}

#[test]
fn an_explicit_metadata_language_that_cannot_be_built_fails_startup() {
    // The asymmetry that matters: an operator who set BEAM_METADATA_LANGUAGE
    // must not have it silently ignored on a headless server.
    let error = build_enrichment_provider(&ServerConfig {
        tmdb_api_token: Some("tmdb-token".to_string()),
        anilist_enabled: false,
        metadata_language: Some("not a bcp-47 tag".to_string()),
        ..Default::default()
    })
    .expect_err("an invalid explicit language tag must fail startup");

    assert!(
        error.to_string().contains("BEAM_METADATA_LANGUAGE"),
        "the error must name the knob the operator set: {error}"
    );
}

#[test]
fn a_valid_metadata_language_still_enables_the_provider() {
    let providers = providers_for(ServerConfig {
        tmdb_api_token: Some("tmdb-token".to_string()),
        anilist_enabled: false,
        metadata_language: Some("fr-FR".to_string()),
        ..Default::default()
    });
    assert_eq!(providers, vec!["tmdb".to_string()]);
}

#[test]
fn a_blank_metadata_language_is_treated_as_unset() {
    // Whitespace-only is what an operator gets from `BEAM_METADATA_LANGUAGE=`
    // in a compose file; it must not be forwarded to cameo as a tag.
    let providers = providers_for(ServerConfig {
        tmdb_api_token: Some("tmdb-token".to_string()),
        anilist_enabled: false,
        metadata_language: Some("   ".to_string()),
        ..Default::default()
    });
    assert_eq!(providers, vec!["tmdb".to_string()]);
}

mod build_failures {
    use super::*;

    fn failure() -> cameo::CameoClientError {
        cameo::CameoClientError::NotConfigured
    }

    #[test]
    fn a_build_failure_with_the_language_left_implicit_disables_enrichment() {
        // Nothing the operator asked for was ignored, so the server starts and
        // enrichment is simply off -- a warning, not a refusal to boot.
        let provider = provider_from_build(Err(failure()), None)
            .expect("an implicit configuration must not stop the server");
        assert!(provider.available_providers().is_empty());
    }

    #[test]
    fn a_build_failure_with_an_explicit_language_stops_startup() {
        // The other half of the same branch: an explicitly-set knob that
        // cannot be honoured must not be silently dropped on a headless
        // server.
        let error = provider_from_build(Err(failure()), Some("fr-FR"))
            .expect_err("an explicit knob that cannot be honoured must fail startup");
        assert!(
            error.to_string().contains("BEAM_METADATA_LANGUAGE"),
            "the error must name the knob the operator set: {error}"
        );
        assert!(error.to_string().contains("fr-FR"), "{error}");
    }

    #[test]
    fn no_configured_provider_is_not_a_failure() {
        let provider = provider_from_build(Ok(None), None).expect("a valid configuration");
        assert!(provider.available_providers().is_empty());
    }
}

mod uptime {
    use std::sync::Arc;
    use std::time::Duration;

    use beam_domain::services::TestClock;

    use crate::routes::test_support::make_app_state_with_clock;

    #[test]
    fn uptime_is_measured_from_when_the_state_was_built() {
        // `uptime_secs` feeds the health and admin-status endpoints. Read from
        // `Instant::now()` directly it is always ~0 in a test, so no assertion
        // about it could fail -- which is why the clock is injected.
        let clock = Arc::new(TestClock::new());
        let state = make_app_state_with_clock(|_| {}, clock.clone());

        assert_eq!(
            state.uptime_secs(),
            0,
            "a freshly built state has no uptime"
        );

        clock.advance(Duration::from_secs(3_600));
        assert_eq!(state.uptime_secs(), 3_600);

        clock.advance(Duration::from_secs(30));
        assert_eq!(state.uptime_secs(), 3_630, "uptime accumulates");
    }

    #[test]
    fn uptime_reports_whole_seconds_and_never_rounds_up() {
        let clock = Arc::new(TestClock::new());
        let state = make_app_state_with_clock(|_| {}, clock.clone());

        clock.advance(Duration::from_millis(1_999));
        assert_eq!(state.uptime_secs(), 1);
    }
}
