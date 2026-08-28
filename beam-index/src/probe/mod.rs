//! FFmpeg-based technical metadata probing.
//!
//! This module is the sole place in the workspace that links against
//! `ffmpeg-next`/FFmpeg (for probing at index time; never at stream time).
//! `beam-domain` must stay framework/FFI-agnostic, so these utilities
//! (previously `beam-domain/src/utils/{metadata,media,color,format}.rs`)
//! live here instead.

pub mod color;
pub mod format;
pub mod media;
pub mod metadata;

#[cfg(test)]
mod real_media_tests;

/// Initialize the FFmpeg bindings. Must be called once, before any probing
/// happens. Exposed here (rather than requiring callers to depend on
/// `ffmpeg-next` directly) since this crate is the sole place in the
/// workspace that links against it.
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
    ffmpeg_next::init()
}
