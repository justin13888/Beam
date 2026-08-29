//! FFmpeg-based technical metadata probing.
//!
//! This module is the sole place in the workspace that links against
//! `ffmpeg-next`/FFmpeg (for probing at index time; never at stream time).
//! `beam-domain` must stay framework/FFI-agnostic, so these utilities
//! (previously `beam-domain/src/utils/{metadata,media,color,format}.rs`)
//! live here instead.

use std::sync::OnceLock;

pub mod color;
pub mod format;
pub mod media;
pub mod metadata;

#[cfg(test)]
mod real_media_tests;

/// Initialize the FFmpeg bindings, before any probing happens. Exposed here
/// (rather than requiring callers to depend on `ffmpeg-next` directly) since
/// this crate is the sole place in the workspace that links against it.
///
/// Safe to call from anywhere, any number of times, from any thread: the first
/// call initialises and every later call returns that same result.
///
/// The guard is load-bearing, not defensive. `ffmpeg_next::init` walks
/// FFmpeg's process-global registries and is not safe to run concurrently with
/// itself -- two threads racing it segfault the process. This function
/// previously just forwarded the call and documented "must be called once",
/// which left the invariant to every caller and held only while exactly one
/// caller existed. It is enforced here instead.
///
/// `#[mutants::skip]`: replacing this body with `Ok(())` is an equivalent
/// mutant. On FFmpeg >= 5 `av_register_all` no longer exists and
/// `ffmpeg_next::init` has no effect this workspace can observe -- verified by
/// stubbing it out, after which the whole probe suite, including the tests
/// that demux real containers, still passes. The call is kept because the
/// bindings document it as required and a future FFmpeg may reinstate an
/// effect. See ADR-0011's decision log.
#[mutants::skip]
pub fn init() -> Result<(), ffmpeg_next::Error> {
    static INIT: OnceLock<Result<(), ffmpeg_next::Error>> = OnceLock::new();
    *INIT.get_or_init(ffmpeg_next::init)
}
