use async_graphql::*;

use schema::*;

use crate::state::AppState;

pub mod guard;
pub mod schema;

pub use guard::AuthGuard;

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
