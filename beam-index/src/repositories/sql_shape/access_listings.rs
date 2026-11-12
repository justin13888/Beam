use super::*;
use std::collections::BTreeSet;

use beam_domain::models::access::{AccessPolicy, ContentRating, LibraryAccess};
use beam_domain::models::user_list::{UserList, UserListPosition};
use beam_domain::repositories::{
    CollectionRepository, GenreRepository, PersonRepository, UserListRepository,
};

use crate::repositories::{
    SqlCollectionRepository, SqlGenreRepository, SqlPersonRepository, SqlUserListRepository,
};

const KIDS: Uuid = Uuid::from_u128(0x1d);

fn restricted() -> AccessPolicy {
    AccessPolicy {
        libraries: LibraryAccess::Only(BTreeSet::from([KIDS])),
        max_rating: Some(ContentRating::AllAges),
    }
}

/// The placeholder `$n` that binds `value`.
#[track_caller]
fn placeholder_of(statement: &Statement, value: Value) -> String {
    let values = &statement.values.as_ref().expect("bound values").0;
    let at = values
        .iter()
        .position(|bound| *bound == value)
        .unwrap_or_else(|| panic!("{value:?} is bound: {values:?}"));
    format!("${}", at + 1)
}

/// Every placeholder from `$1` to the number of values bound is used,
/// and none past it.
#[track_caller]
fn assert_every_value_is_placed(statement: &Statement) {
    let count = statement.values.as_ref().map_or(0, |values| values.0.len());
    for n in 1..=count {
        assert_contains(statement, &format!("${n}"));
    }
    assert!(
        !statement.sql.contains(&format!("${}", count + 1)),
        "a placeholder past the {count} bound: {}",
        statement.sql
    );
}

#[tokio::test]
async fn a_list_page_places_the_policy_between_its_owner_and_its_cursor() {
    let (user, entry) = (Uuid::from_u128(0x51), Uuid::from_u128(0xe1));
    let added_at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let db = connection(empty_mock());
    let repo = SqlUserListRepository::new(db.clone());
    repo.find_page(
        user,
        UserList::Watchlist,
        &restricted(),
        Some(UserListPosition {
            added_at,
            id: entry,
        }),
        7,
    )
    .await
    .unwrap();
    repo.count(user, UserList::Watchlist, &restricted())
        .await
        .unwrap();
    drop(repo);

    let sql = statements(db);
    let page = &sql[0];
    assert_every_value_is_placed(page);
    let kids = placeholder_of(page, KIDS.into());
    assert_contains(
        page,
        &format!("l.movie_id = e.movie_id AND l.library_id IN ({kids})"),
    );
    assert_contains(
        page,
        &format!("l.show_id = e.show_id AND l.library_id IN ({kids})"),
    );
    assert_contains(
        page,
        &format!(
            "(e.added_at, e.id) < ({}, {})",
            placeholder_of(page, added_at.into()),
            placeholder_of(page, entry.into())
        ),
    );
    assert_contains(
        page,
        &format!("LIMIT {}", placeholder_of(page, 7i64.into())),
    );
    assert_every_value_is_placed(&sql[1]);
    assert_contains(&sql[1], "l.movie_id = e.movie_id");
}

#[tokio::test]
async fn a_people_search_places_its_query_after_the_policy() {
    let db = connection(empty_mock());
    let repo = SqlPersonRepository::new(db.clone());
    repo.search(Some("smith"), &restricted(), 5, 10)
        .await
        .unwrap();
    repo.filmography(Uuid::from_u128(0x9e), &restricted())
        .await
        .unwrap();
    drop(repo);

    let sql = statements(db);
    let search = &sql[0];
    assert_every_value_is_placed(search);
    assert_contains(
        search,
        &format!("ILIKE {}", placeholder_of(search, "%smith%".into())),
    );
    assert_contains(
        search,
        &format!(
            "LIMIT {} OFFSET {}",
            placeholder_of(search, 5i64.into()),
            placeholder_of(search, 10i64.into())
        ),
    );
    assert_contains(
        search,
        "c.person_id = people.id AND ((c.movie_id IS NOT NULL",
    );
    let filmography = &sql[1];
    assert_every_value_is_placed(filmography);
    assert_contains(
        filmography,
        &format!(
            "c.person_id = {}",
            placeholder_of(filmography, Uuid::from_u128(0x9e).into())
        ),
    );
}

#[tokio::test]
async fn a_collection_listing_counts_only_the_movies_the_policy_admits() {
    let db = connection(empty_mock());
    let repo = SqlCollectionRepository::new(db.clone());
    repo.list(&restricted(), 20, 40).await.unwrap();
    drop(repo);

    let sql = statements(db);
    assert_every_value_is_placed(&sql[0]);
    assert_contains(
        &sql[0],
        &format!(
            "FROM collection_movies cm WHERE EXISTS (SELECT 1 FROM library_movies l \
             WHERE l.movie_id = cm.movie_id AND l.library_id IN ({})",
            placeholder_of(&sql[0], KIDS.into())
        ),
    );
    assert_contains(
        &sql[0],
        &format!(
            "LIMIT {} OFFSET {}",
            placeholder_of(&sql[0], 20i64.into()),
            placeholder_of(&sql[0], 40i64.into())
        ),
    );
}

#[tokio::test]
async fn the_carried_genres_check_each_junction_against_the_policy() {
    let db = connection(empty_mock());
    let repo = SqlGenreRepository::new(db.clone());
    repo.find_carried(&restricted()).await.unwrap();
    repo.find_carried(&AccessPolicy::unrestricted())
        .await
        .unwrap();
    drop(repo);

    let sql = statements(db);
    assert_every_value_is_placed(&sql[0]);
    assert_contains(&sql[0], "l.movie_id = j.movie_id");
    assert_contains(&sql[0], "l.show_id = j.show_id");
    assert!(
        !sql[1].sql.contains("library_"),
        "an unrestricted caller's genres check no library: {}",
        sql[1].sql
    );
    assert_contains(&sql[1], "FROM movie_genres j WHERE j.genre_id = g.id)");
}
