/// Behavioural contract for [`crate::utils::session_store::SessionStore`].
///
/// `$setup` names an `async fn() -> impl SessionStoreFixture`.
///
/// Every expiry rule here is exercised by advancing the injected clock, never
/// by sleeping: a test that waited out a real idle TTL would take hours, which
/// is why these paths had no tests at all before.
#[macro_export]
macro_rules! session_store_contract {
    ($setup:path) => {
        use ::std::time::Duration;
        use $crate::utils::contract::fixture::SessionStoreFixture as _;
        use $crate::utils::session_store::{
            SESSION_TOUCH_THROTTLE_SECS, SessionData, SessionError,
        };

        const IDLE_TTL: u64 = 14 * 24 * 3600;
        const ABSOLUTE_TTL: u64 = 60 * 24 * 3600;

        fn session_for(user_id: &::uuid::Uuid) -> SessionData {
            $crate::utils::session_store::test_utils::session_for(*user_id)
        }

        #[tokio::test]
        async fn a_malformed_user_id_is_refused_as_invalid() {
            // Every method keyed by a user id parses it before it touches the
            // store, so a malformed one is a typed error everywhere -- never a
            // silent miss that reads as "this user has no sessions".
            let fixture = $setup().await;
            let malformed = "not-a-uuid";

            let created = fixture
                .store()
                .create(
                    &SessionData {
                        user_id: malformed.to_string(),
                        ..session_for(&::uuid::Uuid::new_v4())
                    },
                    IDLE_TTL,
                    ABSOLUTE_TTL,
                )
                .await;
            assert!(
                matches!(created, Err(SessionError::InvalidId(_))),
                "create: {created:?}"
            );

            let listed = fixture.store().list_for_user(malformed).await;
            assert!(
                matches!(listed, Err(SessionError::InvalidId(_))),
                "list_for_user: {listed:?}"
            );

            let revoked_all = fixture.store().delete_all_for_user(malformed).await;
            assert!(
                matches!(revoked_all, Err(SessionError::InvalidId(_))),
                "delete_all_for_user: {revoked_all:?}"
            );

            let revoked = fixture
                .store()
                .delete_by_id(::uuid::Uuid::new_v4(), malformed)
                .await;
            assert!(
                matches!(revoked, Err(SessionError::InvalidId(_))),
                "delete_by_id: {revoked:?}"
            );
        }

        #[tokio::test]
        async fn a_created_session_resolves_from_its_token() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;
            let created_at = fixture.store().now().timestamp();

            let token = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();

            let session = fixture
                .store()
                .get(&token)
                .await
                .unwrap()
                .expect("the session just created resolves");
            assert_eq!(session.user_id, user.to_string());
            assert_eq!(session.ip, session_for(&user).ip);
            assert_eq!(session.device_hash, session_for(&user).device_hash);
            assert_eq!(
                session.created_at, created_at,
                "the store stamps creation time itself; it does not echo the caller's"
            );
        }

        #[tokio::test]
        async fn an_unknown_token_resolves_to_nothing() {
            let fixture = $setup().await;
            assert!(
                fixture
                    .store()
                    .get("not-a-real-token")
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn two_sessions_never_share_a_token() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;

            let first = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();
            let second = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();

            assert_ne!(
                first, second,
                "session tokens must be unguessable, not reused"
            );
        }

        #[tokio::test]
        async fn a_session_stops_resolving_once_the_idle_window_lapses() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;
            let token = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();

            fixture.clock().advance(Duration::from_secs(IDLE_TTL - 1));
            assert!(
                fixture.store().get(&token).await.unwrap().is_some(),
                "one second inside the idle window is still valid"
            );

            fixture.clock().advance(Duration::from_secs(2));
            assert!(
                fixture.store().get(&token).await.unwrap().is_none(),
                "past the idle window the session is gone"
            );
        }

        #[tokio::test]
        async fn touching_slides_the_idle_window_forward() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;
            let token = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();

            // Most of the way through the idle window, then touched.
            fixture.clock().advance(Duration::from_secs(IDLE_TTL - 10));
            fixture.store().touch(&token, IDLE_TTL).await.unwrap();

            // Past where the original window would have ended.
            fixture.clock().advance(Duration::from_secs(20));
            assert!(
                fixture.store().get(&token).await.unwrap().is_some(),
                "the touch must have moved the deadline, not merely rewritten it"
            );
        }

        #[tokio::test]
        async fn touching_never_pushes_a_session_past_its_absolute_ceiling() {
            // The whole point of the absolute TTL: an attacker holding a stolen
            // cookie must not be able to keep it alive indefinitely by using it.
            let fixture = $setup().await;
            let user = fixture.new_user().await;
            let short_absolute = 3600;
            let token = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, short_absolute)
                .await
                .unwrap();

            fixture
                .clock()
                .advance(Duration::from_secs(short_absolute - 1));
            fixture.store().touch(&token, IDLE_TTL).await.unwrap();

            fixture.clock().advance(Duration::from_secs(2));
            assert!(
                fixture.store().get(&token).await.unwrap().is_none(),
                "touching must not extend a session past its absolute expiry"
            );
        }

        #[tokio::test]
        async fn touching_an_unknown_token_is_not_an_error() {
            let fixture = $setup().await;
            fixture
                .store()
                .touch("not-a-real-token", IDLE_TTL)
                .await
                .expect("there is simply nothing to touch");
        }

        #[tokio::test]
        async fn deleting_a_session_revokes_it_immediately() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;
            let token = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();

            fixture.store().delete(&token).await.unwrap();

            assert!(fixture.store().get(&token).await.unwrap().is_none());
        }

        #[tokio::test]
        async fn deleting_all_for_a_user_leaves_other_users_signed_in() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;
            let other = fixture.new_user().await;

            let a = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();
            let b = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();
            let untouched = fixture
                .store()
                .create(&session_for(&other), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();

            let revoked = fixture
                .store()
                .delete_all_for_user(&user.to_string())
                .await
                .unwrap();

            assert_eq!(revoked, 2);
            assert!(fixture.store().get(&a).await.unwrap().is_none());
            assert!(fixture.store().get(&b).await.unwrap().is_none());
            assert!(
                fixture.store().get(&untouched).await.unwrap().is_some(),
                "revoking one user's sessions must not touch another's"
            );
        }

        #[tokio::test]
        async fn listing_shows_a_users_live_sessions_only() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;
            let other = fixture.new_user().await;

            fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();
            fixture
                .store()
                .create(&session_for(&other), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();
            let expiring = fixture
                .store()
                .create(&session_for(&user), 60, ABSOLUTE_TTL)
                .await
                .unwrap();

            assert_eq!(
                fixture
                    .store()
                    .list_for_user(&user.to_string())
                    .await
                    .unwrap()
                    .len(),
                2
            );

            fixture.clock().advance(Duration::from_secs(61));
            let live = fixture
                .store()
                .list_for_user(&user.to_string())
                .await
                .unwrap();
            assert_eq!(
                live.len(),
                1,
                "an expired session must not be offered for revocation"
            );
            assert!(fixture.store().get(&expiring).await.unwrap().is_none());
        }

        #[tokio::test]
        async fn a_session_can_be_revoked_by_id_only_by_its_owner() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;
            let attacker = fixture.new_user().await;
            let token = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();

            let (id, _) = fixture
                .store()
                .list_for_user(&user.to_string())
                .await
                .unwrap()
                .into_iter()
                .next()
                .expect("the session is listed");

            assert!(
                !fixture
                    .store()
                    .delete_by_id(id, &attacker.to_string())
                    .await
                    .unwrap(),
                "one user must not revoke another's session by guessing its id"
            );
            assert!(
                fixture.store().get(&token).await.unwrap().is_some(),
                "the session survives the attempt"
            );

            assert!(
                fixture
                    .store()
                    .delete_by_id(id, &user.to_string())
                    .await
                    .unwrap()
            );
            assert!(fixture.store().get(&token).await.unwrap().is_none());
        }

        #[tokio::test]
        async fn get_and_touch_slides_the_window_only_past_the_throttle() {
            use $crate::utils::session_store::get_and_touch;

            let fixture = $setup().await;
            let user = fixture.new_user().await;
            let token = fixture
                .store()
                .create(&session_for(&user), IDLE_TTL, ABSOLUTE_TTL)
                .await
                .unwrap();

            let created = fixture
                .store()
                .get(&token)
                .await
                .unwrap()
                .expect("just created")
                .last_active;

            // Exactly at the throttle: the comparison is strict, so this is
            // still "too soon". The boundary is the only input that tells a
            // `>` apart from a `>=`.
            fixture
                .clock()
                .advance(Duration::from_secs(SESSION_TOUCH_THROTTLE_SECS as u64));
            assert!(
                get_and_touch(fixture.store(), &token, IDLE_TTL)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                fixture
                    .store()
                    .get(&token)
                    .await
                    .unwrap()
                    .expect("still valid")
                    .last_active,
                created,
                "at exactly the throttle window the session must not be rewritten"
            );
            assert!(
                get_and_touch(fixture.store(), &token, IDLE_TTL)
                    .await
                    .unwrap()
                    .is_some()
            );
            let after_no_touch = fixture
                .store()
                .get(&token)
                .await
                .unwrap()
                .expect("still valid");
            assert_eq!(
                after_no_touch.last_active, created,
                "inside the throttle window the session must not be rewritten"
            );

            // Past the throttle: resolves and slides.
            fixture.clock().advance(Duration::from_secs(1));
            assert!(
                get_and_touch(fixture.store(), &token, IDLE_TTL)
                    .await
                    .unwrap()
                    .is_some()
            );
            let after_touch = fixture
                .store()
                .get(&token)
                .await
                .unwrap()
                .expect("still valid");
            assert!(
                after_touch.last_active > created,
                "past the throttle the session's activity is recorded"
            );
        }

        #[tokio::test]
        async fn get_and_touch_on_an_unknown_token_resolves_to_nothing() {
            use $crate::utils::session_store::get_and_touch;

            let fixture = $setup().await;
            assert!(
                get_and_touch(fixture.store(), "not-a-real-token", IDLE_TTL)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    };
}
