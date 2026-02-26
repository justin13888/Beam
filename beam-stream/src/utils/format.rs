// Format types are defined in beam-index and re-exported here.
// Note: impl From<Resolution> for m3u8_rs::Resolution was removed (orphan rule);
// use m3u8_rs::Resolution { width: r.width as u64, height: r.height as u64 } at call sites.
pub use beam_index::utils::format::{
    ChannelLayout, Disposition, Resolution, SampleFormat, SampleType,
};
