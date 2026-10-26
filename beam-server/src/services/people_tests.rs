//! `PeopleService` over the in-memory stores: how a title's credits are
//! listed, how people search pages, and how a filmography is assembled.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use beam_domain::models::enrichment::FieldLocks;
use beam_domain::models::person::{CreditRole, Person, PersonCredit, TitleCredit};
use beam_domain::models::{CreateMovie, CreateShow};
use beam_domain::providers::enrichment::{CreditEnrichment, ExternalMediaRef, MovieEnrichment};
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::person::in_memory::InMemoryPersonRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::{MovieRepository, PersonRepository, ShowRepository};

use super::{PeopleError, PeopleService, title_credits};
use crate::models::search::TitleKind;

struct Fixture {
    movies: Arc<InMemoryMovieRepository>,
    shows: Arc<InMemoryShowRepository>,
    people: Arc<InMemoryPersonRepository>,
    service: PeopleService,
}

impl Fixture {
    fn new() -> Self {
        let movies = Arc::new(InMemoryMovieRepository::default());
        let shows = Arc::new(InMemoryShowRepository::default());
        let people = Arc::new(InMemoryPersonRepository::default());
        let service = PeopleService::new(people.clone(), movies.clone(), shows.clone());
        Self {
            movies,
            shows,
            people,
            service,
        }
    }

    async fn movie(&self, title: &str, year: Option<u32>) -> Uuid {
        self.movies
            .find_or_create_by_identity(CreateMovie::new(title.to_string(), year, None))
            .await
            .unwrap()
            .id
    }

    async fn show(&self, title: &str, year: Option<u32>) -> Uuid {
        self.shows
            .find_or_create_by_identity(CreateShow::new(title.to_string(), year))
            .await
            .unwrap()
            .id
    }
}

fn cast(native: &str, name: &str, character: &str, billing_order: u32) -> CreditEnrichment {
    CreditEnrichment {
        person_ref: ExternalMediaRef::new("tmdb", native),
        name: name.to_string(),
        profile_url: None,
        role: CreditRole::Cast {
            character: Some(character.to_string()),
            billing_order,
        },
    }
}

fn crew(native: &str, name: &str, job: &str, department: &str) -> CreditEnrichment {
    CreditEnrichment {
        person_ref: ExternalMediaRef::new("tmdb", native),
        name: name.to_string(),
        profile_url: None,
        role: CreditRole::Crew {
            job: job.to_string(),
            department: Some(department.to_string()),
        },
    }
}

#[test]
fn a_titles_credits_split_into_cast_and_crew_each_in_order() {
    let person = |name: &str, profile: Option<&str>| Person {
        id: Uuid::new_v4(),
        external_ref: format!("tmdb:{name}"),
        name: name.to_string(),
        profile_url: profile.map(str::to_string),
    };
    let lead = person("Lead", Some("https://image.example/lead.jpg"));
    let lead_id = lead.id;
    let (cast_list, crew_list) = title_credits(vec![
        TitleCredit {
            person: person("Writer", None),
            role: CreditRole::Crew {
                job: "Screenplay".to_string(),
                department: Some("Writing".to_string()),
            },
        },
        TitleCredit {
            person: person("Second", None),
            role: CreditRole::Cast {
                character: None,
                billing_order: 1,
            },
        },
        TitleCredit {
            person: lead,
            role: CreditRole::Cast {
                character: Some("Hero".to_string()),
                billing_order: 0,
            },
        },
        TitleCredit {
            person: person("Director", None),
            role: CreditRole::Crew {
                job: "Director".to_string(),
                department: Some("Directing".to_string()),
            },
        },
    ]);

    let cast_names: Vec<&str> = cast_list.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cast_names, ["Lead", "Second"]);
    assert_eq!(cast_list[0].character.as_deref(), Some("Hero"));
    assert_eq!(cast_list[0].billing_order, 0);
    // The portrait is Beam's path to it, never the provider's URL.
    assert_eq!(
        cast_list[0].profile_url,
        Some(format!("/v1/artwork/person/{lead_id}/profile"))
    );
    assert_eq!(cast_list[1].profile_url, None);

    let crew_jobs: Vec<&str> = crew_list.iter().map(|c| c.job.as_str()).collect();
    assert_eq!(crew_jobs, ["Director", "Screenplay"]);
    assert_eq!(crew_list[0].department.as_deref(), Some("Directing"));
}

#[tokio::test]
async fn search_pages_through_every_match_by_cursor() {
    let fixture = Fixture::new();
    let movie = fixture.movie("The Matrix", Some(1999)).await;
    fixture
        .people
        .set_movie_credits(
            movie,
            &[
                cast("1", "Keanu Reeves", "Neo", 0),
                cast("2", "Carrie-Anne Moss", "Trinity", 1),
                cast("3", "Hugo Weaving", "Smith", 2),
                crew("4", "Lana Wachowski", "Director", "Directing"),
            ],
        )
        .await
        .unwrap();

    let first = fixture.service.search(None, Some(3), None).await.unwrap();
    let names: Vec<&str> = first.items.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Carrie-Anne Moss", "Hugo Weaving", "Keanu Reeves"]);
    assert_eq!(first.total, 4);
    assert!(first.page_info.has_next_page);
    assert!(!first.page_info.has_previous_page);

    let second = fixture
        .service
        .search(None, Some(3), first.page_info.end_cursor.clone())
        .await
        .unwrap();
    let names: Vec<&str> = second.items.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Lana Wachowski"]);
    assert!(!second.page_info.has_next_page);
    assert!(second.page_info.has_previous_page);

    let found = fixture
        .service
        .search(Some("  WACH ".to_string()), None, None)
        .await
        .unwrap();
    assert_eq!(
        found.total, 1,
        "the query is trimmed and matched ignoring case"
    );
    assert_eq!(found.items[0].name, "Lana Wachowski");

    let blank = fixture
        .service
        .search(Some("   ".to_string()), None, None)
        .await
        .unwrap();
    assert_eq!(blank.total, 4, "a blank query is no query");
}

#[tokio::test]
async fn search_refuses_what_it_cannot_answer() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture
            .service
            .search(Some("nul\0byte".to_string()), None, None)
            .await,
        Err(PeopleError::InvalidSearchQuery(_))
    ));
    for size in [0, super::MAX_PAGE_SIZE + 1] {
        assert!(
            matches!(
                fixture.service.search(None, Some(size), None).await,
                Err(PeopleError::InvalidPagination(_))
            ),
            "a page of {size}"
        );
    }
    let foreign = crate::services::offset_cursor::encode(
        crate::services::offset_cursor::OffsetList::AdminUsers,
        0,
    );
    assert!(matches!(
        fixture.service.search(None, None, Some(foreign)).await,
        Err(PeopleError::InvalidCursor(_))
    ));
}

#[tokio::test]
async fn a_filmography_lists_cast_and_crew_newest_first() {
    let fixture = Fixture::new();
    let old = fixture.movie("Point Break", Some(1991)).await;
    let new = fixture.movie("John Wick", Some(2014)).await;
    let undated = fixture.show("Untitled Series", None).await;
    let gone = Uuid::new_v4();
    fixture
        .movies
        .apply_enrichment(
            new,
            &MovieEnrichment {
                title: "John Wick".to_string(),
                year: Some(2014),
                poster_url: Some("https://image.example/wick.jpg".to_string()),
                ..Default::default()
            },
            &FieldLocks::none(),
        )
        .await
        .unwrap();
    fixture
        .people
        .set_movie_credits(old, &[cast("1", "Keanu Reeves", "Johnny Utah", 0)])
        .await
        .unwrap();
    fixture
        .people
        .set_movie_credits(
            new,
            &[
                cast("1", "Keanu Reeves", "John Wick", 0),
                crew("1", "Keanu Reeves", "Executive Producer", "Production"),
            ],
        )
        .await
        .unwrap();
    fixture
        .people
        .set_show_credits(undated, &[cast("1", "Keanu Reeves", "Narrator", 3)])
        .await
        .unwrap();
    // A credit whose title has since gone is not listed.
    fixture
        .people
        .set_movie_credits(gone, &[cast("1", "Keanu Reeves", "Ghost", 0)])
        .await
        .unwrap();

    let person = fixture.people.movie_credits(old).await.unwrap()[0]
        .person
        .id;
    let detail = fixture
        .service
        .detail(person)
        .await
        .unwrap()
        .expect("the person exists");
    assert_eq!(detail.name, "Keanu Reeves");
    let cast_titles: Vec<(&str, Option<&str>)> = detail
        .cast
        .iter()
        .map(|c| (c.title.title.original.as_str(), c.character.as_deref()))
        .collect();
    assert_eq!(
        cast_titles,
        [
            ("John Wick", Some("John Wick")),
            ("Point Break", Some("Johnny Utah")),
            ("Untitled Series", Some("Narrator")),
        ],
        "newest first, a title with no year last"
    );
    assert_eq!(detail.cast[0].title.media_type, TitleKind::Movie);
    assert_eq!(detail.cast[0].title.id, new);
    assert_eq!(
        detail.cast[0].title.poster_url,
        Some(format!("/v1/artwork/movie/{new}/poster"))
    );
    assert_eq!(detail.cast[1].title.poster_url, None);
    assert_eq!(detail.cast[2].title.media_type, TitleKind::Show);
    assert_eq!(detail.cast[2].billing_order, 3);

    assert_eq!(detail.crew.len(), 1);
    assert_eq!(detail.crew[0].job, "Executive Producer");
    assert_eq!(detail.crew[0].department.as_deref(), Some("Production"));
    assert_eq!(detail.crew[0].title.id, new);

    assert!(
        fixture
            .service
            .detail(Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
}

/// A store whose every read fails, for the error paths.
#[derive(Debug)]
struct FailingPeople;

#[async_trait]
impl PersonRepository for FailingPeople {
    async fn set_movie_credits(&self, _: Uuid, _: &[CreditEnrichment]) -> Result<(), DbErr> {
        Err(DbErr::Custom("down".to_string()))
    }
    async fn set_show_credits(&self, _: Uuid, _: &[CreditEnrichment]) -> Result<(), DbErr> {
        Err(DbErr::Custom("down".to_string()))
    }
    async fn movie_credits(&self, _: Uuid) -> Result<Vec<TitleCredit>, DbErr> {
        Err(DbErr::Custom("down".to_string()))
    }
    async fn show_credits(&self, _: Uuid) -> Result<Vec<TitleCredit>, DbErr> {
        Err(DbErr::Custom("down".to_string()))
    }
    async fn find_by_id(&self, _: Uuid) -> Result<Option<Person>, DbErr> {
        Err(DbErr::Custom("down".to_string()))
    }
    async fn filmography(&self, _: Uuid) -> Result<Vec<PersonCredit>, DbErr> {
        Err(DbErr::Custom("down".to_string()))
    }
    async fn search(&self, _: Option<&str>, _: u64, _: u64) -> Result<Vec<Person>, DbErr> {
        Err(DbErr::Custom("down".to_string()))
    }
    async fn count(&self, _: Option<&str>) -> Result<u64, DbErr> {
        Err(DbErr::Custom("down".to_string()))
    }
}

pub(crate) fn failing_people_service() -> PeopleService {
    PeopleService::new(
        Arc::new(FailingPeople),
        Arc::new(InMemoryMovieRepository::default()),
        Arc::new(InMemoryShowRepository::default()),
    )
}

#[tokio::test]
async fn a_failed_read_is_an_error_never_an_empty_answer() {
    let service = failing_people_service();
    assert!(matches!(
        service.search(None, None, None).await,
        Err(PeopleError::Internal(_))
    ));
    assert!(matches!(
        service.detail(Uuid::new_v4()).await,
        Err(PeopleError::Internal(_))
    ));
}
