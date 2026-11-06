/// Behavioural contract for [`crate::utils::device_auth_store::DeviceAuthStore`].
///
/// `$setup` names an `async fn() -> impl DeviceAuthStoreFixture`.
///
/// The store is the server-side pacing RFC 8628 asks of a device client, and
/// the single-use guard on an approval: both must hold in every
/// implementation, or a client could hammer the IdP through Beam, or mint two
/// sessions from one approval.
#[macro_export]
macro_rules! device_auth_store_contract {
    ($setup:path) => {
        use ::std::time::Duration;
        use $crate::utils::contract::fixture::DeviceAuthStoreFixture as _;
        use $crate::utils::device_auth_store::{Claim, NewDeviceAuth, SLOW_DOWN_STEP_SECS};

        const INTERVAL: u32 = 5;
        const LIFETIME: u64 = 600;

        fn new_flow() -> NewDeviceAuth {
            let handle_hash = ::uuid::Uuid::new_v4().simple().to_string();
            NewDeviceAuth {
                device_code: format!("device-code-for-{handle_hash}"),
                user_code: "BCDF-GHJK".to_string(),
                verification_uri: "https://idp.test/device".to_string(),
                verification_uri_complete: Some(
                    "https://idp.test/device?user_code=BCDF-GHJK".to_string(),
                ),
                interval_secs: INTERVAL,
                expires_in_secs: LIFETIME,
                handle_hash,
            }
        }

        fn claimed(claim: Claim) -> $crate::utils::device_auth_store::DeviceAuth {
            match claim {
                Claim::Claimed(auth) => auth,
                other => panic!("expected the poll to be claimed, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn a_new_flow_may_poll_at_once_and_round_trips_every_field() {
            let fixture = $setup().await;
            let flow = new_flow();
            fixture.store().create(&flow).await.unwrap();
            let started = ::beam_domain::services::Clock::now(fixture.clock());

            let auth = claimed(fixture.store().claim_poll(&flow.handle_hash).await.unwrap());

            assert_eq!(auth.handle_hash, flow.handle_hash);
            assert_eq!(auth.device_code, flow.device_code);
            assert_eq!(auth.user_code, flow.user_code);
            assert_eq!(auth.verification_uri, flow.verification_uri);
            assert_eq!(
                auth.verification_uri_complete,
                flow.verification_uri_complete
            );
            assert_eq!(auth.interval_secs, INTERVAL);
            assert_eq!(auth.created_at, started);
            assert_eq!(
                auth.expires_at,
                started + ::chrono::Duration::seconds(LIFETIME as i64)
            );
            assert_eq!(
                auth.next_poll_at,
                started + ::chrono::Duration::seconds(i64::from(INTERVAL)),
                "claiming a poll moves the next permitted one an interval out"
            );
        }

        #[tokio::test]
        async fn an_unknown_handle_is_not_found() {
            let fixture = $setup().await;
            assert_eq!(
                fixture.store().claim_poll("never-issued").await.unwrap(),
                Claim::NotFound
            );
            assert_eq!(
                fixture.store().bump_interval("never-issued").await.unwrap(),
                None
            );
            assert!(
                fixture
                    .store()
                    .consume("never-issued")
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn a_poll_inside_the_interval_is_too_early_and_does_not_move_the_window() {
            let fixture = $setup().await;
            let flow = new_flow();
            fixture.store().create(&flow).await.unwrap();
            claimed(fixture.store().claim_poll(&flow.handle_hash).await.unwrap());

            fixture
                .clock()
                .advance(Duration::from_secs(u64::from(INTERVAL) - 1));
            assert_eq!(
                fixture.store().claim_poll(&flow.handle_hash).await.unwrap(),
                Claim::TooEarly {
                    interval_secs: INTERVAL
                }
            );

            // The refused poll must not have pushed the window: one second
            // later the interval has fully elapsed since the claim.
            fixture.clock().advance(Duration::from_secs(1));
            claimed(fixture.store().claim_poll(&flow.handle_hash).await.unwrap());
        }

        #[tokio::test]
        async fn slowing_down_grows_the_interval_and_pushes_the_next_poll() {
            let fixture = $setup().await;
            let flow = new_flow();
            fixture.store().create(&flow).await.unwrap();

            let grown = fixture
                .store()
                .bump_interval(&flow.handle_hash)
                .await
                .unwrap();
            assert_eq!(grown, Some(INTERVAL + SLOW_DOWN_STEP_SECS));

            let grown_secs = u64::from(INTERVAL + SLOW_DOWN_STEP_SECS);
            fixture.clock().advance(Duration::from_secs(grown_secs - 1));
            assert_eq!(
                fixture.store().claim_poll(&flow.handle_hash).await.unwrap(),
                Claim::TooEarly {
                    interval_secs: INTERVAL + SLOW_DOWN_STEP_SECS
                }
            );

            fixture.clock().advance(Duration::from_secs(1));
            let auth = claimed(fixture.store().claim_poll(&flow.handle_hash).await.unwrap());
            assert_eq!(
                auth.interval_secs,
                INTERVAL + SLOW_DOWN_STEP_SECS,
                "the grown interval sticks for every later poll"
            );
        }

        #[tokio::test]
        async fn a_flow_past_its_lifetime_is_expired_until_consumed() {
            let fixture = $setup().await;
            let flow = new_flow();
            fixture.store().create(&flow).await.unwrap();

            fixture.clock().advance(Duration::from_secs(LIFETIME));
            assert_eq!(
                fixture.store().claim_poll(&flow.handle_hash).await.unwrap(),
                Claim::Expired
            );
            assert!(
                fixture
                    .store()
                    .consume(&flow.handle_hash)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                fixture.store().claim_poll(&flow.handle_hash).await.unwrap(),
                Claim::NotFound
            );
        }

        #[tokio::test]
        async fn a_flow_ends_at_most_once() {
            let fixture = $setup().await;
            let flow = new_flow();
            fixture.store().create(&flow).await.unwrap();

            let ended = fixture
                .store()
                .consume(&flow.handle_hash)
                .await
                .unwrap()
                .expect("the flow just started can be ended");
            assert_eq!(ended.device_code, flow.device_code);
            assert!(
                fixture
                    .store()
                    .consume(&flow.handle_hash)
                    .await
                    .unwrap()
                    .is_none(),
                "one approval must never mint two sessions"
            );
            assert_eq!(
                fixture.store().claim_poll(&flow.handle_hash).await.unwrap(),
                Claim::NotFound
            );
        }

        #[tokio::test]
        async fn starting_a_flow_sweeps_the_expired_ones_and_leaves_live_ones() {
            let fixture = $setup().await;
            let abandoned = new_flow();
            fixture.store().create(&abandoned).await.unwrap();

            fixture.clock().advance(Duration::from_secs(LIFETIME + 1));
            let live = new_flow();
            fixture.store().create(&live).await.unwrap();
            let newer = new_flow();
            fixture.store().create(&newer).await.unwrap();

            assert!(
                fixture
                    .store()
                    .consume(&abandoned.handle_hash)
                    .await
                    .unwrap()
                    .is_none(),
                "an abandoned flow must not outlive the next flow's start"
            );
            assert!(
                fixture
                    .store()
                    .consume(&live.handle_hash)
                    .await
                    .unwrap()
                    .is_some(),
                "a flow still inside its lifetime must survive the sweep"
            );
        }

        #[tokio::test]
        async fn flows_are_paced_independently() {
            let fixture = $setup().await;
            let a = new_flow();
            let b = new_flow();
            fixture.store().create(&a).await.unwrap();
            fixture.store().create(&b).await.unwrap();

            claimed(fixture.store().claim_poll(&a.handle_hash).await.unwrap());
            fixture.store().bump_interval(&a.handle_hash).await.unwrap();

            let other = claimed(fixture.store().claim_poll(&b.handle_hash).await.unwrap());
            assert_eq!(
                other.interval_secs, INTERVAL,
                "another device's slow_down must not slow this one"
            );
        }
    };
}
