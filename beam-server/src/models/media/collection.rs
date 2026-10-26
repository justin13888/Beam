//! Wire shapes for provider collections (issue #262): a movie's collection
//! on its detail, the collection listing, and one collection's movies.

use kynos::Schema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::MovieMetadata;
use crate::models::search::PageInfo;

/// The collection a movie is in, as its detail names it.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct CollectionRef {
    /// The collection: `GET /v1/collections/{id}` lists its movies.
    pub id: Uuid,
    /// The collection's name, as the provider gives it.
    pub name: String,
}

/// A provider collection with at least one movie in the library.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct Collection {
    /// Stable identifier for this collection.
    pub id: Uuid,
    /// The collection's name, as the provider gives it.
    pub name: String,
    /// Path of the collection's poster, served by the artwork route; absent
    /// when the provider has none.
    pub poster_url: Option<String>,
    /// Path of the collection's backdrop, served by the artwork route;
    /// absent when the provider has none.
    pub backdrop_url: Option<String>,
    /// How many of the library's movies are in it: at least 1.
    pub movie_count: u64,
}

/// One page of the collection listing, by name.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct CollectionConnection {
    /// The collections on this page, by name.
    pub items: Vec<Collection>,
    /// Where this page sits in the whole listing.
    pub page_info: PageInfo,
    /// How many collections there are, across every page.
    pub total: u64,
}

/// One collection, with every movie of the library in it.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct CollectionDetail {
    /// Stable identifier for this collection.
    pub id: Uuid,
    /// The collection's name, as the provider gives it.
    pub name: String,
    /// Path of the collection's poster, served by the artwork route; absent
    /// when the provider has none.
    pub poster_url: Option<String>,
    /// Path of the collection's backdrop, served by the artwork route;
    /// absent when the provider has none.
    pub backdrop_url: Option<String>,
    /// The collection's movies, in release order -- by year, then release
    /// date, a movie with neither last -- each as a browse result lists it,
    /// with the viewer's state.
    pub movies: Vec<MovieMetadata>,
}
