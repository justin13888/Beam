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
/// surface -- not to route around the diagnostic they currently raise. That
/// diagnostic is real and is filed upstream: Kynos describes a binary body as
/// `"schema": {}`, which is the idiomatic 3.1+ spelling now that `format:
/// binary` is deprecated in favour of `contentMediaType`, and spargen's E009
/// rejects it for wanting a string-like or binary schema. Two first-party tools
/// disagreeing about one media type is exactly the case AGENTS.md says to fix
/// upstream, and is filed as getkono/spargen#72. When it lands, these rules
/// stay: the paragraph above is reason enough on its own, and dropping them
/// would generate the four methods this client is deliberately without. What
/// the fix retires is the second half of this rationale, not the omission.
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
/// The spargen gap here is real and separate, and is filed as
/// getkono/spargen#82: `classify_media` has no arm for a media type *range*,
/// so `image/*` -- the only honest description of a response whose concrete
/// type is chosen per request -- is rejected as `E009`. Naming an exact type
/// instead does not help; `image/jpeg` is not classified either. When that
/// lands, these rules stay for the reason in the paragraph above.
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
