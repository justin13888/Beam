use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::access::AccessPolicy;
use crate::models::collection::{Collection, CollectionSummary};
use crate::providers::enrichment::CollectionEnrichment;

/// Provider collections and the movies in them (issue #262), written by
/// enrichment a movie at a time and read by a movie's detail, the collection
/// listing and a collection's detail.
#[async_trait]
pub trait CollectionRepository: Send + Sync + std::fmt::Debug {
    /// Put the movie in `collection` -- upserting the collection by its
    /// provider id, and refreshing the name and art already stored -- or,
    /// for `None`, take it out of whichever it was in. A movie is in at most
    /// one collection, so this replaces its membership (idempotent).
    async fn set_movie_collection(
        &self,
        movie_id: Uuid,
        collection: Option<&CollectionEnrichment>,
    ) -> Result<(), DbErr>;

    /// The collection the movie is in, if any.
    async fn movie_collection(&self, movie_id: Uuid) -> Result<Option<Collection>, DbErr>;

    /// The collection with this id, whether or not any movie is in it now.
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Collection>, DbErr>;

    /// The movies in the collection `access` admits (issue #328), in no
    /// particular order: callers order them by what they display. Empty for
    /// an unknown id.
    async fn movie_ids(
        &self,
        collection_id: Uuid,
        access: &AccessPolicy,
    ) -> Result<Vec<Uuid>, DbErr>;

    /// One page of the collections at least one movie `access` admits is
    /// in, with how many such movies are, ordered by name ignoring ASCII
    /// case -- by code point once `A`-`Z` are folded to `a`-`z`, and nothing
    /// else is folded, so the order cannot depend on a store's locale -- and
    /// then by id.
    async fn list(
        &self,
        access: &AccessPolicy,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<CollectionSummary>, DbErr>;

    /// How many collections [`Self::list`] lists across every page.
    async fn count(&self, access: &AccessPolicy) -> Result<u64, DbErr>;
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::watch_state::TitleRef;
    use crate::repositories::access_policy::in_memory::InMemoryTitleAccess;
    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// The in-memory double. The `Default` applies access policies over no
    /// titles; see [`InMemoryTitleAccess`].
    #[derive(Debug, Default)]
    pub struct InMemoryCollectionRepository {
        collections: RwLock<HashMap<Uuid, Collection>>,
        /// Movie id to collection id.
        members: RwLock<HashMap<Uuid, Uuid>>,
        titles: InMemoryTitleAccess,
    }

    impl InMemoryCollectionRepository {
        /// A double applying access policies over the titles `titles` reads.
        pub fn with_titles(titles: InMemoryTitleAccess) -> Self {
            Self {
                titles,
                ..Self::default()
            }
        }

        fn upsert(&self, enrichment: &CollectionEnrichment) -> Uuid {
            let mut collections = self.collections.write();
            let existing = collections
                .values_mut()
                .find(|c| c.external_ref == enrichment.collection_ref.as_str());
            match existing {
                Some(collection) => {
                    collection.name = enrichment.name.clone();
                    collection.poster_url = enrichment.poster_url.clone();
                    collection.backdrop_url = enrichment.backdrop_url.clone();
                    collection.id
                }
                None => {
                    let id = Uuid::new_v4();
                    collections.insert(
                        id,
                        Collection {
                            id,
                            external_ref: enrichment.collection_ref.as_str().to_string(),
                            name: enrichment.name.clone(),
                            poster_url: enrichment.poster_url.clone(),
                            backdrop_url: enrichment.backdrop_url.clone(),
                        },
                    );
                    id
                }
            }
        }

        fn summaries(&self, access: &AccessPolicy) -> Vec<CollectionSummary> {
            let mut counts: HashMap<Uuid, u64> = HashMap::new();
            for (movie_id, collection_id) in self.members.read().iter() {
                if self.titles.admits(access, TitleRef::Movie(*movie_id)) {
                    *counts.entry(*collection_id).or_default() += 1;
                }
            }
            let collections = self.collections.read();
            let mut summaries: Vec<CollectionSummary> = counts
                .into_iter()
                .filter_map(|(id, movie_count)| {
                    collections.get(&id).map(|collection| CollectionSummary {
                        collection: collection.clone(),
                        movie_count,
                    })
                })
                .collect();
            summaries.sort_by(|a, b| {
                a.collection
                    .name
                    .to_ascii_lowercase()
                    .cmp(&b.collection.name.to_ascii_lowercase())
                    .then_with(|| a.collection.id.cmp(&b.collection.id))
            });
            summaries
        }
    }

    #[async_trait]
    impl CollectionRepository for InMemoryCollectionRepository {
        async fn set_movie_collection(
            &self,
            movie_id: Uuid,
            collection: Option<&CollectionEnrichment>,
        ) -> Result<(), DbErr> {
            match collection {
                Some(enrichment) => {
                    let id = self.upsert(enrichment);
                    self.members.write().insert(movie_id, id);
                }
                None => {
                    self.members.write().remove(&movie_id);
                }
            }
            Ok(())
        }

        async fn movie_collection(&self, movie_id: Uuid) -> Result<Option<Collection>, DbErr> {
            let Some(id) = self.members.read().get(&movie_id).copied() else {
                return Ok(None);
            };
            Ok(self.collections.read().get(&id).cloned())
        }

        async fn find_by_id(&self, id: Uuid) -> Result<Option<Collection>, DbErr> {
            Ok(self.collections.read().get(&id).cloned())
        }

        async fn movie_ids(
            &self,
            collection_id: Uuid,
            access: &AccessPolicy,
        ) -> Result<Vec<Uuid>, DbErr> {
            Ok(self
                .members
                .read()
                .iter()
                .filter(|(_, id)| **id == collection_id)
                .map(|(movie_id, _)| *movie_id)
                .filter(|movie_id| self.titles.admits(access, TitleRef::Movie(*movie_id)))
                .collect())
        }

        async fn list(
            &self,
            access: &AccessPolicy,
            limit: u64,
            offset: u64,
        ) -> Result<Vec<CollectionSummary>, DbErr> {
            Ok(self
                .summaries(access)
                .into_iter()
                .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .collect())
        }

        async fn count(&self, access: &AccessPolicy) -> Result<u64, DbErr> {
            Ok(self.summaries(access).len() as u64)
        }
    }
}

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::CollectionRepository;
    use super::in_memory::InMemoryCollectionRepository;
    use crate::repositories::access_policy::in_memory::InMemoryTitleAccess;
    use crate::repositories::contract::fixture::CollectionRepositoryFixture;
    use crate::repositories::movie::in_memory::InMemoryMovieRepository;
    use crate::repositories::show::in_memory::InMemoryShowRepository;
    use crate::repositories::{MovieRepository, ShowRepository};

    /// The hermetic instantiation of the collection contract, the
    /// collection double reading the title doubles to apply a policy.
    #[derive(Debug)]
    pub struct InMemoryFixture {
        repo: InMemoryCollectionRepository,
        movies: Arc<InMemoryMovieRepository>,
        shows: Arc<InMemoryShowRepository>,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            let movies = Arc::new(InMemoryMovieRepository::default());
            let shows = Arc::new(InMemoryShowRepository::default());
            Self {
                repo: InMemoryCollectionRepository::with_titles(InMemoryTitleAccess::new(
                    movies.clone(),
                    shows.clone(),
                )),
                movies,
                shows,
            }
        }
    }

    #[async_trait::async_trait]
    impl CollectionRepositoryFixture for InMemoryFixture {
        fn repo(&self) -> &dyn CollectionRepository {
            &self.repo
        }

        fn movies(&self) -> &dyn MovieRepository {
            self.movies.as_ref()
        }

        fn shows(&self) -> &dyn ShowRepository {
            self.shows.as_ref()
        }

        async fn new_library(&self) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::collection_repository_contract!(setup);
}
