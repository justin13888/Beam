use async_graphql::*;

use schema::*;

use crate::state::AppState;

pub mod guard;
pub mod schema;

#[cfg(test)]
mod auth_tests;

pub use guard::{AdminGuard, AuthGuard};

pub type AppSchema = Schema<QueryRoot, MutationRoot, SubscriptionRoot>;

pub fn create_schema(state: AppState) -> AppSchema {
    Schema::build(
        QueryRoot::default(),
        MutationRoot::default(),
        SubscriptionRoot::default(),
    )
    .data(state)
    .finish()
}
