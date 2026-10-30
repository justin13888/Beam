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

        /// A JIT-provisioning request from an identity provider of its own,
        /// so no two calls ever collide on the identity key.
        fn create_user(subject: &str, name: &str) -> CreateUser {
            CreateUser {
                display_name: name.to_string(),
                ..$crate::utils::models::test_utils::create_user(
                    &format!("https://idp.test/{}", Uuid::new_v4()),
                    subject,
                )
            }
        }

        #[tokio::test]
        async fn create_keeps_the_admin_flag_it_is_given() {
            // Login provisions a first-time admin with the flag already set
            // from the IdP claim; a store that dropped it would demote them
            // until their next login.
            let fixture = $setup().await;
            for is_admin in [true, false] {
                let created = fixture
                    .repo()
                    .create(CreateUser {
                        is_admin,
                        ..create_user("s1", "Alice")
                    })
                    .await
                    .unwrap();

                let stored = fixture
                    .repo()
                    .find_by_id(created.id)
                    .await
                    .unwrap()
                    .expect("the user just created resolves");
                assert_eq!(stored.is_admin, is_admin, "read back after create");
                assert_eq!(created.is_admin, is_admin, "returned from create");
                assert!(!stored.disabled, "JIT-provisioned users must start enabled");
            }
        }

        #[tokio::test]
        async fn a_second_user_for_the_same_identity_is_refused() {
            // `(issuer, subject)` is the login key: two rows for it would make
            // which account a login lands in depend on the lookup's row order.
            let fixture = $setup().await;
            let first = create_user("s1", "Alice");
            let issuer = first.oidc_issuer.clone();
            let original = fixture.repo().create(first).await.unwrap();
            let before = fixture.repo().count().await.unwrap();

            let duplicate = fixture
                .repo()
                .create(CreateUser {
                    oidc_issuer: issuer.clone(),
                    ..create_user("s1", "Mallory")
                })
                .await;

            assert!(duplicate.is_err(), "{duplicate:?}");
            assert_eq!(fixture.repo().count().await.unwrap(), before);
            let found = fixture
                .repo()
                .find_by_oidc_identity(&issuer, "s1")
                .await
                .unwrap()
                .expect("the original still resolves");
            assert_eq!(found.id, original.id);
            assert_eq!(found.display_name, "Alice");

            // Only the pair is unique: the same subject at another IdP is
            // another person.
            fixture
                .repo()
                .create(create_user("s1", "Alice elsewhere"))
                .await
                .expect("the same subject under another issuer is a new user");
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
