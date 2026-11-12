use super::*;
use beam_domain::repositories::AdminLogRepository;

use crate::repositories::SqlAdminLogRepository;

#[tokio::test]
async fn list_orders_newest_first_and_paginates() {
    let db = connection(empty_mock());
    let repo = SqlAdminLogRepository::new(db.clone());
    let _ = repo.list(20, 40).await;
    drop(repo);

    let sql = statements(db);
    assert_contains(&sql[0], r#"ORDER BY "admin_logs"."created_at" DESC"#);
    // The limit and the offset each bind their own number: one bound to
    // the other's placeholder would page by the wrong amount.
    assert_contains(&sql[0], "LIMIT $1 OFFSET $2");
    let values = bound_values(&sql[0]);
    assert_eq!(values.len(), 2, "{values:?}");
    assert!(values[0].contains("20"), "the limit binds 20: {values:?}");
    assert!(values[1].contains("40"), "the offset binds 40: {values:?}");
}

#[tokio::test]
async fn list_by_category_adds_the_category_filter_to_the_same_ordering() {
    let db = connection(empty_mock());
    let repo = SqlAdminLogRepository::new(db.clone());
    let _ = repo
        .list_by_category(
            beam_domain::models::admin_log::AdminLogCategory::LibraryScan,
            10,
            0,
        )
        .await;
    drop(repo);

    let sql = statements(db);
    assert_filters(&sql[0], "admin_logs", "category", "=");
    assert_contains(&sql[0], r#"ORDER BY "admin_logs"."created_at" DESC"#);
}
