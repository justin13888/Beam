//! Recording an event in both places an operator reads it: the process log and
//! the admin log (NFR-403).

use serde_json::Value;

use beam_domain::models::{AdminLogCategory, AdminLogLevel, CreateAdminLog};
use beam_domain::repositories::AdminLogRepository;

/// Record an event the operator reads in two places: the process log, as a
/// structured `tracing` event, and the admin log the admin viewer pages through
/// (NFR-403).
///
/// Never fails. The event is already in the process log when the admin log is
/// written, so a store that cannot take the row costs the viewer one entry and
/// costs the action that caused it nothing: an administrator's change has been
/// made, and a sign-in has been decided, whether or not the row lands. The
/// failure is itself logged.
///
/// `details` is written as given; it is the caller's to keep it free of
/// session tokens, cookies, IdP tokens and device codes.
pub async fn record(
    admin_log: &dyn AdminLogRepository,
    level: AdminLogLevel,
    category: AdminLogCategory,
    message: String,
    details: Value,
) {
    match level {
        AdminLogLevel::Info => {
            tracing::info!(category = ?category, details = %details, "{message}");
        }
        AdminLogLevel::Warning => {
            tracing::warn!(category = ?category, details = %details, "{message}");
        }
        AdminLogLevel::Error => {
            tracing::error!(category = ?category, details = %details, "{message}");
        }
    }
    let entry = CreateAdminLog {
        level,
        category,
        message,
        details: Some(details),
    };
    if let Err(err) = admin_log.create(entry).await {
        tracing::warn!(error = %err, "could not write an admin log entry");
    }
}

/// Record what an administrator did, and who: [`record`] at `Info`, with
/// `admin_user_id` added to `details`.
///
/// `details` should be a JSON object; anything else is wrapped as `{"detail":
/// ...}` so the administrator's id is never dropped.
pub async fn record_admin_action(
    admin_log: &dyn AdminLogRepository,
    category: AdminLogCategory,
    message: String,
    admin_user_id: &str,
    details: Value,
) {
    let mut details = match details {
        Value::Object(fields) => fields,
        other => {
            let mut fields = serde_json::Map::new();
            fields.insert("detail".to_owned(), other);
            fields
        }
    };
    details.insert("admin_user_id".to_owned(), Value::from(admin_user_id));
    record(
        admin_log,
        AdminLogLevel::Info,
        category,
        message,
        Value::Object(details),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;

    #[tokio::test]
    async fn an_administrators_action_is_stored_with_who_did_it() {
        let repo = InMemoryAdminLogRepository::default();

        record_admin_action(
            &repo,
            AdminLogCategory::System,
            "Created library \"Films\"".to_owned(),
            "admin-1",
            serde_json::json!({"library_id": "lib-1"}),
        )
        .await;
        // A caller that hands over something other than an object keeps it,
        // and the administrator's id is still recorded beside it.
        record_admin_action(
            &repo,
            AdminLogCategory::Enrichment,
            "Refreshed".to_owned(),
            "admin-2",
            serde_json::json!(7),
        )
        .await;

        let stored = repo.list(10, 0).await.unwrap();
        let created = stored
            .iter()
            .find(|entry| entry.message == "Created library \"Films\"")
            .expect("written through to the store");
        assert_eq!(
            (&created.level, &created.category, created.details.as_ref()),
            (
                &AdminLogLevel::Info,
                &AdminLogCategory::System,
                Some(&serde_json::json!({"library_id": "lib-1", "admin_user_id": "admin-1"}))
            )
        );
        let refreshed = stored
            .iter()
            .find(|entry| entry.message == "Refreshed")
            .expect("written through to the store");
        assert_eq!(
            (&refreshed.category, refreshed.details.as_ref()),
            (
                &AdminLogCategory::Enrichment,
                Some(&serde_json::json!({"detail": 7, "admin_user_id": "admin-2"}))
            )
        );
    }

    #[tokio::test]
    async fn a_recorded_event_keeps_its_level() {
        let repo = InMemoryAdminLogRepository::default();
        for (level, message) in [
            (AdminLogLevel::Info, "info"),
            (AdminLogLevel::Warning, "warning"),
            (AdminLogLevel::Error, "error"),
        ] {
            record(
                &repo,
                level,
                AdminLogCategory::Auth,
                message.to_owned(),
                serde_json::json!({}),
            )
            .await;
        }
        let mut stored: Vec<(String, AdminLogLevel)> = repo
            .list(10, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| (entry.message, entry.level))
            .collect();
        stored.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            stored,
            vec![
                ("error".to_owned(), AdminLogLevel::Error),
                ("info".to_owned(), AdminLogLevel::Info),
                ("warning".to_owned(), AdminLogLevel::Warning),
            ]
        );
    }
}
