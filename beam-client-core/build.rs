//! Generates the Beam REST client from the vendored OpenAPI document.
//!
//! The client is never hand-written: `api/openapi.json` is exported from
//! `beam-server`'s own handler annotations (`mise run codegen:openapi:core`),
//! and spargen lowers it to Rust at compile time. A server-side contract
//! change that this crate has not absorbed therefore fails the build rather
//! than drifting silently -- the Rust twin of the TypeScript client's
//! compiler-as-contract-check.

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo");
    let build = spargen::Spec::new("api/openapi.json")
        .carve(false)
        .build(format!("{out_dir}/beam_api.rs"));

    let report = spargen::generate(&build);
    for diagnostic in report.diagnostics() {
        println!("cargo::warning={diagnostic}");
    }
    assert!(
        matches!(
            report.outcome(),
            spargen::Outcome::Generated | spargen::Outcome::Cached
        ),
        "spargen could not generate the Beam client: {report}"
    );
}
