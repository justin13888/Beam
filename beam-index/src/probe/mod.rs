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

/// Initialize the FFmpeg bindings. Must be called once, before any probing
/// happens. Exposed here (rather than requiring callers to depend on
/// `ffmpeg-next` directly) since this crate is the sole place in the
/// workspace that links against it.
pub fn init() -> Result<(), ffmpeg_next::Error> {
    ffmpeg_next::init()
}
