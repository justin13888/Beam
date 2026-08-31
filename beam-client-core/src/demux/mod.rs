//! Container demuxing the client owns, because no server will do it.
//!
//! [ADR-0004] commits Beam to direct play: the server never transcodes and
//! never remuxes, so the container is delivered exactly as it sits on disk.
//! That is fine on Android, where Media3 demuxes Matroska natively, and it is
//! not fine on Apple, where AVFoundation demuxes MP4, MOV and a handful of
//! others and simply cannot open an `.mkv` -- which is a large share of a
//! typical self-hosted library.
//!
//! There are only two honest answers to that: tell the viewer the file will
//! never play, or demux it ourselves. This module is the second. It reads
//! Matroska and WebM and hands out **encoded** samples with the codec-private
//! bytes needed to build a decoder; it decodes nothing. Decoding stays with
//! the platform's hardware decoders, which is the whole reason a native client
//! is worth writing ([ADR-0012]).
//!
//! The split is deliberate and is the boundary to defend in review:
//!
//! | Concern | Owner |
//! |---|---|
//! | Fetching bytes | the platform, via [`crate::ports::byte_source::ByteSource`] |
//! | Parsing the container | this module |
//! | Building format descriptions, decoding, rendering, A/V sync | the platform |
//!
//! Nothing here is Apple-specific. Android does not need it today because
//! Media3 already demuxes Matroska, but a future GTK client would.
//!
//! [ADR-0004]: ../../docs/architecture/decisions/ADR-0004-never-transcode.md
//! [ADR-0012]: ../../docs/architecture/decisions/ADR-0012-native-client-rust-core.md

pub mod mkv;
pub mod reader;

pub use mkv::{
    EncodedSample, ExtractorTrack, MatroskaExtractor, SubtitleFormat, TrackKind, probe_containers,
};
pub use reader::ByteSourceReader;
