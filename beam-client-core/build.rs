//! Generates the Beam REST client from the vendored OpenAPI document.
//!
//! The client is never hand-written: `api/openapi.json` is exported from
//! `beam-server`'s own handler annotations (`mise run codegen:openapi`), and
//! spargen lowers it to Rust at compile time. A server-side contract change
//! that this crate has not absorbed therefore fails the build rather than
//! drifting silently -- the Rust twin of the TypeScript client's
//! compiler-as-contract-check.

use spargen::{OmitMethod, OmitRule};

/// The media-delivery operations, which this client does not call.
///
/// Playback never goes through the generated client. `MediaSource` carries
/// `stream_url` and `download_url`, `ServerRecord::absolute_url` resolves them
/// against the origin, and the absolute URL is handed to Media3, which does its
/// own HTTP so it can range-request and seek (see `servers.rs` and
/// `ffi.rs::playback_config`). Generating a Rust method that buffers a 40 GiB
/// response into memory would be generating something no caller may use.
///
/// So these are omitted because they are genuinely not part of this client's
/// surface. They once also raised a spargen diagnostic -- E009 rejected the
/// `"schema": {}` Kynos writes for a binary body, getkono/spargen#72 -- which
/// spargen 0.5 fixes. The rules stay: the paragraph above is reason enough on
/// its own, and dropping them would generate the four methods this client is
/// deliberately without.
const MEDIA_DELIVERY: [(OmitMethod, &str); 4] = [
    (OmitMethod::Get, "/v1/files/{file_id}/stream"),
    (OmitMethod::Head, "/v1/files/{file_id}/stream"),
    (OmitMethod::Get, "/v1/files/{file_id}/download"),
    (OmitMethod::Head, "/v1/files/{file_id}/download"),
];

/// The artwork operations, which this client does not call either.
///
/// Poster and backdrop art is fetched by the platform's image loader -- Coil
/// on Android, `URLSession` on Apple -- because those cache to disk, decode
/// incrementally and size to the view. This crate's part is already done by
/// `ServerRecord::absolute_url`, which turns the relative artwork path the
/// catalog carries into the absolute URL the loader is handed (see
/// `catalog.rs`). A generated method returning a `Vec<u8>` of a poster would
/// be a method with no caller, for the same reason as the four above.
///
/// These too once raised E009, because spargen could not classify a media type
/// range such as `image/*` (getkono/spargen#82, fixed in spargen 0.5). The
/// rules stay for the reason in the paragraph above.
const ARTWORK: [(OmitMethod, &str); 2] = [
    (OmitMethod::Get, "/v1/artwork/{kind}/{id}/{variant}"),
    (OmitMethod::Head, "/v1/artwork/{kind}/{id}/{variant}"),
];

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo");

    let mut spec = spargen::Spec::new("api/openapi.json").carve(false);
    for (method, path) in MEDIA_DELIVERY.into_iter().chain(ARTWORK) {
        spec = spec.omit_rule(OmitRule::operation(method, path));
    }

    let build = spec.build(format!("{out_dir}/beam_api.rs"));

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
