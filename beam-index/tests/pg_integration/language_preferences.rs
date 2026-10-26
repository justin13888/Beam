//! The shared `LanguagePreferenceRepository` contract, run against real SQL:
//! the upsert on the user, the user foreign key, and the `CHECK`s holding
//! the stored codes and mode (issue #264).

use std::sync::Arc;

use uuid::Uuid as FixtureUuid;

use beam_domain::repositories::LanguagePreferenceRepository;
use beam_domain::repositories::contract::fixture::LanguagePreferenceFixture;
use beam_index::repositories::SqlLanguagePreferenceRepository;
use beam_test_support::{postgres, seed};

struct PgFixture {
    repo: SqlLanguagePreferenceRepository,
    db: Arc<sea_orm::DatabaseConnection>,
}

#[async_trait::async_trait]
impl LanguagePreferenceFixture for PgFixture {
    fn repo(&self) -> &dyn LanguagePreferenceRepository {
        &self.repo
    }

    async fn new_user(&self) -> FixtureUuid {
        seed::user(&self.db).await.expect("seed a user row")
    }
}

async fn setup() -> PgFixture {
    let db = postgres::connection().await;
    PgFixture {
        repo: SqlLanguagePreferenceRepository::new(db.clone()),
        db,
    }
}

beam_domain::language_preference_repository_contract!(setup);

/// The table refuses what the domain never stores: a code that is not three
/// lowercase letters, a list past sixteen, and a mode it does not name.
#[tokio::test]
async fn the_table_refuses_what_no_preference_holds() {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let fixture = setup().await;
    let user = fixture.new_user().await;
    let insert = |audio: &str, mode: &str| {
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "INSERT INTO user_language_preferences \
                     (user_id, audio_languages, subtitle_languages, subtitle_mode, updated_at) \
                 VALUES ($1, {audio}, '{{}}', $2, now())"
            ),
            vec![user.into(), mode.into()],
        )
    };

    let seventeen = format!(
        "ARRAY[{}]",
        (0..17)
            .map(|i| format!("'q{}{}'", (b'a' + i / 26) as char, (b'a' + i % 26) as char))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (audio, mode) in [
        ("ARRAY['en']", "on"),
        ("ARRAY['english']", "on"),
        ("ARRAY['ENG']", "on"),
        ("ARRAY['en', 'g']", "on"),
        ("ARRAY['eng,jpn']", "on"),
        ("ARRAY['eng', NULL]", "on"),
        ("ARRAY[NULL]::TEXT[]", "on"),
        (seventeen.as_str(), "on"),
        ("ARRAY['eng']", "sometimes"),
    ] {
        assert!(
            fixture.db.execute_raw(insert(audio, mode)).await.is_err(),
            "{audio} with {mode} was stored"
        );
    }
    fixture
        .db
        .execute_raw(insert("ARRAY['eng', 'jpn']", "forced_only"))
        .await
        .expect("a well-formed row is stored");
}

/// A user's preferences go with the user.
#[tokio::test]
async fn preferences_go_with_their_user() {
    use beam_domain::models::language_preferences::SubtitleMode;
    use beam_domain::models::language_preferences::test_utils::prefs;
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let fixture = setup().await;
    let user = fixture.new_user().await;
    fixture
        .repo
        .replace(user, &prefs(&["eng"], &["eng"], SubtitleMode::On))
        .await
        .unwrap();

    fixture
        .db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM users WHERE id = $1",
            vec![user.into()],
        ))
        .await
        .unwrap();

    let left = fixture
        .db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM user_language_preferences WHERE user_id = $1",
            vec![user.into()],
        ))
        .await
        .unwrap()
        .expect("a count row");
    assert_eq!(left.try_get::<i64>("", "n").unwrap(), 0);
}
