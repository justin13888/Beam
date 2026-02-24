// use std::collections::HashMap;

use async_graphql::*;

use admin::AdminQuery;
use library::{LibraryMutation, LibraryQuery};
use media::{MediaMutation, MediaQuery};

pub mod admin;
pub mod library;
pub mod media;

pub struct QueryRoot {
    pub library: LibraryQuery,
    pub media: MediaQuery,
    pub admin: AdminQuery,
}

#[Object]
impl QueryRoot {
    async fn library(&self) -> &LibraryQuery {
        &self.library
    }

    async fn media(&self) -> &MediaQuery {
        &self.media
    }

    async fn admin(&self) -> &AdminQuery {
        &self.admin
    }
}

pub struct MutationRoot {
    pub library: LibraryMutation,
    pub media: MediaMutation,
}

#[Object]
impl MutationRoot {
    async fn library(&self) -> &LibraryMutation {
        &self.library
    }

    async fn media(&self) -> &MediaMutation {
        &self.media
    }
}
