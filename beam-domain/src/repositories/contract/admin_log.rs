/// Behavioural contract for [`crate::repositories::AdminLogRepository`]
/// (issue #349): an entry reads back as written -- every level and category
/// included, which Postgres stores as enums -- stamped from the injected
/// clock; the log lists newest first and pages by offset, a category lists
/// only its own entries, and the count is of every entry.
///
/// `$setup` names an `async fn() -> impl AdminLogFixture` whose log starts
/// empty.
#[macro_export]
macro_rules! admin_log_repository_contract {
    ($setup:path) => {
        use $crate::models::admin_log::{
            AdminLog, AdminLogCategory, AdminLogLevel, CreateAdminLog,
        };
        use $crate::repositories::contract::fixture::AdminLogFixture;
        use $crate::services::Clock as _;

        /// Every level, listed by a match so a new one cannot be left out.
        fn every_level() -> Vec<AdminLogLevel> {
            let listed = |level: &AdminLogLevel| match level {
                AdminLogLevel::Info | AdminLogLevel::Warning | AdminLogLevel::Error => true,
            };
            let levels = vec![
                AdminLogLevel::Info,
                AdminLogLevel::Warning,
                AdminLogLevel::Error,
            ];
            assert!(levels.iter().all(listed));
            levels
        }

        /// Every category, listed by a match so a new one cannot be left out.
        fn every_category() -> Vec<AdminLogCategory> {
            let listed = |category: &AdminLogCategory| match category {
                AdminLogCategory::LibraryScan
                | AdminLogCategory::System
                | AdminLogCategory::Auth
                | AdminLogCategory::Enrichment => true,
            };
            let categories = vec![
                AdminLogCategory::LibraryScan,
                AdminLogCategory::System,
                AdminLogCategory::Auth,
                AdminLogCategory::Enrichment,
            ];
            assert!(categories.iter().all(listed));
            categories
        }

        fn entry(
            level: AdminLogLevel,
            category: AdminLogCategory,
            message: &str,
        ) -> CreateAdminLog {
            CreateAdminLog {
                level,
                category,
                message: message.to_string(),
                details: None,
            }
        }

        fn messages(logs: &[AdminLog]) -> Vec<&str> {
            logs.iter().map(|log| log.message.as_str()).collect()
        }

        /// Write `count` entries of `category`, a second apart, named
        /// `{prefix}{n}` from the oldest up.
        async fn write(
            fixture: &impl AdminLogFixture,
            category: AdminLogCategory,
            prefix: &str,
            count: usize,
        ) {
            for n in 0..count {
                fixture.clock().advance(::std::time::Duration::from_secs(1));
                fixture
                    .repo()
                    .create(entry(
                        AdminLogLevel::Info,
                        category.clone(),
                        &format!("{prefix}{n}"),
                    ))
                    .await
                    .unwrap();
            }
        }

        #[tokio::test]
        async fn an_empty_log_lists_nothing_and_counts_none() {
            let fixture = $setup().await;
            let repo = fixture.repo();

            assert!(repo.list(10, 0).await.unwrap().is_empty());
            assert!(
                repo.list_by_category(AdminLogCategory::System, 10, 0)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(repo.count().await.unwrap(), 0);
        }

        #[tokio::test]
        async fn an_entry_reads_back_as_written_stamped_from_the_clock() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let details = ::serde_json::json!({ "library": "Films", "files": 12, "ok": true });

            let created = repo
                .create(CreateAdminLog {
                    level: AdminLogLevel::Warning,
                    category: AdminLogCategory::LibraryScan,
                    message: "Scan finished with warnings".to_string(),
                    details: Some(details.clone()),
                })
                .await
                .unwrap();

            let listed = repo.list(10, 0).await.unwrap();
            assert_eq!(listed.len(), 1);
            for log in [&created, &listed[0]] {
                let AdminLog {
                    id,
                    level,
                    category,
                    message,
                    details: read_details,
                    created_at,
                } = log;
                assert_eq!(*id, created.id);
                assert_eq!(*level, AdminLogLevel::Warning);
                assert_eq!(*category, AdminLogCategory::LibraryScan);
                assert_eq!(message, "Scan finished with warnings");
                assert_eq!(read_details.as_ref(), Some(&details));
                assert_eq!(*created_at, fixture.clock().now());
            }
            assert_eq!(repo.count().await.unwrap(), 1);
        }

        #[tokio::test]
        async fn every_level_and_category_round_trips() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let mut written = Vec::new();
            for level in every_level() {
                for category in every_category() {
                    fixture.clock().advance(::std::time::Duration::from_secs(1));
                    let message = format!("{level:?} {category:?}");
                    repo.create(entry(level.clone(), category.clone(), &message))
                        .await
                        .unwrap();
                    written.push((message, level.clone(), category));
                }
            }

            let mut read: Vec<_> = repo
                .list(100, 0)
                .await
                .unwrap()
                .into_iter()
                .map(|log| (log.message, log.level, log.category))
                .collect();
            read.reverse();
            assert_eq!(read, written, "oldest first once reversed");
            assert_eq!(repo.count().await.unwrap(), written.len() as u64);
            for category in every_category() {
                let only: Vec<_> = repo
                    .list_by_category(category.clone(), 100, 0)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|log| log.category)
                    .collect();
                assert_eq!(only, vec![category; every_level().len()]);
            }
        }

        #[tokio::test]
        async fn the_log_lists_newest_first_and_pages_by_offset() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            write(&fixture, AdminLogCategory::System, "entry ", 5).await;

            assert_eq!(
                messages(&repo.list(2, 0).await.unwrap()),
                ["entry 4", "entry 3"]
            );
            assert_eq!(
                messages(&repo.list(2, 2).await.unwrap()),
                ["entry 2", "entry 1"]
            );
            assert_eq!(messages(&repo.list(2, 4).await.unwrap()), ["entry 0"]);
            assert!(repo.list(2, 5).await.unwrap().is_empty());
            assert!(repo.list(0, 0).await.unwrap().is_empty());
            assert_eq!(repo.count().await.unwrap(), 5);
        }

        #[tokio::test]
        async fn a_category_lists_only_its_own_entries_newest_first_and_pages() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            write(&fixture, AdminLogCategory::LibraryScan, "scan ", 2).await;
            write(&fixture, AdminLogCategory::Auth, "auth ", 1).await;
            write(&fixture, AdminLogCategory::LibraryScan, "rescan ", 2).await;

            assert_eq!(
                messages(
                    &repo
                        .list_by_category(AdminLogCategory::LibraryScan, 10, 0)
                        .await
                        .unwrap()
                ),
                ["rescan 1", "rescan 0", "scan 1", "scan 0"]
            );
            assert_eq!(
                messages(
                    &repo
                        .list_by_category(AdminLogCategory::LibraryScan, 2, 1)
                        .await
                        .unwrap()
                ),
                ["rescan 0", "scan 1"]
            );
            assert_eq!(
                messages(
                    &repo
                        .list_by_category(AdminLogCategory::Auth, 10, 0)
                        .await
                        .unwrap()
                ),
                ["auth 0"]
            );
            assert!(
                repo.list_by_category(AdminLogCategory::Enrichment, 10, 0)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(repo.count().await.unwrap(), 5, "the count is of every category");
        }
    };
}
