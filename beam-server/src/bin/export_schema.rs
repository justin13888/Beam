//! Export the GraphQL schema as SDL to stdout.
//!
//! Run via: `cargo run --bin export_schema -p beam-server > schema.graphql`

use async_graphql::Schema;
use beam_server::graphql::schema::{MutationRoot, QueryRoot, SubscriptionRoot};

fn main() {
    // Build schema from type definitions only — no runtime services needed
    let schema = Schema::build(
        QueryRoot::default(),
        MutationRoot::default(),
        SubscriptionRoot::default(),
    )
    .finish();

    print!("{}", schema.sdl());
}
