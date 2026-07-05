pub mod codec;
pub mod math;
pub mod stream;

// `beam-domain` stays framework/FFI-agnostic and only exposes plain,
// ffmpeg-free utilities (file, hash). FFmpeg-dependent probing types
// (metadata, media, color, format) now live in beam-index's `probe` module,
// which is the sole ffmpeg-linking crate in the workspace.
pub use beam_domain::utils::{file, hash};
pub use beam_index::probe::{format, metadata};
