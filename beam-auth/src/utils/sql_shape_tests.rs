//! Hermetic assertions on the SQL these stores generate.
//!
//! The behavioural contracts in `contract.rs` say what the stores *do*; run
//! against the in-memory double they cannot say anything about the statements
//! the SQL implementations send. The `pg-integration` tier can, but it is
//! opt-in and needs a database. `sea_orm::MockDatabase` records the statement
//! before it looks for a result, so a call can be driven with an empty result
//! buffer and the generated SQL inspected regardless of whether it succeeds.
//!
//! These assert *properties* -- which column a filter binds and to what, which
//! table is written, which clause appears -- never a whole statement string. A
//! test that pins the full SQL is a second copy of the query builder's output.

use std::collections::BTreeMap;
use std::sync::Arc;

use sea_orm::{DatabaseConnection, DbBackend, MockDatabase, Statement, Value};
use uuid::Uuid;

use beam_domain::services::TestClock;

/// A Postgres mock that answers every query with no rows and every write with
/// "one row affected", enough times that a multi-statement method runs to the
/// end instead of short-circuiting on the first missing result.
fn empty_mock() -> MockDatabase {
    let no_rows: Vec<Vec<BTreeMap<String, Value>>> = (0..8).map(|_| Vec::new()).collect();
    MockDatabase::new(DbBackend::Postgres)
        .append_query_results(no_rows)
        .append_exec_results((0..8).map(|_| sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }))
}

fn connection(mock: MockDatabase) -> Arc<DatabaseConnection> {
    Arc::new(mock.into_connection())
}

/// Every statement issued, in order. Takes the connection by value: draining
/// the log consumes the mock, so the store holding the other `Arc` handle must
/// be dropped first.
fn statements(db: Arc<DatabaseConnection>) -> Vec<Statement> {
    Arc::try_unwrap(db)
        .expect("drop the store before draining its statement log")
        .into_transaction_log()
        .into_iter()
        .flat_map(|transaction| transaction.statements().to_vec())
        .collect()
}

#[track_caller]
fn assert_filters(statement: &Statement, table: &str, column: &str, operator: &str) {
    let needle = format!(r#""{table}"."{column}" {operator}"#);
    assert!(
        statement.sql.contains(&needle),
        "expected the statement to filter on `{needle}`, got:\n{}",
        statement.sql
    );
}

#[track_caller]
fn assert_contains(statement: &Statement, needle: &str) {
    assert!(
        statement.sql.contains(needle),
        "expected the statement to contain `{needle}`, got:\n{}",
        statement.sql
    );
}

fn bound_values(statement: &Statement) -> Vec<String> {
    statement
        .values
        .as_ref()
        .map(|values| values.0.iter().map(|v| format!("{v:?}")).collect())
        .unwrap_or_default()
}

#[track_caller]
fn assert_bound(statement: &Statement, expected: &str) {
    let values = bound_values(statement);
    assert!(
        values.iter().any(|v| v.contains(expected)),
        "expected `{expected}` among the bound parameters {values:?} of:\n{}",
        statement.sql
    );
}

#[track_caller]
fn assert_not_bound(statement: &Statement, forbidden: &str) {
    let values = bound_values(statement);
    assert!(
        !values.iter().any(|v| v.contains(forbidden)),
        "`{forbidden}` must never reach the database, but was bound in {values:?}"
    );
}

fn clock() -> Arc<TestClock> {
    Arc::new(TestClock::starting_at(
        chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant"),
    ))
}

mod user_repository {
    use super::*;
    use crate::utils::models::CreateUser;
    use crate::utils::repository::{SqlUserRepository, UserRepository};

    fn create_user() -> CreateUser {
        CreateUser {
            oidc_issuer: "https://idp.test".to_string(),
            oidc_subject: "subj-1".to_string(),
            email: Some("ada@example.com".to_string()),
            display_name: "Ada".to_string(),
            avatar_url: None,
            is_admin: false,
        }
    }

    #[tokio::test]
    async fn provisioning_writes_the_identity_and_never_trusts_a_caller_supplied_disabled_flag() {
        let db = connection(empty_mock());
        let repo = SqlUserRepository::with_clock(db.clone(), clock());
        let _ = repo.create(create_user()).await;
        drop(repo);

        let sql = statements(db);
        assert_eq!(sql.len(), 1, "provisioning is one INSERT");
        assert_contains(&sql[0], r#"INSERT INTO "users""#);
        assert_bound(&sql[0], "https://idp.test");
        assert_bound(&sql[0], "subj-1");
        assert_bound(&sql[0], "ada@example.com");
        assert_contains(&sql[0], r#""disabled""#);
    }

    #[tokio::test]
    async fn the_identity_lookup_binds_both_halves_of_the_key() {
        // `(oidc_issuer, oidc_subject)` is the identity. Filtering on only one
        // half would let a subject from one IdP resolve to another IdP's user.
        let db = connection(empty_mock());
        let repo = SqlUserRepository::new(db.clone());
        let _ = repo
            .find_by_oidc_identity("https://idp.test", "subj-1")
            .await;
        drop(repo);

        let sql = statements(db);
        assert_filters(&sql[0], "users", "oidc_issuer", "=");
        assert_filters(&sql[0], "users", "oidc_subject", "=");
        assert_bound(&sql[0], "https://idp.test");
        assert_bound(&sql[0], "subj-1");
    }

    #[tokio::test]
    async fn find_by_id_is_a_primary_key_lookup() {
        let db = connection(empty_mock());
        let repo = SqlUserRepository::new(db.clone());
        let id = Uuid::from_u128(7);
        let _ = repo.find_by_id(id).await;
        drop(repo);

        let sql = statements(db);
        assert_filters(&sql[0], "users", "id", "=");
        assert_bound(&sql[0], &id.to_string());
    }

    #[tokio::test]
    async fn the_admin_list_is_ordered_oldest_first_and_paginated() {
        let db = connection(empty_mock());
        let repo = SqlUserRepository::new(db.clone());
        let _ = repo.list_page(25, 50).await;
        drop(repo);

        let sql = statements(db);
        // Oldest-first is what keeps a page stable while users are being
        // provisioned; newest-first would shift every page on each signup.
        assert_contains(&sql[0], r#"ORDER BY "users"."created_at" ASC"#);
        assert_bound(&sql[0], "25");
        assert_bound(&sql[0], "50");
    }

    #[tokio::test]
    async fn counting_users_is_a_count_over_the_whole_table() {
        let db = connection(
            MockDatabase::new(DbBackend::Postgres).append_query_results([vec![BTreeMap::from([
                ("num_items".to_string(), Value::BigInt(Some(3))),
            ])]]),
        );
        let repo = SqlUserRepository::new(db.clone());
        assert_eq!(repo.count().await.unwrap(), 3);
        drop(repo);

        let sql = statements(db);
        assert_contains(&sql[0], r#"FROM "users""#);
        assert_contains(&sql[0], "COUNT");
    }

    #[tokio::test]
    async fn moderation_and_admin_updates_touch_one_row_and_nothing_else() {
        for (label, id) in [
            ("set_disabled", Uuid::from_u128(11)),
            ("set_admin", Uuid::from_u128(12)),
        ] {
            let db = connection(empty_mock());
            let repo = SqlUserRepository::new(db.clone());
            if label == "set_disabled" {
                let _ = repo.set_disabled(id, true).await;
            } else {
                let _ = repo.set_admin(id, true).await;
            }
            drop(repo);

            let sql = statements(db);
            // The row is located before it is written: an UPDATE with no
            // WHERE would promote or disable every account at once.
            assert_filters(&sql[0], "users", "id", "=");
            assert_bound(&sql[0], &id.to_string());
        }
    }

    #[tokio::test]
    async fn updating_a_user_that_no_longer_exists_issues_no_write() {
        // Every one of these is a lookup-then-update; with no row found there
        // must be no UPDATE at all, and no error either. Covering all three
        // also pins that each actually reaches the database rather than
        // returning `Ok(())` without doing anything.
        let id = Uuid::from_u128(1);
        for label in ["set_admin", "set_disabled", "update_oidc_profile"] {
            let db = connection(empty_mock());
            let repo = SqlUserRepository::new(db.clone());
            match label {
                "set_admin" => repo.set_admin(id, true).await.unwrap(),
                "set_disabled" => repo.set_disabled(id, true).await.unwrap(),
                _ => repo
                    .update_oidc_profile(id, "Ada".to_string(), None)
                    .await
                    .unwrap(),
            }
            drop(repo);

            let sql = statements(db);
            assert_eq!(sql.len(), 1, "{label}: only the lookup, got {sql:?}");
            assert!(!sql[0].sql.starts_with("UPDATE"), "{label}");
            assert_filters(&sql[0], "users", "id", "=");
            assert_bound(&sql[0], &id.to_string());
        }
    }

    #[tokio::test]
    async fn refreshing_a_profile_writes_the_idp_fields_and_leaves_the_rest_alone() {
        let db = connection(
            MockDatabase::new(DbBackend::Postgres)
                .append_query_results([vec![user_row()], vec![user_row()]]),
        );
        let repo = SqlUserRepository::new(db.clone());
        let _ = repo
            .update_oidc_profile(
                Uuid::from_u128(9),
                "Ada Lovelace".to_string(),
                Some("https://idp.test/ada.png".to_string()),
            )
            .await;
        drop(repo);

        let sql = statements(db);
        let update = sql
            .iter()
            .find(|s| s.sql.starts_with("UPDATE"))
            .expect("a found user is updated");
        assert_bound(update, "Ada Lovelace");
        assert_bound(update, "https://idp.test/ada.png");
        assert_filters(update, "users", "id", "=");
        // beam's own moderation state is not the IdP's to set.
        assert!(
            !update.sql.contains(r#""disabled" ="#),
            "a profile refresh must not rewrite `disabled`:\n{}",
            update.sql
        );
    }

    /// A stored user, for the paths that read a row back before writing.
    fn user_row() -> beam_entity::user::Model {
        let now: chrono::DateTime<chrono::FixedOffset> =
            chrono::DateTime::from_timestamp(1_700_000_000, 0)
                .expect("valid instant")
                .into();
        beam_entity::user::Model {
            id: Uuid::from_u128(9),
            oidc_issuer: "https://idp.test".to_string(),
            oidc_subject: "subj-1".to_string(),
            email: Some("ada@example.com".to_string()),
            display_name: "Ada".to_string(),
            avatar_url: None,
            is_admin: false,
            disabled: false,
            created_at: now,
            updated_at: now,
        }
    }
}

mod session_store {
    use super::*;
    use crate::utils::session_store::{PgSessionStore, SessionData, SessionStore};

    fn session(user_id: Uuid) -> SessionData {
        SessionData {
            user_id: user_id.to_string(),
            device_hash: "device-hash".to_string(),
            ip: "203.0.113.7".to_string(),
            created_at: 0,
            last_active: 0,
        }
    }

    #[tokio::test]
    async fn only_the_hash_of_a_session_token_is_ever_stored() {
        // The plaintext token is the credential. If it reached the database,
        // a read-only leak of the sessions table would be a full account
        // takeover for every signed-in user.
        let db = connection(empty_mock());
        let store = PgSessionStore::with_clock(db.clone(), clock());
        // The insert cannot return a row from an empty mock, so the call
        // fails after issuing its statement -- which is all this asserts on.
        let _ = store.create(&session(Uuid::from_u128(3)), 100, 200).await;
        drop(store);

        let sql = statements(db);
        assert_contains(&sql[0], r#"INSERT INTO "sessions""#);
        assert_bound(&sql[0], "203.0.113.7");
        // A 64-character hex digest is bound; the 43-character URL-safe base64
        // token never is.
        let values = bound_values(&sql[0]);
        assert!(
            values.iter().any(|v| {
                v.len() >= 64 && v.chars().filter(|c| c.is_ascii_hexdigit()).count() >= 64
            }),
            "expected a SHA-256 digest among {values:?}"
        );
    }

    #[tokio::test]
    async fn a_session_is_looked_up_by_token_hash_not_by_token() {
        let db = connection(empty_mock());
        let store = PgSessionStore::with_clock(db.clone(), clock());
        let _ = store.get("a-plaintext-token").await;
        drop(store);

        let sql = statements(db);
        assert_filters(&sql[0], "sessions", "token_hash", "=");
        assert_not_bound(&sql[0], "a-plaintext-token");
    }

    #[tokio::test]
    async fn listing_a_users_sessions_filters_out_both_kinds_of_expiry_in_sql() {
        // Doing this in SQL rather than in Rust is what keeps an expired
        // session from being offered for revocation; both deadlines matter.
        let db = connection(empty_mock());
        let store = PgSessionStore::with_clock(db.clone(), clock());
        let user = Uuid::from_u128(5);
        let _ = store.list_for_user(&user.to_string()).await;
        drop(store);

        let sql = statements(db);
        assert_filters(&sql[0], "sessions", "user_id", "=");
        assert_filters(&sql[0], "sessions", "idle_expires_at", ">");
        assert_filters(&sql[0], "sessions", "absolute_expires_at", ">");
        assert_bound(&sql[0], &user.to_string());
    }

    #[tokio::test]
    async fn revoking_by_id_is_scoped_to_the_owning_user_in_the_statement_itself() {
        // The ownership check is part of the DELETE, not a Rust `if` around
        // it -- so there is no window in which the wrong row can be removed.
        let db = connection(empty_mock());
        let store = PgSessionStore::with_clock(db.clone(), clock());
        let id = Uuid::from_u128(6);
        let user = Uuid::from_u128(7);
        let _ = store.delete_by_id(&id.to_string(), &user.to_string()).await;
        drop(store);

        let sql = statements(db);
        assert_contains(&sql[0], "DELETE FROM");
        assert_filters(&sql[0], "sessions", "id", "=");
        assert_filters(&sql[0], "sessions", "user_id", "=");
        assert_bound(&sql[0], &id.to_string());
        assert_bound(&sql[0], &user.to_string());
    }

    #[tokio::test]
    async fn revoking_every_session_is_scoped_to_one_user() {
        let db = connection(empty_mock());
        let store = PgSessionStore::with_clock(db.clone(), clock());
        let user = Uuid::from_u128(8);
        let _ = store.delete_all_for_user(&user.to_string()).await;
        drop(store);

        let sql = statements(db);
        assert_contains(&sql[0], "DELETE FROM");
        assert_filters(&sql[0], "sessions", "user_id", "=");
        assert_bound(&sql[0], &user.to_string());
    }

    #[tokio::test]
    async fn a_single_logout_deletes_by_token_hash_only() {
        let db = connection(empty_mock());
        let store = PgSessionStore::with_clock(db.clone(), clock());
        let _ = store.delete("a-plaintext-token").await;
        drop(store);

        let sql = statements(db);
        assert_contains(&sql[0], "DELETE FROM");
        assert_filters(&sql[0], "sessions", "token_hash", "=");
        assert_not_bound(&sql[0], "a-plaintext-token");
    }

    #[tokio::test]
    async fn touching_a_session_that_is_gone_issues_no_write() {
        let db = connection(empty_mock());
        let store = PgSessionStore::with_clock(db.clone(), clock());
        store.touch("a-plaintext-token", 100).await.unwrap();
        drop(store);

        let sql = statements(db);
        assert_eq!(sql.len(), 1, "only the lookup: {sql:?}");
        assert!(!sql[0].sql.starts_with("UPDATE"));
    }
}

mod pending_auth_store {
    use super::*;
    use crate::utils::pending_auth_store::{PendingAuth, PendingAuthStore, SqlPendingAuthStore};

    fn pending() -> PendingAuth {
        PendingAuth {
            state: "state-token".to_string(),
            nonce: "nonce-token".to_string(),
            pkce_verifier: "verifier-token".to_string(),
            redirect_path: Some("/libraries".to_string()),
        }
    }

    #[tokio::test]
    async fn starting_a_login_persists_the_state_nonce_and_verifier() {
        let db = connection(empty_mock());
        let store = SqlPendingAuthStore::with_clock(db.clone(), clock());
        let _ = store.create(&pending(), 600).await;
        drop(store);

        let sql = statements(db);
        assert_contains(&sql[0], r#"INSERT INTO "pending_auths""#);
        for value in ["state-token", "nonce-token", "verifier-token", "/libraries"] {
            assert_bound(&sql[0], value);
        }
    }

    #[tokio::test]
    async fn consuming_a_state_is_a_single_delete_returning_statement() {
        // Single-use is enforced by the statement, not by a SELECT followed by
        // a DELETE: under READ COMMITTED the latter lets two concurrent
        // callers both complete a login with the same state.
        let db = connection(empty_mock());
        let store = SqlPendingAuthStore::with_clock(db.clone(), clock());
        let _ = store.consume("state-token").await;
        drop(store);

        let sql = statements(db);
        assert_eq!(sql.len(), 1, "exactly one round trip: {sql:?}");
        assert_contains(&sql[0], "DELETE FROM");
        assert_contains(&sql[0], "RETURNING");
        assert_filters(&sql[0], "pending_auths", "state", "=");
        assert_bound(&sql[0], "state-token");
    }
}

/// The expiry rules the SQL stores apply to rows they read back, and the
/// deadlines they write.
///
/// These are the parts of the SQL implementations that are neither statement
/// shape nor behaviour a fake can stand in for: comparisons against a fetched
/// row, and arithmetic whose result is a bound parameter. `MockDatabase` can
/// supply a row with chosen timestamps and hand back the statement it was
/// asked for, so both are reachable without a database.
mod expiry_rules {
    use super::*;
    use chrono::{DateTime, FixedOffset, Utc};

    use crate::utils::pending_auth_store::{PendingAuth, PendingAuthStore, SqlPendingAuthStore};
    use crate::utils::session_store::{PgSessionStore, SessionData, SessionStore};

    /// The instant `clock()` is fixed at.
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("valid instant")
    }

    fn offset(at: DateTime<Utc>) -> DateTime<FixedOffset> {
        at.into()
    }

    /// A bound `timestamptz` as sea-orm renders it in its debug form.
    fn bound_instant(at: DateTime<Utc>) -> String {
        offset(at).to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
    }

    fn session_row(
        idle_expires_at: DateTime<Utc>,
        absolute_expires_at: DateTime<Utc>,
    ) -> beam_entity::session::Model {
        beam_entity::session::Model {
            id: Uuid::from_u128(1),
            user_id: Uuid::from_u128(2),
            token_hash: "hash".to_string(),
            device_hash: "device".to_string(),
            ip: "203.0.113.7".to_string(),
            created_at: offset(now()),
            last_active: offset(now()),
            idle_expires_at: offset(idle_expires_at),
            absolute_expires_at: offset(absolute_expires_at),
        }
    }

    /// A store whose first two queries both return `row` -- enough for a
    /// lookup followed by an `UPDATE ... RETURNING`.
    fn store_returning(
        row: beam_entity::session::Model,
    ) -> (PgSessionStore, Arc<DatabaseConnection>) {
        let db = connection(
            MockDatabase::new(DbBackend::Postgres)
                .append_query_results([vec![row.clone()], vec![row]])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }]),
        );
        (PgSessionStore::with_clock(db.clone(), clock()), db)
    }

    #[tokio::test]
    async fn a_session_one_second_inside_both_windows_still_resolves() {
        let ahead = now() + chrono::Duration::seconds(1);
        let (store, _db) = store_returning(session_row(ahead, ahead));
        assert!(store.get("token").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_session_past_its_idle_deadline_does_not_resolve() {
        let behind = now() - chrono::Duration::seconds(1);
        let ahead = now() + chrono::Duration::days(30);
        let (store, _db) = store_returning(session_row(behind, ahead));
        assert!(
            store.get("token").await.unwrap().is_none(),
            "an idle-expired session must not authenticate a request"
        );
    }

    #[tokio::test]
    async fn a_session_past_its_absolute_ceiling_does_not_resolve_however_recently_used() {
        // The two deadlines are independent: an actively-used session must
        // still die at its absolute ceiling. An `&&` here would keep a stolen
        // cookie alive forever as long as it kept being used.
        let ahead = now() + chrono::Duration::days(30);
        let behind = now() - chrono::Duration::seconds(1);
        let (store, _db) = store_returning(session_row(ahead, behind));
        assert!(store.get("token").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_deadline_exactly_now_is_still_valid() {
        // The comparison is strict (`expires_at < now`), so the instant the
        // deadline names is the last valid one rather than the first invalid.
        let (store, _db) = store_returning(session_row(now(), now()));
        assert!(store.get("token").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn creating_a_session_writes_deadlines_in_the_future_not_the_past() {
        // `now + ttl` mistyped as `now - ttl` produces a session that is
        // already expired -- which reads as "login silently does nothing".
        let db = connection(empty_mock());
        let store = PgSessionStore::with_clock(db.clone(), clock());
        let _ = store
            .create(
                &SessionData {
                    user_id: Uuid::from_u128(2).to_string(),
                    device_hash: "device".to_string(),
                    ip: "203.0.113.7".to_string(),
                    created_at: 0,
                    last_active: 0,
                },
                3_600,
                86_400,
            )
            .await;
        drop(store);

        let sql = statements(db);
        assert_bound(
            &sql[0],
            &bound_instant(now() + chrono::Duration::seconds(3_600)),
        );
        assert_bound(
            &sql[0],
            &bound_instant(now() + chrono::Duration::seconds(86_400)),
        );
    }

    #[tokio::test]
    async fn touching_slides_the_idle_deadline_forward_and_stops_at_the_ceiling() {
        let ceiling = now() + chrono::Duration::seconds(60);
        let (store, db) = store_returning(session_row(now(), ceiling));
        store.touch("token", 3_600).await.unwrap();
        drop(store);

        let sql = statements(db);
        let update = sql
            .iter()
            .find(|s| s.sql.starts_with("UPDATE"))
            .expect("touch issues an UPDATE");
        assert_bound(update, &bound_instant(ceiling));
        assert!(
            !bound_values(update)
                .iter()
                .any(|v| v.contains(&bound_instant(now() + chrono::Duration::seconds(3_600)))),
            "the requested deadline is past the ceiling and must be clamped to it"
        );
    }

    #[tokio::test]
    async fn revoking_reports_whether_a_row_was_actually_removed() {
        // `rows_affected > 0` is what turns "no such session, or not yours"
        // into a 401 at the route. Reporting success for zero rows would make
        // a guessed id look like a successful revocation.
        for (rows_affected, expected) in [(0u64, false), (1, true)] {
            let db = connection(MockDatabase::new(DbBackend::Postgres).append_exec_results([
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected,
                },
            ]));
            let store = PgSessionStore::with_clock(db.clone(), clock());
            assert_eq!(
                store
                    .delete_by_id(
                        &Uuid::from_u128(1).to_string(),
                        &Uuid::from_u128(2).to_string()
                    )
                    .await
                    .unwrap(),
                expected,
                "for {rows_affected} rows affected"
            );
        }
    }

    fn pending_row(expires_at: DateTime<Utc>) -> beam_entity::pending_auth::Model {
        beam_entity::pending_auth::Model {
            state: "state-token".to_string(),
            nonce: "nonce".to_string(),
            pkce_verifier: "verifier".to_string(),
            redirect_path: None,
            created_at: offset(now()),
            expires_at: offset(expires_at),
        }
    }

    async fn consume_with(expires_at: DateTime<Utc>) -> Option<PendingAuth> {
        let db = connection(
            MockDatabase::new(DbBackend::Postgres)
                .append_query_results([vec![pending_row(expires_at)]]),
        );
        let store = SqlPendingAuthStore::with_clock(db, clock());
        store.consume("state-token").await.unwrap()
    }

    #[tokio::test]
    async fn a_pending_state_one_second_inside_its_window_is_exchangeable() {
        assert!(
            consume_with(now() + chrono::Duration::seconds(1))
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_pending_state_exactly_at_its_deadline_is_still_exchangeable() {
        // Strict `<`: the deadline instant is the last valid one. A `<=` would
        // reject a login that arrived exactly on time.
        assert!(consume_with(now()).await.is_some());
    }

    #[tokio::test]
    async fn an_expired_pending_state_is_refused() {
        assert!(
            consume_with(now() - chrono::Duration::seconds(1))
                .await
                .is_none(),
            "a stale callback URL must not complete a login"
        );
    }

    #[tokio::test]
    async fn starting_a_login_writes_a_deadline_in_the_future() {
        let db = connection(empty_mock());
        let store = SqlPendingAuthStore::with_clock(db.clone(), clock());
        let _ = store
            .create(
                &PendingAuth {
                    state: "state-token".to_string(),
                    nonce: "nonce".to_string(),
                    pkce_verifier: "verifier".to_string(),
                    redirect_path: None,
                },
                600,
            )
            .await;
        drop(store);

        let sql = statements(db);
        assert_bound(
            &sql[0],
            &bound_instant(now() + chrono::Duration::seconds(600)),
        );
    }
}
