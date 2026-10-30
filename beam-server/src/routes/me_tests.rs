//! Subcutaneous tests for `/v1/me/language-preferences` (issue #264): the
//! real router and handlers over the in-memory preference store, each case
//! asserting the response and what a later read returns.

use std::sync::Arc;

use beam_auth::utils::session_store::SessionData;
use beam_domain::models::language_preferences::test_utils::prefs;
use beam_domain::models::language_preferences::{self, LanguagePreferences};
use beam_domain::repositories::LanguagePreferenceRepository;
use beam_domain::repositories::language_preferences::in_memory::InMemoryLanguagePreferenceRepository;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::models::{LanguagePreferenceSettings, SubtitleMode};
use crate::routes::me::{get_language_preferences, set_language_preferences};
use crate::routes::test_support::state_with_language_preferences;
use crate::state::AppState;

const PATH: &str = "/v1/me/language-preferences";

struct Fixture {
    state: AppState,
    store: Arc<InMemoryLanguagePreferenceRepository>,
    client: TestClient<AppState>,
}

fn fixture_over(store: InMemoryLanguagePreferenceRepository) -> Fixture {
    let store = Arc::new(store);
    let state = state_with_language_preferences(store.clone());
    let service = Router::new()
        .nest(
            "/v1",
            Router::new().mount(kynos::routes![
                get_language_preferences,
                set_language_preferences,
            ]),
        )
        .build(state.clone())
        .expect("the preferences router describes itself");
    Fixture {
        state,
        store,
        client: TestClient::new(service),
    }
}

fn fixture() -> Fixture {
    fixture_over(InMemoryLanguagePreferenceRepository::default())
}

/// A session for a new user, and that user's id.
async fn session(fixture: &Fixture) -> (Uuid, String) {
    let user = Uuid::new_v4();
    let token = fixture
        .state
        .services
        .session_store
        .create(
            &SessionData {
                user_id: user.to_string(),
                device_hash: "test-device".to_owned(),
                ip: "127.0.0.1".to_owned(),
                created_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
            },
            86_400,
            86_400,
        )
        .await
        .expect("the in-memory session store issues a session");
    (user, token)
}

async fn read(fixture: &Fixture, token: &str) -> LanguagePreferenceSettings {
    fixture
        .client
        .get(PATH)
        .cookie("beam_session", token)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

#[tokio::test]
async fn both_operations_need_a_session() {
    let fixture = fixture();
    fixture
        .client
        .get(PATH)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    fixture
        .client
        .put(PATH)
        .json(&json!({
            "audio_languages": [],
            "subtitle_languages": [],
            "subtitle_mode": "off",
        }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_user_who_set_nothing_reads_the_defaults() {
    let fixture = fixture();
    let (_, token) = session(&fixture).await;

    assert_eq!(
        read(&fixture, &token).await,
        LanguagePreferenceSettings {
            audio_languages: vec![],
            subtitle_languages: vec![],
            subtitle_mode: SubtitleMode::ForcedOnly,
        }
    );
}

/// Every spelling is answered and stored as the code tracks carry, a repeat
/// keeps its first place, and the next read -- and only this user's --
/// returns it.
#[tokio::test]
async fn a_replacement_is_stored_as_track_codes_and_read_back() {
    let fixture = fixture();
    let (user, token) = session(&fixture).await;
    let (other, other_token) = session(&fixture).await;

    let answered: LanguagePreferenceSettings = fixture
        .client
        .put(PATH)
        .cookie("beam_session", &token)
        .json(&json!({
            "audio_languages": ["ja", "German", "deu", "eng"],
            "subtitle_languages": ["pt-BR", "EN"],
            "subtitle_mode": "on",
        }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    let expected = LanguagePreferenceSettings {
        audio_languages: vec!["jpn".into(), "ger".into(), "eng".into()],
        subtitle_languages: vec!["por".into(), "eng".into()],
        subtitle_mode: SubtitleMode::On,
    };
    assert_eq!(answered, expected, "what PUT answers");
    assert_eq!(read(&fixture, &token).await, expected, "what GET reads");
    assert_eq!(
        fixture.store.find(user).await.unwrap(),
        prefs(
            &["jpn", "ger", "eng"],
            &["por", "eng"],
            language_preferences::SubtitleMode::On
        ),
        "what was stored"
    );
    assert_eq!(
        fixture.store.find(other).await.unwrap(),
        LanguagePreferences::default(),
        "another user's are untouched"
    );
    assert_eq!(
        read(&fixture, &other_token).await.subtitle_mode,
        SubtitleMode::ForcedOnly
    );
}

#[tokio::test]
async fn a_name_that_is_no_language_is_422_naming_it_and_changes_nothing() {
    let fixture = fixture();
    let (user, token) = session(&fixture).await;
    let stored = prefs(&["eng"], &[], language_preferences::SubtitleMode::Off);
    fixture.store.replace(user, &stored).await.unwrap();

    let problem: Value = fixture
        .client
        .put(PATH)
        .cookie("beam_session", &token)
        .json(&json!({
            "audio_languages": ["jpn", "klingonese", "und"],
            "subtitle_languages": ["", "eng"],
            "subtitle_mode": "on",
        }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#validation-failed")
        .json();
    let pointers: Vec<&str> = problem["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("an errors list: {problem}"))
        .iter()
        .map(|error| error["pointer"].as_str().unwrap())
        .collect();
    assert_eq!(
        pointers,
        [
            "/audio_languages/1",
            "/audio_languages/2",
            "/subtitle_languages/0"
        ]
    );
    assert_eq!(fixture.store.find(user).await.unwrap(), stored);
}

/// A list past sixteen entries is refused by the schema's bound before the
/// handler runs, so nothing is stored.
#[tokio::test]
async fn a_list_past_its_bound_is_refused_and_changes_nothing() {
    let fixture = fixture();
    let (user, token) = session(&fixture).await;
    let seventeen: Vec<String> = (0..17u8)
        .map(|i| {
            format!(
                "q{}{}",
                char::from(b'a' + i / 26),
                char::from(b'a' + i % 26)
            )
        })
        .collect();

    let response = fixture
        .client
        .put(PATH)
        .cookie("beam_session", &token)
        .json(&json!({
            "audio_languages": [],
            "subtitle_languages": seventeen,
            "subtitle_mode": "on",
        }))
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        fixture.store.find(user).await.unwrap(),
        LanguagePreferences::default()
    );
}

/// A store that refuses the write is a 500, never a 200 claiming what was
/// not kept.
#[tokio::test]
async fn a_failed_write_is_a_500() {
    // Knows no user, so every write fails as the store's foreign key would.
    let fixture = fixture_over(InMemoryLanguagePreferenceRepository::enforcing_users());
    let (_, token) = session(&fixture).await;

    fixture
        .client
        .put(PATH)
        .cookie("beam_session", &token)
        .json(&json!({
            "audio_languages": ["eng"],
            "subtitle_languages": [],
            "subtitle_mode": "off",
        }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
}

/// A preference store that fails every call: what a database outage looks
/// like from the handlers.
#[derive(Debug)]
struct FailingPreferences;

#[async_trait::async_trait]
impl LanguagePreferenceRepository for FailingPreferences {
    async fn find(&self, _: Uuid) -> Result<LanguagePreferences, sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(
            "preference store unavailable".into(),
        ))
    }
    async fn replace(&self, _: Uuid, _: &LanguagePreferences) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(
            "preference store unavailable".into(),
        ))
    }
}

/// NFR-205: a store that cannot read is a 500, never the defaults -- which a
/// client would apply as though the user had chosen nothing.
#[tokio::test]
async fn a_preference_store_failure_is_a_500_never_the_defaults() {
    let state = state_with_language_preferences(Arc::new(FailingPreferences));
    let service = Router::new()
        .nest(
            "/v1",
            Router::new().mount(kynos::routes![get_language_preferences]),
        )
        .build(state.clone())
        .expect("the preferences router describes itself");
    let client = TestClient::new(service);
    let token = state
        .services
        .session_store
        .create(
            &SessionData {
                user_id: Uuid::new_v4().to_string(),
                device_hash: "test-device".to_owned(),
                ip: "127.0.0.1".to_owned(),
                created_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
            },
            86_400,
            86_400,
        )
        .await
        .expect("the in-memory session store issues a session");

    let response = client.get(PATH).cookie("beam_session", &token).send().await;
    response
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
    assert!(
        !response.text().contains("subtitle_mode"),
        "no preferences body accompanies the failure"
    );
}

/// The schema's `maxItems` on each list is the domain's
/// `MAX_PREFERRED_LANGUAGES`. The handler relies on it: Kynos refuses a longer
/// list before the handler runs, so the domain's own bound can never refuse a
/// request that reached it, and the handler treats that as a server fault.
/// Read from the served document, so the two cannot drift apart unnoticed.
#[test]
fn the_documented_list_bound_is_the_domain_bound() {
    use kynos::openapi::SpecVersion;

    let document = crate::routes::create_router(&crate::bootstrap::AllowedOrigins::none())
        .openapi_as(SpecVersion::V3_2)
        .expect("the contract exports");
    let document = serde_json::to_value(&document).expect("the document serializes");
    let properties = &document["components"]["schemas"]["LanguagePreferenceSettings"]["properties"];
    for list in ["audio_languages", "subtitle_languages"] {
        assert_eq!(
            properties[list]["maxItems"].as_u64(),
            Some(language_preferences::MAX_PREFERRED_LANGUAGES as u64),
            "{list}'s documented bound is the domain's"
        );
    }
}
