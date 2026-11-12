use super::*;
use crate::catalog::{LanguagePreferences, SubtitleMode};

/// What `GET`/`PUT /v1/me/language-preferences` answer once the server has
/// normalised the languages to the codes tracks are tagged with.
const STORED: &str = r#"{"audio_languages":["jpn","eng"],"subtitle_languages":["eng"],"subtitle_mode":"forced_only"}"#;

/// Storing preferences is a `PUT` of the record as given, and the answer is
/// the server's normalised copy, not the one sent: `ja` comes back as `jpn`.
#[tokio::test]
async fn setting_preferences_puts_them_and_answers_what_the_server_stored() {
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering(200, "application/json", STORED));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let stored = client
        .set_language_preferences(LanguagePreferences {
            audio_languages: vec!["ja".to_owned(), "English".to_owned()],
            subtitle_languages: vec!["en".to_owned()],
            subtitle_mode: SubtitleMode::ForcedOnly,
        })
        .await
        .expect("the canned answer decodes");

    assert_eq!(
        stored,
        LanguagePreferences {
            audio_languages: vec!["jpn".to_owned(), "eng".to_owned()],
            subtitle_languages: vec!["eng".to_owned()],
            subtitle_mode: SubtitleMode::ForcedOnly,
        }
    );
    let recorded = backend.recorded();
    assert_eq!(recorded[0].method, reqwest::Method::PUT);
    assert_eq!(recorded[0].url.path(), "/v1/me/language-preferences");
    let sent: serde_json::Value =
        serde_json::from_slice(&recorded[0].body).expect("the body is JSON");
    assert_eq!(
        sent,
        serde_json::json!({
            "audio_languages": ["ja", "English"],
            "subtitle_languages": ["en"],
            "subtitle_mode": "forced_only",
        })
    );
}

/// Every subtitle mode reaches the platform as itself, read with a `GET`.
#[tokio::test]
async fn reading_preferences_carries_each_subtitle_mode() {
    for (body, mode) in [
        (
            r#"{"audio_languages":[],"subtitle_languages":[],"subtitle_mode":"off"}"#,
            SubtitleMode::Off,
        ),
        (
            r#"{"audio_languages":[],"subtitle_languages":[],"subtitle_mode":"forced_only"}"#,
            SubtitleMode::ForcedOnly,
        ),
        (
            r#"{"audio_languages":[],"subtitle_languages":[],"subtitle_mode":"on"}"#,
            SubtitleMode::On,
        ),
    ] {
        let (client, id, _) = signed_in_client().await;
        let backend = Arc::new(CannedBackend::answering(200, "application/json", body));
        client
            .use_transport(
                &id,
                Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
            )
            .expect("the server is registered");

        let read = client
            .language_preferences()
            .await
            .expect("the canned answer decodes");

        assert_eq!(read.subtitle_mode, mode, "{body}");
        assert!(read.audio_languages.is_empty());
        let recorded = backend.recorded();
        assert_eq!(recorded[0].method, reqwest::Method::GET);
        assert_eq!(recorded[0].url.path(), "/v1/me/language-preferences");
    }
}

/// A language the server does not know is its 422, refused as the request's
/// fault, so a settings screen can say so rather than offer a retry.
#[tokio::test]
async fn a_refused_language_is_a_bad_request() {
    const REFUSED: &str = r#"{"type":"https://beam.justinchung.net/reference/errors/#validation-failed","title":"Validation failed","status":422,"detail":"the request body breaks 1 rule(s)"}"#;
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering(
        422,
        "application/problem+json",
        REFUSED,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let refused = client
        .set_language_preferences(LanguagePreferences {
            audio_languages: vec!["klingon".to_owned()],
            subtitle_languages: Vec::new(),
            subtitle_mode: SubtitleMode::Off,
        })
        .await;

    assert!(
        matches!(
            refused,
            Err(BeamError::Server {
                status: 422,
                retryable: false,
                ..
            })
        ),
        "{refused:?}"
    );
}
