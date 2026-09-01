//! What a download *is*, and where each one has got to.
//!
//! The platform moves the bytes and the core remembers everything else. That
//! split is not arbitrary. Moving bytes is the one part that genuinely cannot
//! be shared: Android's `DownloadManager` writes into the same `Cache`
//! ExoPlayer reads from, so a download and a partially-watched stream are the
//! same bytes, and Apple's background `URLSession` keeps transferring after the
//! app is suspended. Neither is expressible in Rust, and reimplementing either
//! would lose a viewer's part-finished download to a tunnel.
//!
//! Everything above that *was* written twice, and had already diverged by the
//! time this module was written: Android modelled six download states and Apple
//! five, with different payloads and two separate implementations of "fraction
//! complete". Two clients disagreeing about whether a download is paused or
//! waiting for a network is precisely the failure ADR-0012 exists to prevent,
//! so the vocabulary and the bookkeeping live here once.

mod catalog;
mod record;

pub use catalog::DownloadCatalog;
pub use record::{DownloadRecord, DownloadState};
