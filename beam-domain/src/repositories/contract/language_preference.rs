/// Behavioural contract for
/// [`crate::repositories::LanguagePreferenceRepository`]: a user who set
/// nothing reads as the default, preferences read back as they were
/// replaced -- every list in its order -- users are kept apart, and
/// preferences for a user that does not exist are refused (issue #264).
///
/// `$setup` names an `async fn() -> impl LanguagePreferenceFixture`.
#[macro_export]
macro_rules! language_preference_repository_contract {
    ($setup:path) => {
        use ::uuid::Uuid;
        use $crate::models::language_preferences::test_utils::prefs;
        use $crate::models::language_preferences::{LanguagePreferences, SubtitleMode};
        use $crate::repositories::contract::fixture::LanguagePreferenceFixture as _;

        #[tokio::test]
        async fn a_user_who_set_nothing_has_the_defaults() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;

            assert_eq!(
                fixture.repo().find(user).await.unwrap(),
                LanguagePreferences::default()
            );
            assert_eq!(
                fixture.repo().find(Uuid::new_v4()).await.unwrap(),
                LanguagePreferences::default(),
                "an id no user has"
            );
        }

        #[tokio::test]
        async fn preferences_read_back_as_they_were_replaced() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;

            for preferences in [
                prefs(&["jpn", "eng"], &["eng", "spa", "fre"], SubtitleMode::On),
                prefs(&["eng", "jpn"], &[], SubtitleMode::Off),
                prefs(&[], &["fre", "eng", "spa"], SubtitleMode::ForcedOnly),
                LanguagePreferences::default(),
            ] {
                repo.replace(user, &preferences).await.unwrap();
                assert_eq!(repo.find(user).await.unwrap(), preferences);
            }
        }

        #[tokio::test]
        async fn one_users_preferences_leave_anothers_alone() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (first, second) = (fixture.new_user().await, fixture.new_user().await);
            let seconds = prefs(&["kor"], &["kor"], SubtitleMode::On);
            repo.replace(second, &seconds).await.unwrap();

            repo.replace(first, &prefs(&["eng"], &[], SubtitleMode::Off))
                .await
                .unwrap();

            assert_eq!(repo.find(second).await.unwrap(), seconds);
        }

        #[tokio::test]
        async fn preferences_for_a_missing_user_are_refused() {
            let fixture = $setup().await;
            let missing = Uuid::new_v4();

            assert!(
                fixture
                    .repo()
                    .replace(missing, &prefs(&["eng"], &[], SubtitleMode::On))
                    .await
                    .is_err()
            );
            assert_eq!(
                fixture.repo().find(missing).await.unwrap(),
                LanguagePreferences::default()
            );
        }
    };
}
