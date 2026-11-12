use super::*;
use std::collections::BTreeSet;

use beam_domain::models::access::{AccessPolicy, ContentRating, LibraryAccess};
use beam_domain::repositories::{AccessPolicyRepository, MovieRepository, ShowRepository};

use crate::repositories::{SqlAccessPolicyRepository, SqlMovieRepository, SqlShowRepository};

/// A policy is read in one statement -- the row beside each grant -- so
/// a replacement interleaving cannot mix two policies (issue #263).
#[tokio::test]
async fn a_policy_is_read_with_its_grants_in_one_statement() {
    let user = Uuid::from_u128(0x51);
    let db = connection(empty_mock());
    let repo = SqlAccessPolicyRepository::new(db.clone());
    assert_eq!(
        repo.find(user).await.unwrap(),
        AccessPolicy::unrestricted(),
        "no row is no restriction"
    );
    drop(repo);

    let sql = statements(db);
    assert_eq!(sql.len(), 1, "{sql:?}");
    assert_contains(&sql[0], "LEFT JOIN user_library_access");
    assert_contains(&sql[0], "WHERE p.user_id = $1");
    assert_bound(&sql[0], &user.to_string());
}

/// Replacing a restricting policy upserts its row on the user and
/// replaces every grant, all in one transaction; lifting every
/// restriction deletes what was stored and writes nothing.
#[tokio::test]
async fn a_replacement_is_one_transaction_that_replaces_the_grants_whole() {
    let (user, kids, family) = (
        Uuid::from_u128(0x52),
        Uuid::from_u128(0x1d),
        Uuid::from_u128(0xfa),
    );
    let db = connection(empty_mock());
    let repo = SqlAccessPolicyRepository::new(db.clone());
    repo.replace(
        user,
        &AccessPolicy {
            libraries: LibraryAccess::Only(BTreeSet::from([kids, family])),
            max_rating: Some(ContentRating::Teen),
        },
    )
    .await
    .unwrap();
    repo.replace(user, &AccessPolicy::unrestricted())
        .await
        .unwrap();
    drop(repo);

    let log = Arc::try_unwrap(db)
        .expect("the repository is dropped")
        .into_transaction_log();
    assert_eq!(log.len(), 2, "one transaction per replacement: {log:?}");
    let restricting: Vec<&Statement> = log[0].statements().iter().collect();
    let sql: Vec<&str> = restricting.iter().map(|s| s.sql.as_str()).collect();
    assert_eq!(sql.first(), Some(&"BEGIN"), "{sql:?}");
    assert_eq!(sql.last(), Some(&"COMMIT"), "{sql:?}");
    assert!(
        sql[1].starts_with(r#"DELETE FROM "user_library_access""#),
        "the old grants go first: {sql:?}"
    );
    assert_filters(restricting[1], "user_library_access", "user_id", "=");
    assert_bound(restricting[1], &user.to_string());
    assert!(
        sql[2].contains(r#"ON CONFLICT ("user_id") DO UPDATE"#),
        "{sql:?}"
    );
    assert_bound(restricting[2], "teen");
    let grants: Vec<&&Statement> = restricting
        .iter()
        .filter(|s| s.sql.starts_with(r#"INSERT INTO "user_library_access""#))
        .collect();
    assert_eq!(grants.len(), 2, "one grant per library: {sql:?}");
    for library in [kids, family] {
        assert!(
            grants.iter().any(|grant| bound_values(grant)
                .iter()
                .any(|v| v.contains(&library.to_string()))),
            "{library} is granted: {sql:?}"
        );
    }

    let lifting: Vec<&str> = log[1].statements().iter().map(|s| s.sql.as_str()).collect();
    assert!(
        lifting
            .iter()
            .any(|s| s.starts_with(r#"DELETE FROM "user_access_policies""#)),
        "{lifting:?}"
    );
    assert!(
        !lifting.iter().any(|s| s.starts_with("INSERT")),
        "an unrestricted policy stores nothing: {lifting:?}"
    );
}

/// The libraries of many titles are one read of the junction, by the
/// titles asked about; asking about none reads nothing.
#[tokio::test]
async fn library_ids_read_the_junction_once_for_every_title_asked_about() {
    let (first, second) = (Uuid::from_u128(0x61), Uuid::from_u128(0x62));
    let db = connection(empty_mock());
    let movies = SqlMovieRepository::new(db.clone());
    let shows = SqlShowRepository::new(db.clone());
    assert!(movies.library_ids(&[]).await.unwrap().is_empty());
    let _ = movies.library_ids(&[first, second]).await;
    let _ = shows.library_ids(&[first]).await;
    drop((movies, shows));

    let sql = statements(db);
    assert_eq!(sql.len(), 2, "the empty ask issued nothing: {sql:?}");
    assert_filters(&sql[0], "library_movies", "movie_id", "IN");
    assert_bound(&sql[0], &first.to_string());
    assert_bound(&sql[0], &second.to_string());
    assert_filters(&sql[1], "library_shows", "show_id", "IN");
    assert_bound(&sql[1], &first.to_string());
}
