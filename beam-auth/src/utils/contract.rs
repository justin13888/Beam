//! Shared behavioural contract for [`crate::utils::repository::UserRepository`].
//!
//! Instantiated over the in-memory double (hermetic, always run) and, under
//! the opt-in `pg-integration` feature, over `SqlUserRepository` against a real
//! Postgres. This is what makes the double legitimate: the same assertions
//! constrain both, so a divergence fails rather than drifting silently. Without
//! it the tests here drove a `HashMap` and proved nothing about production.

/// Fixture the contract is written against.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod fixture {
    use beam_domain::services::TestClock;

    use crate::utils::repository::UserRepository;

    use crate::utils::pending_auth_store::PendingAuthStore;
    use crate::utils::session_store::SessionStore;

    #[async_trait::async_trait]
    pub trait UserRepositoryFixture: Send + Sync {
        /// The repository under contract, holding no users yet.
        fn repo(&self) -> &dyn UserRepository;

        /// The clock the repository stamps `created_at` from.
        fn clock(&self) -> &TestClock;
    }

    #[async_trait::async_trait]
    pub trait SessionStoreFixture: Send + Sync {
        /// The store under contract, holding no sessions yet.
        fn store(&self) -> &dyn SessionStore;

        /// The clock the store reads every expiry against.
        fn clock(&self) -> &TestClock;

        /// A user that exists as far as the backing store is concerned. A real
        /// Postgres enforces the `sessions.user_id` foreign key; the in-memory
        /// store does not, so the fixture owns the difference.
        async fn new_user(&self) -> uuid::Uuid;
    }

    #[async_trait::async_trait]
    pub trait PendingAuthStoreFixture: Send + Sync {
        /// The store under contract, holding no pending authorizations yet.
        fn store(&self) -> &dyn PendingAuthStore;

        /// The clock the store reads the TTL against.
        fn clock(&self) -> &TestClock;
    }
}

/// Behavioural contract for [`crate::utils::repository::UserRepository`].
///
/// `$setup` names an `async fn() -> impl UserRepositoryFixture`.
#[macro_export]
macro_rules! user_repository_contract {
    ($setup:path) => {
        use ::std::time::Duration;
        use ::uuid::Uuid;
        use $crate::utils::contract::fixture::UserRepositoryFixture as _;
        use $crate::utils::models::CreateUser;

        /// A JIT-provisioning request from one identity provider.
        fn create_user(subject: &str, name: &str) -> CreateUser {
            CreateUser {
                oidc_issuer: format!("https://idp.test/{}", Uuid::new_v4()),
                oidc_subject: subject.to_string(),
                email: None,
                display_name: name.to_string(),
                avatar_url: None,
                is_admin: false,
            }
        }

        #[tokio::test]
        async fn a_new_user_is_enabled_and_not_an_admin() {
            let fixture = $setup().await;
            let user = fixture
                .repo()
                .create(create_user("s1", "Alice"))
                .await
                .unwrap();

            assert!(!user.disabled, "JIT-provisioned users must start enabled");
            assert!(
                !user.is_admin,
                "admin is granted from the IdP claim, never at provisioning"
            );
        }

        #[tokio::test]
        async fn a_user_is_found_again_by_the_identity_they_were_created_with() {
            let fixture = $setup().await;
            let create = create_user("s1", "Alice");
            let issuer = create.oidc_issuer.clone();
            let created = fixture.repo().create(create).await.unwrap();

            let found = fixture
                .repo()
                .find_by_oidc_identity(&issuer, "s1")
                .await
                .unwrap()
                .expect("the identity just provisioned resolves");
            assert_eq!(found.id, created.id);

            assert!(
                fixture
                    .repo()
                    .find_by_oidc_identity(&issuer, "someone-else")
                    .await
                    .unwrap()
                    .is_none(),
                "a different subject under the same issuer is a different user"
            );
            assert!(
                fixture
                    .repo()
                    .find_by_oidc_identity("https://other.idp.test", "s1")
                    .await
                    .unwrap()
                    .is_none(),
                "the same subject under a different issuer is a different user"
            );
        }

        #[tokio::test]
        async fn find_by_id_is_none_for_an_unknown_user() {
            let fixture = $setup().await;
            assert!(
                fixture
                    .repo()
                    .find_by_id(Uuid::new_v4())
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn count_tracks_created_users() {
            let fixture = $setup().await;
            let before = fixture.repo().count().await.unwrap();

            fixture
                .repo()
                .create(create_user("s1", "Alice"))
                .await
                .unwrap();
            fixture
                .repo()
                .create(create_user("s2", "Bob"))
                .await
                .unwrap();

            assert_eq!(fixture.repo().count().await.unwrap(), before + 2);
        }

        #[tokio::test]
        async fn list_page_orders_oldest_first_and_slices_without_gaps_or_repeats() {
            let fixture = $setup().await;
            let mut created = Vec::new();
            for (subject, name) in [("s1", "Alice"), ("s2", "Bob"), ("s3", "Carol")] {
                created.push(
                    fixture
                        .repo()
                        .create(create_user(subject, name))
                        .await
                        .unwrap()
                        .id,
                );
                // Distinct `created_at` values, so the ordering is the
                // property under test rather than the id tie-break.
                fixture.clock().advance(Duration::from_secs(60));
            }

            let ids = |page: Vec<$crate::utils::models::User>| -> Vec<Uuid> {
                page.into_iter().map(|u| u.id).collect()
            };

            assert_eq!(
                ids(fixture.repo().list_page(10, 0).await.unwrap()),
                created,
                "oldest first"
            );
            assert_eq!(
                ids(fixture.repo().list_page(2, 0).await.unwrap()),
                created[..2].to_vec()
            );
            assert_eq!(
                ids(fixture.repo().list_page(2, 2).await.unwrap()),
                created[2..].to_vec()
            );
            assert!(
                fixture.repo().list_page(10, 3).await.unwrap().is_empty(),
                "an offset past the end is empty, not a wrap-around"
            );
        }

        #[tokio::test]
        async fn set_disabled_toggles_the_flag_and_ignores_an_unknown_id() {
            let fixture = $setup().await;
            let user = fixture
                .repo()
                .create(create_user("s1", "Alice"))
                .await
                .unwrap();

            fixture.repo().set_disabled(user.id, true).await.unwrap();
            assert!(
                fixture
                    .repo()
                    .find_by_id(user.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .disabled
            );

            fixture.repo().set_disabled(user.id, false).await.unwrap();
            assert!(
                !fixture
                    .repo()
                    .find_by_id(user.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .disabled
            );

            fixture
                .repo()
                .set_disabled(Uuid::new_v4(), true)
                .await
                .expect("an unknown id is a silent no-op, never an error");
        }

        #[tokio::test]
        async fn set_admin_grants_and_revokes_and_ignores_an_unknown_id() {
            // Admin is recomputed from the IdP claim on every login, so
            // revoking has to work as reliably as granting.
            let fixture = $setup().await;
            let user = fixture
                .repo()
                .create(create_user("s1", "Alice"))
                .await
                .unwrap();

            fixture.repo().set_admin(user.id, true).await.unwrap();
            assert!(
                fixture
                    .repo()
                    .find_by_id(user.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_admin
            );

            fixture.repo().set_admin(user.id, false).await.unwrap();
            assert!(
                !fixture
                    .repo()
                    .find_by_id(user.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_admin
            );

            fixture
                .repo()
                .set_admin(Uuid::new_v4(), true)
                .await
                .expect("an unknown id is a silent no-op, never an error");
        }

        #[tokio::test]
        async fn update_oidc_profile_refreshes_the_idp_sourced_fields_only() {
            let fixture = $setup().await;
            let user = fixture
                .repo()
                .create(create_user("s1", "Alice"))
                .await
                .unwrap();
            fixture.repo().set_disabled(user.id, true).await.unwrap();

            fixture
                .repo()
                .update_oidc_profile(
                    user.id,
                    "Alice Liddell".to_string(),
                    Some("https://idp.test/avatar.png".to_string()),
                )
                .await
                .unwrap();

            let refreshed = fixture.repo().find_by_id(user.id).await.unwrap().unwrap();
            assert_eq!(refreshed.display_name, "Alice Liddell");
            assert_eq!(
                refreshed.avatar_url.as_deref(),
                Some("https://idp.test/avatar.png")
            );
            assert!(
                refreshed.disabled,
                "a profile refresh must not clear beam's own moderation state"
            );
        }

        #[tokio::test]
        async fn update_oidc_profile_can_clear_a_removed_avatar() {
            let fixture = $setup().await;
            let user = fixture
                .repo()
                .create(create_user("s1", "Alice"))
                .await
                .unwrap();
            fixture
                .repo()
                .update_oidc_profile(
                    user.id,
                    "Alice".to_string(),
                    Some("https://idp.test/avatar.png".to_string()),
                )
                .await
                .unwrap();

            fixture
                .repo()
                .update_oidc_profile(user.id, "Alice".to_string(), None)
                .await
                .unwrap();

            assert_eq!(
                fixture
                    .repo()
                    .find_by_id(user.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .avatar_url,
                None,
                "an avatar removed at the IdP must be removed here too"
            );
        }
    };
}

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
        use $crate::utils::session_store::{SESSION_TOUCH_THROTTLE_SECS, SessionData};

        const IDLE_TTL: u64 = 14 * 24 * 3600;
        const ABSOLUTE_TTL: u64 = 60 * 24 * 3600;

        fn session_for(user_id: &::uuid::Uuid) -> SessionData {
            SessionData {
                user_id: user_id.to_string(),
                device_hash: "device-hash".to_string(),
                ip: "203.0.113.7".to_string(),
                created_at: 0,
                last_active: 0,
            }
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
            assert_eq!(session.ip, "203.0.113.7");
            assert_eq!(session.device_hash, "device-hash");
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
                    .delete_by_id(&id, &attacker.to_string())
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
                    .delete_by_id(&id, &user.to_string())
                    .await
                    .unwrap()
            );
            assert!(fixture.store().get(&token).await.unwrap().is_none());
        }

        /// A malformed id is a different answer from a session that is not
        /// there, and both implementations have to give it.
        ///
        /// The SQL store parses the caller-supplied id before it queries, so it
        /// errors. The double string-compared and answered `Ok(false)`. The
        /// handler maps the first to 400 and the second to 401, so the same
        /// request produced two different statuses depending on which store was
        /// underneath -- and every default-tier test ran against the one that
        /// hid it.
        #[tokio::test]
        async fn a_malformed_session_id_is_an_error_not_a_miss() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;

            let err = fixture
                .store()
                .delete_by_id("not-a-uuid", &user.to_string())
                .await
                .expect_err(
                    "a malformed session id must be distinguishable from one that does not exist",
                );
            // `InvalidId` specifically, not any error: the handler maps it to
            // the 400 and everything else to the 500, so a store answering
            // `Db(..)` here would report a client's typo as Beam's fault.
            assert!(
                matches!(
                    err,
                    $crate::utils::session_store::SessionError::InvalidId(_)
                ),
                "a malformed session id must be reported as InvalidId, got {err:?}"
            );
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

/// Behavioural contract for [`crate::utils::pending_auth_store::PendingAuthStore`].
///
/// `$setup` names an `async fn() -> impl PendingAuthStoreFixture`.
///
/// This store is a security control, not a cache: it is what stops a captured
/// callback URL from being replayed. Single-use consumption and TTL expiry are
/// therefore the two things that must hold in every implementation.
#[macro_export]
macro_rules! pending_auth_store_contract {
    ($setup:path) => {
        use ::std::time::Duration;
        use $crate::utils::contract::fixture::PendingAuthStoreFixture as _;
        use $crate::utils::pending_auth_store::PendingAuth;

        const TTL: u64 = 600;

        fn pending(state: &str) -> PendingAuth {
            PendingAuth {
                state: state.to_string(),
                nonce: format!("nonce-for-{state}"),
                pkce_verifier: format!("verifier-for-{state}"),
                redirect_path: Some("/libraries".to_string()),
            }
        }

        #[tokio::test]
        async fn a_pending_authorization_round_trips_every_field() {
            let fixture = $setup().await;
            let state = ::uuid::Uuid::new_v4().to_string();
            fixture.store().create(&pending(&state), TTL).await.unwrap();

            let consumed = fixture
                .store()
                .consume(&state)
                .await
                .unwrap()
                .expect("the state just minted is consumable");

            assert_eq!(consumed.state, state);
            assert_eq!(consumed.nonce, format!("nonce-for-{state}"));
            assert_eq!(consumed.pkce_verifier, format!("verifier-for-{state}"));
            assert_eq!(consumed.redirect_path.as_deref(), Some("/libraries"));
        }

        #[tokio::test]
        async fn a_state_can_be_consumed_at_most_once() {
            // The replay defence: a captured callback URL must be inert once
            // its state has been exchanged.
            let fixture = $setup().await;
            let state = ::uuid::Uuid::new_v4().to_string();
            fixture.store().create(&pending(&state), TTL).await.unwrap();

            assert!(fixture.store().consume(&state).await.unwrap().is_some());
            assert!(
                fixture.store().consume(&state).await.unwrap().is_none(),
                "a state that has already been exchanged must never be accepted again"
            );
        }

        #[tokio::test]
        async fn an_unknown_state_is_never_accepted() {
            let fixture = $setup().await;
            assert!(
                fixture
                    .store()
                    .consume("a-state-that-was-never-minted")
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn a_state_expires_and_is_consumed_regardless() {
            let fixture = $setup().await;
            let state = ::uuid::Uuid::new_v4().to_string();
            fixture.store().create(&pending(&state), TTL).await.unwrap();

            fixture.clock().advance(Duration::from_secs(TTL - 1));
            // Consuming inside the window works; re-create to test the far side.
            assert!(fixture.store().consume(&state).await.unwrap().is_some());

            let stale = ::uuid::Uuid::new_v4().to_string();
            fixture.store().create(&pending(&stale), TTL).await.unwrap();
            fixture.clock().advance(Duration::from_secs(TTL + 1));

            assert!(
                fixture.store().consume(&stale).await.unwrap().is_none(),
                "an expired state must not complete a login"
            );
            assert!(
                fixture.store().consume(&stale).await.unwrap().is_none(),
                "and it must stay consumed, not linger for a later attempt"
            );
        }

        #[tokio::test]
        async fn consuming_one_state_leaves_the_others_alone() {
            let fixture = $setup().await;
            let a = ::uuid::Uuid::new_v4().to_string();
            let b = ::uuid::Uuid::new_v4().to_string();
            fixture.store().create(&pending(&a), TTL).await.unwrap();
            fixture.store().create(&pending(&b), TTL).await.unwrap();

            assert!(fixture.store().consume(&a).await.unwrap().is_some());
            assert!(
                fixture.store().consume(&b).await.unwrap().is_some(),
                "a concurrent login in another tab must still be able to finish"
            );
        }

        #[tokio::test]
        async fn a_redirect_path_is_optional() {
            let fixture = $setup().await;
            let state = ::uuid::Uuid::new_v4().to_string();
            let mut auth = pending(&state);
            auth.redirect_path = None;
            fixture.store().create(&auth, TTL).await.unwrap();

            let consumed = fixture.store().consume(&state).await.unwrap().unwrap();
            assert_eq!(consumed.redirect_path, None);
        }
    };
}
