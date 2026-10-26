//! Cast, crew and people (issue #261): a title's credits as its detail lists
//! them, people search, and a person's filmography.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use sea_orm::DbErr;
use uuid::Uuid;

use beam_domain::models::access::AccessPolicy;
use beam_domain::models::catalog::TitleKind as CatalogTitleKind;
use beam_domain::models::person::{
    CreditRole, Person as StoredPerson, TitleCredit, compare_title_credits,
};
use beam_domain::repositories::{MovieRepository, PersonRepository, ShowRepository};

use crate::models::search::TitleKind;
use crate::models::{
    ArtworkKind, ArtworkVariant, CastCredit, CreditedTitle, CrewCredit, FilmographyCastCredit,
    FilmographyCrewCredit, Person, PersonConnection, PersonDetail, Title, artwork_path,
};
use crate::services::offset_cursor::{self, OffsetList};

/// The page size when a people search names none.
pub const DEFAULT_PAGE_SIZE: u32 = 20;
/// The largest page a people search may ask for.
pub const MAX_PAGE_SIZE: u32 = 100;

/// Why a people request was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum PeopleError {
    /// `after` is not a cursor people search issued.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),
    /// `first` is outside `1..=MAX_PAGE_SIZE`.
    #[error("invalid pagination: {0}")]
    InvalidPagination(String),
    /// The search text holds a NUL, which no name can contain.
    #[error("invalid search query: {0}")]
    InvalidSearchQuery(String),
    /// The store failed.
    #[error("internal people error: {0}")]
    Internal(String),
}

fn internal(err: DbErr) -> PeopleError {
    PeopleError::Internal(err.to_string())
}

/// A person's portrait, as the path Beam serves it from.
fn profile_path(person: &StoredPerson) -> Option<String> {
    person
        .profile_url
        .as_ref()
        .map(|_| artwork_path(ArtworkKind::Person, person.id, ArtworkVariant::Profile))
}

/// A title's credits as its detail lists them: the cast in billing order,
/// then the crew by department and job.
pub fn title_credits(mut credits: Vec<TitleCredit>) -> (Vec<CastCredit>, Vec<CrewCredit>) {
    credits.sort_by(compare_title_credits);
    let mut cast = Vec::new();
    let mut crew = Vec::new();
    for TitleCredit { person, role } in credits {
        let profile_url = profile_path(&person);
        let StoredPerson {
            id: person_id,
            external_ref: _,
            name,
            profile_url: _,
        } = person;
        match role {
            CreditRole::Cast {
                character,
                billing_order,
            } => cast.push(CastCredit {
                person_id,
                name,
                profile_url,
                character,
                billing_order,
            }),
            CreditRole::Crew { job, department } => crew.push(CrewCredit {
                person_id,
                name,
                profile_url,
                job,
                department,
            }),
        }
    }
    (cast, crew)
}

/// Reads people and their filmographies.
#[derive(Debug)]
pub struct PeopleService {
    people: Arc<dyn PersonRepository>,
    movies: Arc<dyn MovieRepository>,
    shows: Arc<dyn ShowRepository>,
}

/// The order a filmography lists its titles in: newest first, a title with no
/// year last, then by title ignoring case, then by id so the order is total.
fn filmography_order(a: &CreditedTitle, b: &CreditedTitle) -> Ordering {
    match (a.year, b.year) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
    .then_with(|| {
        a.title
            .original
            .to_lowercase()
            .cmp(&b.title.original.to_lowercase())
    })
    .then_with(|| a.id.cmp(&b.id))
}

impl PeopleService {
    pub fn new(
        people: Arc<dyn PersonRepository>,
        movies: Arc<dyn MovieRepository>,
        shows: Arc<dyn ShowRepository>,
    ) -> Self {
        Self {
            people,
            movies,
            shows,
        }
    }

    /// The stored person `id` names, portrait URL and all: what the artwork
    /// route fetches a portrait by. `Ok(None)` for a person no title
    /// `access` admits credits any more, whom search no longer lists either
    /// -- their row outlives their last credit, but nothing of theirs is
    /// served once it has gone, or while the policy hides it (issue #328).
    pub async fn find_person(
        &self,
        id: Uuid,
        access: &AccessPolicy,
    ) -> Result<Option<StoredPerson>, DbErr> {
        let Some(person) = self.people.find_by_id(id).await? else {
            return Ok(None);
        };
        if self.people.filmography(id, access).await?.is_empty() {
            return Ok(None);
        }
        Ok(Some(person))
    }

    /// One page of the people credited in the library on a title `access`
    /// admits, by name: those whose name contains `query`, ignoring case, or
    /// everyone without one. A blank query is no query.
    pub async fn search(
        &self,
        query: Option<String>,
        access: &AccessPolicy,
        first: Option<u32>,
        after: Option<String>,
    ) -> Result<PersonConnection, PeopleError> {
        let query = query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty());
        // Postgres text cannot hold a NUL, so no name contains one and the
        // store cannot even bind it (22021): refused here rather than
        // answered with a 500.
        if query.is_some_and(|query| query.contains('\0')) {
            return Err(PeopleError::InvalidSearchQuery(
                "query must not contain a NUL character".to_string(),
            ));
        }
        let size = first.unwrap_or(DEFAULT_PAGE_SIZE);
        let size = NonZeroU32::new(size)
            .filter(|size| size.get() <= MAX_PAGE_SIZE)
            .ok_or_else(|| {
                PeopleError::InvalidPagination(format!(
                    "a page holds 1 to {MAX_PAGE_SIZE} people, not {size}"
                ))
            })?;
        let offset = offset_cursor::start_after(OffsetList::People, after.as_deref())
            .map_err(|err| PeopleError::InvalidCursor(err.to_string()))?;

        let people = self
            .people
            .search(query, access, u64::from(size.get()), offset)
            .await
            .map_err(internal)?;
        let total = self.people.count(query, access).await.map_err(internal)?;
        Ok(PersonConnection {
            page_info: offset_cursor::page_info(OffsetList::People, offset, people.len(), total),
            items: people
                .into_iter()
                .map(|person| Person {
                    profile_url: profile_path(&person),
                    id: person.id,
                    name: person.name,
                })
                .collect(),
            total,
        })
    }

    /// One person with every credit they have on a title `access` admits,
    /// `Ok(None)` when no person has that id or no such title credits them
    /// any more (as search no longer lists them): a person credited only on
    /// hidden titles answers as one that does not exist. A credit on a title
    /// that has since gone is left out.
    pub async fn detail(
        &self,
        id: Uuid,
        access: &AccessPolicy,
    ) -> Result<Option<PersonDetail>, PeopleError> {
        let Some(person) = self.people.find_by_id(id).await.map_err(internal)? else {
            return Ok(None);
        };
        let credits = self
            .people
            .filmography(id, access)
            .await
            .map_err(internal)?;
        if credits.is_empty() {
            return Ok(None);
        }
        let titles = self.titles(&credits).await?;

        let mut cast = Vec::new();
        let mut crew = Vec::new();
        for credit in credits {
            let Some(title) = titles.get(&(credit.title_kind, credit.title_id)) else {
                continue;
            };
            let title = title.clone();
            match credit.role {
                CreditRole::Cast {
                    character,
                    billing_order,
                } => cast.push(FilmographyCastCredit {
                    title,
                    character,
                    billing_order,
                }),
                CreditRole::Crew { job, department } => crew.push(FilmographyCrewCredit {
                    title,
                    job,
                    department,
                }),
            }
        }
        // One person can hold two roles on one title: those keep a stable
        // order too.
        cast.sort_by(|a, b| {
            filmography_order(&a.title, &b.title)
                .then(a.billing_order.cmp(&b.billing_order))
                .then_with(|| a.character.cmp(&b.character))
        });
        crew.sort_by(|a, b| filmography_order(&a.title, &b.title).then_with(|| a.job.cmp(&b.job)));

        Ok(Some(PersonDetail {
            profile_url: profile_path(&person),
            id: person.id,
            name: person.name,
            cast,
            crew,
        }))
    }

    /// The titles `credits` are on, by kind and id: two reads however many
    /// credits there are.
    async fn titles(
        &self,
        credits: &[beam_domain::models::person::PersonCredit],
    ) -> Result<HashMap<(CatalogTitleKind, Uuid), CreditedTitle>, PeopleError> {
        let ids_of = |kind: CatalogTitleKind| -> Vec<Uuid> {
            let mut ids: Vec<Uuid> = credits
                .iter()
                .filter(|credit| credit.title_kind == kind)
                .map(|credit| credit.title_id)
                .collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        let movie_ids = ids_of(CatalogTitleKind::Movie);
        let show_ids = ids_of(CatalogTitleKind::Show);
        let mut titles = HashMap::new();
        if !movie_ids.is_empty() {
            for movie in self
                .movies
                .find_by_ids(&movie_ids)
                .await
                .map_err(internal)?
            {
                titles.insert(
                    (CatalogTitleKind::Movie, movie.id),
                    CreditedTitle {
                        id: movie.id,
                        media_type: TitleKind::Movie,
                        poster_url: movie.poster_url.as_ref().map(|_| {
                            artwork_path(ArtworkKind::Movie, movie.id, ArtworkVariant::Poster)
                        }),
                        title: Title {
                            original: movie.title,
                            localized: movie.title_localized,
                            alternatives: None,
                        },
                        year: movie.year,
                    },
                );
            }
        }
        if !show_ids.is_empty() {
            for show in self.shows.find_by_ids(&show_ids).await.map_err(internal)? {
                titles.insert(
                    (CatalogTitleKind::Show, show.id),
                    CreditedTitle {
                        id: show.id,
                        media_type: TitleKind::Show,
                        poster_url: show.poster_url.as_ref().map(|_| {
                            artwork_path(ArtworkKind::Show, show.id, ArtworkVariant::Poster)
                        }),
                        title: Title {
                            original: show.title,
                            localized: show.title_localized,
                            alternatives: None,
                        },
                        year: show.year,
                    },
                );
            }
        }
        Ok(titles)
    }
}

#[cfg(test)]
#[path = "people_tests.rs"]
pub(crate) mod people_tests;
