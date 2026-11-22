use super::*;

use beam_domain::models::language_preferences::test_utils::prefs;
use beam_domain::models::language_preferences::{LanguagePreferences, SubtitleMode};
use beam_domain::repositories::LanguagePreferenceRepository;

use crate::repositories::SqlLanguagePreferenceRepository;

/// The read is one row by the user's id; no row is the defaults
/// (issue #264).
#[tokio::test]
async fn preferences_are_read_by_the_users_id() {
    let user = Uuid::from_u128(0x64);
    let db = connection(empty_mock());
    let repo = SqlLanguagePreferenceRepository::new(db.clone());
    assert_eq!(
        repo.find(user).await.unwrap(),
        LanguagePreferences::default()
    );
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "{sql:?}");
    assert_filters(&sql[0], "user_language_preferences", "user_id", "=");
    assert_bound(&sql[0], &user.to_string());
}

/// A stored row reads back in its order, and one holding what the
/// table's checks should have refused is an error, not a guess.
#[tokio::test]
async fn a_stored_row_reads_back_and_a_corrupt_one_is_an_error() {
    let stored = |mode: &str, audio: &[&str]| {
        row([
            ("user_id", Value::from(Uuid::from_u128(0x64))),
            (
                "audio_languages",
                Value::from(
                    audio
                        .iter()
                        .map(|code| (*code).to_owned())
                        .collect::<Vec<_>>(),
                ),
            ),
            (
                "subtitle_languages",
                Value::from(vec!["spa".to_owned(), "eng".to_owned()]),
            ),
            ("subtitle_mode", Value::from(mode)),
            (
                "updated_at",
                Value::from(chrono::DateTime::parse_from_rfc3339("2026-10-26T00:00:00Z").unwrap()),
            ),
        ])
    };
    let db = connection(
        MockDatabase::new(DbBackend::Postgres).append_query_results([
            vec![stored("on", &["jpn", "eng"])],
            vec![stored("sometimes", &["jpn"])],
            vec![stored("off", &["j1n"])],
        ]),
    );
    let repo = SqlLanguagePreferenceRepository::new(db);
    let user = Uuid::from_u128(0x64);

    assert_eq!(
        repo.find(user).await.unwrap(),
        prefs(&["jpn", "eng"], &["spa", "eng"], SubtitleMode::On)
    );
    assert!(repo.find(user).await.is_err(), "an unknown mode");
    assert!(repo.find(user).await.is_err(), "an unknown language");
}

/// A replacement upserts on the user and writes every column it owns,
/// so a second save replaces the first whole.
#[tokio::test]
async fn a_replacement_upserts_every_column_on_the_user() {
    let user = Uuid::from_u128(0x65);
    let db = connection(empty_mock());
    let repo = SqlLanguagePreferenceRepository::new(db.clone());
    let _ = repo
        .replace(
            user,
            &prefs(&["jpn"], &["eng", "fre"], SubtitleMode::ForcedOnly),
        )
        .await;
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "{sql:?}");
    assert_contains(&sql[0], r#"ON CONFLICT ("user_id") DO UPDATE SET"#);
    for column in [
        "audio_languages",
        "subtitle_languages",
        "subtitle_mode",
        "updated_at",
    ] {
        assert_contains(&sql[0], &format!(r#""{column}" = "excluded"."{column}""#));
    }
    assert_bound(&sql[0], &user.to_string());
    assert_bound(&sql[0], "forced_only");
    assert_bound(&sql[0], "fre");
}
