//! Export the OpenAPI specification as JSON to stdout.
//!
//! Run via: `cargo run --example export_openapi > ../beam-web/openapi.json`
//!
//! This creates the docs router (REST-only, no real state required) and
//! merges it with the OpenAPI builder to produce the full spec.

use beam_stream::routes::create_docs_router;
use salvo::prelude::*;

fn main() {
    let router = create_docs_router();
    let doc = OpenApi::new("Beam Stream API", "1.0.0").merge_router(&router);
    let json = serde_json::to_string_pretty(&doc).expect("Failed to serialize OpenAPI spec");
    println!("{json}");
}
