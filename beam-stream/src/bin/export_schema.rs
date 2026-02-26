//! Export GraphQL schema to a file for code generation

use async_graphql::Schema;
use beam_stream::graphql::schema::{MutationRoot, QueryRoot, SubscriptionRoot};
use eyre::Result;
use std::fs;

fn main() -> Result<()> {
    // Build schema from type definitions only - no runtime services needed
    let schema = Schema::build(
        QueryRoot::default(),
        MutationRoot::default(),
        SubscriptionRoot::default(),
    )
    .finish();

    let sdl = schema.sdl();

    let output_path = "schema.graphql";
    fs::write(output_path, &sdl)?;

    println!("GraphQL schema exported to: {output_path}");

    Ok(())
}
