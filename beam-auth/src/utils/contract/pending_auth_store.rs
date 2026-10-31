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
            $crate::utils::pending_auth_store::test_utils::pending_auth(state)
        }

        #[tokio::test]
        async fn minting_a_state_twice_is_refused_and_the_first_survives() {
            // A state names one login attempt. Silently replacing it would let
            // a second `begin_auth` swap the nonce and verifier out from under
            // a browser that is already at the IdP.
            let fixture = $setup().await;
            let state = ::uuid::Uuid::new_v4().to_string();
            fixture.store().create(&pending(&state), TTL).await.unwrap();

            let again = fixture
                .store()
                .create(
                    &PendingAuth {
                        nonce: "a-different-nonce".to_string(),
                        pkce_verifier: "a-different-verifier".to_string(),
                        ..pending(&state)
                    },
                    TTL,
                )
                .await;
            assert!(again.is_err(), "{again:?}");

            let consumed = fixture
                .store()
                .consume(&state)
                .await
                .unwrap()
                .expect("the first authorization is still pending");
            assert_eq!(consumed.nonce, pending(&state).nonce);
            assert_eq!(consumed.pkce_verifier, pending(&state).pkce_verifier);
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
