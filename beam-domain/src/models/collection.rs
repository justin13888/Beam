//! Provider collections (issue #262): a grouping of movies a metadata
//! provider makes, such as a franchise -- TMDB's `belongs_to_collection`.
//!
//! A collection is identified by the provider's own id for it, so every movie
//! a provider puts in one collection joins one row. A movie belongs to at most
//! one. Membership is written by enrichment, a movie at a time, exactly as its
//! genres are.

use uuid::Uuid;

/// A collection some movie in the library belonged to when it was last
/// enriched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: Uuid,
    /// The provider-qualified id of the collection, e.g. `"tmdb:2344"`: what
    /// makes two movies' collections one.
    pub external_ref: String,
    pub name: String,
    /// The provider's poster for the collection, served through Beam's
    /// artwork route rather than handed to clients (ADR-0015).
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
}

/// A collection in a listing, with how many of the library's movies belong
/// to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionSummary {
    pub collection: Collection,
    /// At least 1: a collection no movie belongs to is not listed.
    pub movie_count: u64,
}

#[cfg(feature = "entity")]
impl From<beam_entity::collection::Model> for Collection {
    fn from(model: beam_entity::collection::Model) -> Self {
        let beam_entity::collection::Model {
            id,
            external_ref,
            name,
            poster_url,
            backdrop_url,
            created_at: _,
            updated_at: _,
        } = model;
        Self {
            id,
            external_ref,
            name,
            poster_url,
            backdrop_url,
        }
    }
}
