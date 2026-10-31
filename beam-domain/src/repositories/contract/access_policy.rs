/// Behavioural contract for [`crate::repositories::AccessPolicyRepository`]:
/// a user nobody restricted reads as unrestricted, a policy reads back as it
/// was replaced -- its grants whole, never merged with the previous ones --
/// users are kept apart, and a policy naming a user or library that does not
/// exist is refused without touching the stored one (issue #263).
///
/// `$setup` names an `async fn() -> impl AccessPolicyFixture`.
#[macro_export]
macro_rules! access_policy_repository_contract {
    ($setup:path) => {
        use ::std::collections::BTreeSet;
        use ::uuid::Uuid;
        use $crate::models::access::{AccessPolicy, ContentRating, LibraryAccess};
        use $crate::repositories::contract::fixture::AccessPolicyFixture as _;

        fn only(ids: &[Uuid]) -> LibraryAccess {
            LibraryAccess::Only(ids.iter().copied().collect::<BTreeSet<_>>())
        }

        #[tokio::test]
        async fn a_user_nobody_restricted_is_unrestricted() {
            let fixture = $setup().await;
            let user = fixture.new_user().await;

            assert_eq!(
                fixture.repo().find(user).await.unwrap(),
                AccessPolicy::unrestricted()
            );
            assert_eq!(
                fixture.repo().find(Uuid::new_v4()).await.unwrap(),
                AccessPolicy::unrestricted(),
                "an id no user has"
            );
        }

        #[tokio::test]
        async fn a_policy_reads_back_as_it_was_replaced() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let (kids, family) = (fixture.new_library().await, fixture.new_library().await);

            for policy in [
                AccessPolicy {
                    libraries: only(&[kids, family]),
                    max_rating: Some(ContentRating::ParentalGuidance),
                },
                AccessPolicy {
                    libraries: only(&[]),
                    max_rating: None,
                },
                AccessPolicy {
                    libraries: LibraryAccess::All,
                    max_rating: Some(ContentRating::Adult),
                },
                AccessPolicy::unrestricted(),
            ] {
                repo.replace(user, &policy).await.unwrap();
                assert_eq!(repo.find(user).await.unwrap(), policy);
            }
        }

        #[tokio::test]
        async fn replacing_a_policy_replaces_its_grants_whole() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let (first, second) = (fixture.new_library().await, fixture.new_library().await);
            repo.replace(
                user,
                &AccessPolicy {
                    libraries: only(&[first]),
                    max_rating: None,
                },
            )
            .await
            .unwrap();

            let replacement = AccessPolicy {
                libraries: only(&[second]),
                max_rating: None,
            };
            repo.replace(user, &replacement).await.unwrap();

            assert_eq!(repo.find(user).await.unwrap(), replacement);
        }

        #[tokio::test]
        async fn one_users_policy_leaves_anothers_alone() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (restricted, other) = (fixture.new_user().await, fixture.new_user().await);
            let library = fixture.new_library().await;
            let other_policy = AccessPolicy {
                libraries: LibraryAccess::All,
                max_rating: Some(ContentRating::Teen),
            };
            repo.replace(other, &other_policy).await.unwrap();

            repo.replace(
                restricted,
                &AccessPolicy {
                    libraries: only(&[library]),
                    max_rating: Some(ContentRating::AllAges),
                },
            )
            .await
            .unwrap();

            assert_eq!(repo.find(other).await.unwrap(), other_policy);
        }

        #[tokio::test]
        async fn a_policy_naming_a_missing_user_or_library_is_refused_and_changes_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let user = fixture.new_user().await;
            let library = fixture.new_library().await;
            let stored = AccessPolicy {
                libraries: only(&[library]),
                max_rating: Some(ContentRating::Teen),
            };
            repo.replace(user, &stored).await.unwrap();

            let naming_a_missing_library = AccessPolicy {
                libraries: only(&[library, Uuid::new_v4()]),
                max_rating: None,
            };
            assert!(repo.replace(user, &naming_a_missing_library).await.is_err());
            assert_eq!(
                repo.find(user).await.unwrap(),
                stored,
                "the failed replacement left the stored policy whole"
            );

            let missing_user = Uuid::new_v4();
            assert!(
                repo.replace(
                    missing_user,
                    &AccessPolicy {
                        libraries: only(&[library]),
                        max_rating: None,
                    },
                )
                .await
                .is_err()
            );
            // Lifting every restriction stores nothing, so it needs no user.
            repo.replace(missing_user, &AccessPolicy::unrestricted())
                .await
                .unwrap();
            assert_eq!(
                repo.find(missing_user).await.unwrap(),
                AccessPolicy::unrestricted()
            );
        }
    };
}
