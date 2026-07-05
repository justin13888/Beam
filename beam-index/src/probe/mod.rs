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
