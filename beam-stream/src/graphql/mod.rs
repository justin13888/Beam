// use parking_lot::RwLock;

use async_graphql::*;

use schema::*;

use crate::{
    graphql::schema::{
        admin::AdminQuery,
        library::{LibraryMutation, LibraryQuery},
        media::{MediaMutation, MediaQuery},
    },
    state::AppState,
};

pub mod guard;
pub mod schema;

pub use guard::{AdminGuard, AuthGuard};

pub type AppSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

pub fn create_schema(state: AppState) -> AppSchema {
    Schema::build(
        QueryRoot {
            library: LibraryQuery,
            media: MediaQuery,
            admin: AdminQuery,
        },
        MutationRoot {
            library: LibraryMutation,
            media: MediaMutation,
        },
        EmptySubscription,
    )
    .data(state)
    .finish()
}
