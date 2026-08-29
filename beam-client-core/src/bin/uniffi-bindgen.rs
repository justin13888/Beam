//! Entry point for `uniffi-bindgen`, invoked by `mise run core:bindgen`.
//!
//! Shipping this as a bin of the crate itself, rather than pinning a separate
//! `uniffi-bindgen` tool, guarantees the Kotlin bindings are produced by the
//! exact UniFFI version that built the `.so`. A mismatch between the two is
//! silent at build time and fails at runtime, so it is worth designing out.

fn main() {
    uniffi::uniffi_bindgen_main()
}
