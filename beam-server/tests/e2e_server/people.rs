//! People (`GET /v1/people`, `GET /v1/people/{id}`, #261) over the SQL
//! people store: search by name, paging, and a person's filmography, each
//! through the caller's access policy.
//!
//! People arrive from a metadata provider's credits, which this tier never
//! reaches, so the people and credits here are written straight into the
//! database the way enrichment writes them.

use beam_test_support::e2e::client::api;
use sea_orm::{ActiveModelTrait, Set};
use uuid::Uuid;

use crate::common::{Admin, VIEWER, Video, assert_refused};

/// A person enrichment would store.
async fn person(admin: &Admin, external_ref: &str, name: &str) -> Uuid {
    let now = chrono::Utc::now().fixed_offset();
    beam_entity::person::ActiveModel {
        id: Set(Uuid::new_v4()),
        external_ref: Set(external_ref.to_owned()),
        name: Set(name.to_owned()),
        profile_url: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(admin.stack.db())
    .await
    .unwrap_or_else(|e| panic!("store {name}: {e}"))
    .id
}

/// What one credit says, beside who and on which movie.
enum Role {
    Cast {
        character: &'static str,
        billing_order: i32,
    },
    Crew {
        job: &'static str,
        department: &'static str,
    },
}

/// Credit `person` on the movie `movie`.
async fn credit(admin: &Admin, person: Uuid, movie: Uuid, role: Role) {
    let (kind, character, billing_order, job, department) = match role {
        Role::Cast {
            character,
            billing_order,
        } => (
            beam_entity::credit::KIND_CAST,
            Some(character.to_owned()),
            Some(billing_order),
            None,
            None,
        ),
        Role::Crew { job, department } => (
            beam_entity::credit::KIND_CREW,
            None,
            None,
            Some(job.to_owned()),
            Some(department.to_owned()),
        ),
    };
    beam_entity::credit::ActiveModel {
        id: Set(Uuid::new_v4()),
        person_id: Set(person),
        movie_id: Set(Some(movie)),
        show_id: Set(None),
        kind: Set(kind.to_owned()),
        character: Set(character),
        billing_order: Set(billing_order),
        job: Set(job),
        department: Set(department),
    }
    .insert(admin.stack.db())
    .await
    .expect("store a credit");
}

/// Every name `client` finds for `query`, one page of `first` at a time,
/// and the total the first page reported.
async fn names(client: &api::Client, query: Option<&str>, first: i64) -> (Vec<String>, i64) {
    let mut names = Vec::new();
    let mut after = None;
    let mut total = None;
    loop {
        let page = client
            .search_people(api::SearchPeopleParams {
                query: query.map(str::to_owned),
                first: Some(first),
                after: after.clone(),
            })
            .await
            .expect("search people")
            .into_inner();
        total.get_or_insert(page.total);
        names.extend(page.items.into_iter().map(|person| person.name));
        if !page.page_info.has_next_page {
            break;
        }
        after = page.page_info.end_cursor;
    }
    (names, total.expect("one page at least"))
}

/// People are searched by any part of their name, ignoring case, ordered by
/// name and paged; a `%` is a character, not a wildcard. A viewer restricted
/// to one library finds only the people credited there, and a person's
/// filmography lists only the titles they may see.
#[tokio::test]
async fn people_are_searched_and_listed_through_the_callers_access_over_postgres() {
    let mut admin = Admin::start().await;
    admin.stack.library.add_video(
        "kids/Paddington (2014)/Paddington (2014).mp4",
        &Video::new(330),
    );
    admin
        .stack
        .library
        .add_video("grown/Heat (1995)/Heat (1995).mp4", &Video::new(550));
    let kids = admin.add_library("Kids", "kids").await;
    let grown = admin.add_library("Grown-ups", "grown").await;
    admin.scan_ok(kids).await;
    admin.scan_ok(grown).await;
    let paddington = admin.title("Paddington").await.id;
    let heat = admin.title("Heat").await.id;

    let hawkins = person(&admin, "tmdb:39658", "Sally Hawkins").await;
    let pacino = person(&admin, "tmdb:1158", "Al Pacino").await;
    let mann = person(&admin, "tmdb:638", "Michael Mann").await;
    let king = person(&admin, "tmdb:1077814", "Paul King").await;
    credit(
        &admin,
        hawkins,
        paddington,
        Role::Cast {
            character: "Mary Brown",
            billing_order: 1,
        },
    )
    .await;
    credit(
        &admin,
        pacino,
        heat,
        Role::Cast {
            character: "Vincent Hanna",
            billing_order: 0,
        },
    )
    .await;
    credit(
        &admin,
        mann,
        heat,
        Role::Crew {
            job: "Director",
            department: "Directing",
        },
    )
    .await;
    // On both libraries' titles: behind the camera on one, in front on the
    // other.
    credit(
        &admin,
        king,
        paddington,
        Role::Crew {
            job: "Director",
            department: "Directing",
        },
    )
    .await;
    credit(
        &admin,
        king,
        heat,
        Role::Cast {
            character: "Bystander",
            billing_order: 40,
        },
    )
    .await;

    // Everyone, by name, across pages of one.
    let (everyone, total) = names(&admin.api, None, 1).await;
    assert_eq!(
        everyone,
        ["Al Pacino", "Michael Mann", "Paul King", "Sally Hawkins"]
    );
    assert_eq!(total, 4, "the total is of every page");
    // Any part of the name, in any case: "AL" is in "Al" and in "Sally".
    assert_eq!(
        names(&admin.api, Some("AL"), 20).await.0,
        ["Al Pacino", "Sally Hawkins"]
    );
    assert_eq!(
        names(&admin.api, Some("%"), 20).await,
        (Vec::new(), 0),
        "a % is matched as itself, and no name holds one"
    );

    let detail = admin
        .api
        .get_person(king)
        .await
        .expect("read Paul King")
        .into_inner();
    assert_eq!(detail.name, "Paul King");
    let cast: Vec<(Uuid, Option<&str>, i64)> = detail
        .cast
        .iter()
        .map(|c| (c.title.id, c.character.as_deref(), c.billing_order))
        .collect();
    assert_eq!(cast, [(heat, Some("Bystander"), 40)]);
    let crew: Vec<(Uuid, &str, Option<&str>)> = detail
        .crew
        .iter()
        .map(|c| (c.title.id, c.job.as_str(), c.department.as_deref()))
        .collect();
    assert_eq!(crew, [(paddington, "Director", Some("Directing"))]);
    assert_eq!(detail.crew[0].title.title.original, "Paddington");
    assert_eq!(detail.crew[0].title.year, Some(2014));

    // A viewer who may see only the children's library.
    let viewer = admin.stack.client(&admin.stack.sign_in(VIEWER).await);
    let viewer_id = viewer
        .get_current_user()
        .await
        .expect("the viewer reads who they are")
        .into_inner()
        .id;
    admin
        .api
        .set_admin_user_access(
            viewer_id,
            &api::types::UserAccess {
                all_libraries: false,
                library_ids: vec![kids],
                max_content_rating: None,
            },
        )
        .await
        .expect("restrict the viewer");

    assert_eq!(
        names(&viewer, None, 20).await,
        (vec!["Paul King".to_owned(), "Sally Hawkins".to_owned()], 2),
        "only the people credited on a title the viewer may see"
    );
    assert_eq!(
        names(&viewer, Some("al"), 20).await.0,
        ["Sally Hawkins"],
        "a search is filtered by access too"
    );
    assert_refused(
        "a person credited only where the viewer may not look",
        404,
        viewer.get_person(pacino).await,
    );
    let theirs = viewer
        .get_person(king)
        .await
        .expect("a person credited on the viewer's library")
        .into_inner();
    assert!(
        theirs.cast.is_empty(),
        "the hidden title's credit is left out: {:?}",
        theirs.cast
    );
    assert_eq!(
        theirs.crew.iter().map(|c| c.title.id).collect::<Vec<_>>(),
        [paddington]
    );

    assert_refused(
        "a person nobody stored",
        404,
        admin.api.get_person(Uuid::new_v4()).await,
    );

    admin.finish().await;
}
