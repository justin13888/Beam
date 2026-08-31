//! Export the OpenAPI specification as JSON to stdout.
//!
//! Run via `mise run codegen:openapi`, which writes it to
//! `beam-web/openapi.json` and regenerates the TypeScript client from it.
//!
//! The router is built without a database, a listener, or any initialized
//! service: `Router::openapi()` reads the same declarations `Router::build()`
//! routes on, so there is nothing to start. That is also why this no longer
//! needs a separate "docs router" -- under Salvo the description was assembled
//! by a second pass over the route table and the two could disagree, which is
//! exactly what `routes/contract_tests.rs` existed to catch.

use beam_server::routes::create_router;
use kynos::openapi::SpecVersion;

fn main() -> kynos::Result<()> {
    // No `.info(..)` here: `create_router` carries it, so the document this
    // exports and the one the running server serves at `/api-doc/openapi.json`
    // come from the same value rather than two that must be kept in step.
    let document = create_router().openapi_as(SpecVersion::V3_2)?;

    // Pretty-printed rather than `Document::to_json`, which is compact: the
    // spec is committed, so `mise run codegen:openapi:check` turns a contract
    // change into a reviewable diff, and a 100 KB single line is not one.
    let json = serde_json::to_string_pretty(&document).expect("a document serializes");
    println!("{json}");
    Ok(())
}
