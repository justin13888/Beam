//! The shared `AccessPolicyRepository` contract, run against real SQL: the
//! upsert of the policy row, the foreign keys a grant is held to, and the one
//! transaction a replacement is (issue #263).

use std::sync::Arc;

use uuid::Uuid as FixtureUuid;

// `AccessPolicy` and `LibraryAccess` are in scope from the contract's
// expansion below.
use beam_domain::repositories::AccessPolicyRepository;
use beam_domain::repositories::contract::fixture::AccessPolicyFixture;
use beam_index::repositories::SqlAccessPolicyRepository;
use beam_test_support::{postgres, seed};

struct PgFixture {
    repo: SqlAccessPolicyRepository,
    db: Arc<sea_orm::DatabaseConnection>,
}

#[async_trait::async_trait]
impl AccessPolicyFixture for PgFixture {
    fn repo(&self) -> &dyn AccessPolicyRepository {
        &self.repo
    }

    async fn new_user(&self) -> FixtureUuid {
        seed::user(&self.db).await.expect("seed a user row")
    }

    async fn new_library(&self) -> FixtureUuid {
        seed::library(&self.db).await.expect("seed a library row")
    }
}

async fn setup() -> PgFixture {
    let db = postgres::connection().await;
    PgFixture {
        repo: SqlAccessPolicyRepository::new(db.clone()),
        db,
    }
}

beam_domain::access_policy_repository_contract!(setup);

/// A user's policy and grants go with the user, and a grant with its
/// library: the foreign keys cascade, so neither outlives what it names.
#[tokio::test]
async fn a_policy_goes_with_its_user_and_a_grant_with_its_library() {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let fixture = setup().await;
    let (user, kept, deleted) = (
        fixture.new_user().await,
        fixture.new_library().await,
        fixture.new_library().await,
    );
    fixture
        .repo
        .replace(
            user,
            &AccessPolicy {
                libraries: LibraryAccess::Only([kept, deleted].into_iter().collect()),
                max_rating: None,
            },
        )
        .await
        .unwrap();

    fixture
        .db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM libraries WHERE id = $1",
            [deleted.into()],
        ))
        .await
        .unwrap();
    assert_eq!(
        fixture.repo.find(user).await.unwrap(),
        AccessPolicy {
            libraries: LibraryAccess::Only([kept].into_iter().collect()),
            max_rating: None,
        }
    );

    fixture
        .db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM users WHERE id = $1",
            [user.into()],
        ))
        .await
        .unwrap();
    let left = fixture
        .db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT (SELECT count(*) FROM user_access_policies WHERE user_id = $1) \
                  + (SELECT count(*) FROM user_library_access WHERE user_id = $1) AS n",
            [user.into()],
        ))
        .await
        .unwrap()
        .expect("a count")
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(left, 0);
}
