use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::person::{CreditRole, Person, PersonCredit, TitleCredit};
use crate::providers::enrichment::CreditEnrichment;

/// People and their credits (issue #261), written by enrichment a title's
/// whole set at a time and read by a title's detail, people search and a
/// person's filmography.
#[async_trait]
pub trait PersonRepository: Send + Sync + std::fmt::Debug {
    /// Upsert every credited person by their provider id -- refreshing the
    /// name and portrait already stored -- and replace the movie's complete
    /// credit set with `credits` (idempotent). A credit listed twice is
    /// stored once.
    async fn set_movie_credits(
        &self,
        movie_id: Uuid,
        credits: &[CreditEnrichment],
    ) -> Result<(), DbErr>;
    /// Same as `set_movie_credits`, for shows.
    async fn set_show_credits(
        &self,
        show_id: Uuid,
        credits: &[CreditEnrichment],
    ) -> Result<(), DbErr>;
    /// The movie's credits, each with its person, in no particular order:
    /// callers order them with [`crate::models::person::compare_title_credits`].
    async fn movie_credits(&self, movie_id: Uuid) -> Result<Vec<TitleCredit>, DbErr>;
    /// Same as `movie_credits`, for shows.
    async fn show_credits(&self, show_id: Uuid) -> Result<Vec<TitleCredit>, DbErr>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Person>, DbErr>;
    /// Every credit the person has, on movies and shows alike, in no
    /// particular order. Empty for an unknown id.
    async fn filmography(&self, person_id: Uuid) -> Result<Vec<PersonCredit>, DbErr>;
    /// One page of the people credited on at least one title, ordered by name
    /// ignoring case and then by id: those whose name contains `query`,
    /// ignoring case, or everyone without one. `query`'s `%` and `_` are
    /// themselves, not wildcards.
    async fn search(
        &self,
        query: Option<&str>,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Person>, DbErr>;
    /// How many people [`Self::search`] would list for `query` across every
    /// page.
    async fn count(&self, query: Option<&str>) -> Result<u64, DbErr>;
}

/// The credits a set is stored as: each distinct `(person, role)` once, in
/// the order first listed. A provider can list one credit twice, and storing
/// it twice would show it twice.
pub fn distinct_credits(credits: &[CreditEnrichment]) -> Vec<&CreditEnrichment> {
    let mut seen: std::collections::HashSet<(&str, &CreditRole)> = std::collections::HashSet::new();
    credits
        .iter()
        .filter(|credit| seen.insert((credit.person_ref.as_str(), &credit.role)))
        .collect()
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::catalog::TitleKind;
    use parking_lot::RwLock;
    use std::collections::HashMap;

    #[derive(Debug, Clone)]
    struct StoredCredit {
        title_kind: TitleKind,
        title_id: Uuid,
        person_id: Uuid,
        role: CreditRole,
    }

    #[derive(Debug, Default)]
    pub struct InMemoryPersonRepository {
        people: RwLock<HashMap<Uuid, Person>>,
        credits: RwLock<Vec<StoredCredit>>,
    }

    impl InMemoryPersonRepository {
        fn upsert_person(&self, credit: &CreditEnrichment) -> Uuid {
            let mut people = self.people.write();
            let existing = people
                .values_mut()
                .find(|person| person.external_ref == credit.person_ref.as_str());
            match existing {
                Some(person) => {
                    person.name = credit.name.clone();
                    person.profile_url = credit.profile_url.clone();
                    person.id
                }
                None => {
                    let id = Uuid::new_v4();
                    people.insert(
                        id,
                        Person {
                            id,
                            external_ref: credit.person_ref.as_str().to_string(),
                            name: credit.name.clone(),
                            profile_url: credit.profile_url.clone(),
                        },
                    );
                    id
                }
            }
        }

        fn set_credits(&self, title_kind: TitleKind, title_id: Uuid, credits: &[CreditEnrichment]) {
            let stored: Vec<StoredCredit> = distinct_credits(credits)
                .into_iter()
                .map(|credit| StoredCredit {
                    title_kind,
                    title_id,
                    person_id: self.upsert_person(credit),
                    role: credit.role.clone(),
                })
                .collect();
            let mut all = self.credits.write();
            all.retain(|c| !(c.title_kind == title_kind && c.title_id == title_id));
            all.extend(stored);
        }

        fn credits_of(&self, title_kind: TitleKind, title_id: Uuid) -> Vec<TitleCredit> {
            let people = self.people.read();
            self.credits
                .read()
                .iter()
                .filter(|c| c.title_kind == title_kind && c.title_id == title_id)
                .filter_map(|c| {
                    people.get(&c.person_id).map(|person| TitleCredit {
                        person: person.clone(),
                        role: c.role.clone(),
                    })
                })
                .collect()
        }

        fn matching(&self, query: Option<&str>) -> Vec<Person> {
            let credited: std::collections::HashSet<Uuid> =
                self.credits.read().iter().map(|c| c.person_id).collect();
            let needle = query.map(str::to_lowercase);
            let mut people: Vec<Person> = self
                .people
                .read()
                .values()
                .filter(|person| credited.contains(&person.id))
                .filter(|person| {
                    needle
                        .as_deref()
                        .is_none_or(|needle| person.name.to_lowercase().contains(needle))
                })
                .cloned()
                .collect();
            people.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then_with(|| a.id.cmp(&b.id))
            });
            people
        }
    }

    #[async_trait]
    impl PersonRepository for InMemoryPersonRepository {
        async fn set_movie_credits(
            &self,
            movie_id: Uuid,
            credits: &[CreditEnrichment],
        ) -> Result<(), DbErr> {
            self.set_credits(TitleKind::Movie, movie_id, credits);
            Ok(())
        }

        async fn set_show_credits(
            &self,
            show_id: Uuid,
            credits: &[CreditEnrichment],
        ) -> Result<(), DbErr> {
            self.set_credits(TitleKind::Show, show_id, credits);
            Ok(())
        }

        async fn movie_credits(&self, movie_id: Uuid) -> Result<Vec<TitleCredit>, DbErr> {
            Ok(self.credits_of(TitleKind::Movie, movie_id))
        }

        async fn show_credits(&self, show_id: Uuid) -> Result<Vec<TitleCredit>, DbErr> {
            Ok(self.credits_of(TitleKind::Show, show_id))
        }

        async fn find_by_id(&self, id: Uuid) -> Result<Option<Person>, DbErr> {
            Ok(self.people.read().get(&id).cloned())
        }

        async fn filmography(&self, person_id: Uuid) -> Result<Vec<PersonCredit>, DbErr> {
            Ok(self
                .credits
                .read()
                .iter()
                .filter(|c| c.person_id == person_id)
                .map(|c| PersonCredit {
                    title_kind: c.title_kind,
                    title_id: c.title_id,
                    role: c.role.clone(),
                })
                .collect())
        }

        async fn search(
            &self,
            query: Option<&str>,
            limit: u64,
            offset: u64,
        ) -> Result<Vec<Person>, DbErr> {
            Ok(self
                .matching(query)
                .into_iter()
                .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .collect())
        }

        async fn count(&self, query: Option<&str>) -> Result<u64, DbErr> {
            Ok(self.matching(query).len() as u64)
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use super::PersonRepository;
    use super::in_memory::InMemoryPersonRepository;
    use crate::repositories::contract::fixture::PersonRepositoryFixture;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use crate::repositories::{MovieRepository, ShowRepository};

    /// The hermetic instantiation of the person contract.
    #[derive(Debug, Default)]
    pub struct InMemoryFixture {
        repo: InMemoryPersonRepository,
        movies: InMemoryMovieRepository,
        shows: InMemoryShowRepository,
    }

    impl PersonRepositoryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn PersonRepository {
            &self.repo
        }

        fn movies(&self) -> &dyn MovieRepository {
            &self.movies
        }

        fn shows(&self) -> &dyn ShowRepository {
            &self.shows
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::person_repository_contract!(setup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::enrichment::ExternalMediaRef;

    fn credit(person: &str, role: CreditRole) -> CreditEnrichment {
        CreditEnrichment {
            person_ref: ExternalMediaRef::new("tmdb", person),
            name: format!("Person {person}"),
            profile_url: None,
            role,
        }
    }

    #[test]
    fn a_credit_listed_twice_is_kept_once_and_distinct_roles_are_kept() {
        let director = CreditRole::Crew {
            job: "Director".to_string(),
            department: Some("Directing".to_string()),
        };
        let writer = CreditRole::Crew {
            job: "Writer".to_string(),
            department: Some("Writing".to_string()),
        };
        let credits = [
            credit("1", director.clone()),
            credit("1", writer.clone()),
            credit("1", director.clone()),
            credit("2", director.clone()),
        ];
        let kept: Vec<(&str, &CreditRole)> = distinct_credits(&credits)
            .into_iter()
            .map(|c| (c.person_ref.native(), &c.role))
            .collect();
        assert_eq!(
            kept,
            [("1", &director), ("1", &writer), ("2", &director)],
            "one person may hold two jobs, but not one job twice"
        );
    }
}
