//! Trickplay: the thumbnails a player shows while the viewer scrubs
//! (issue #265).
//!
//! Beam generates them in the background, one set per file, as JPEG sheets
//! of tiles -- a grid of frames, one every `interval_ms`, left to right then
//! top to bottom -- under its own data directory, never beside the library's
//! files (FR-202). A [`TrickplayRecord`] says which version of the file a
//! set was made from, so a file whose content changed is made again.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// How one file's trickplay sheets are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrickplayLayout {
    /// The time between two tiles: tile `n` is the frame at
    /// `n * interval_ms`.
    pub interval_ms: u32,
    /// One tile's width in pixels.
    pub tile_width: u32,
    /// One tile's height in pixels.
    pub tile_height: u32,
    /// Tiles across one sheet.
    pub columns: u32,
    /// Tiles down one sheet.
    pub rows: u32,
    /// Tiles in every sheet together; the last sheet may be part-filled.
    pub tile_count: u32,
}

impl TrickplayLayout {
    /// How many tiles one full sheet holds.
    pub fn tiles_per_sheet(&self) -> u32 {
        self.columns.saturating_mul(self.rows)
    }

    /// How many sheets the tiles fill: the last one part-filled when
    /// `tile_count` is not a multiple of a sheet's tiles.
    pub fn sheet_count(&self) -> u32 {
        match self.tiles_per_sheet() {
            0 => 0,
            per_sheet => self.tile_count.div_ceil(per_sheet),
        }
    }
}

/// What Beam made of one file's trickplay, for the version of the file
/// whose hash is `file_hash`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrickplayRecord {
    pub file_id: Uuid,
    /// The content hash of the file the sheets were made from
    /// ([`crate::models::MediaFile::hash`]). A file whose hash moved since is
    /// made again.
    pub file_hash: u64,
    /// The sheets' layout; `None` when the file gave none -- it has no video
    /// stream, or its frames could not be decoded -- so it is not tried
    /// again until its content changes.
    pub layout: Option<TrickplayLayout>,
    pub generated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(columns: u32, rows: u32, tile_count: u32) -> TrickplayLayout {
        TrickplayLayout {
            interval_ms: 10_000,
            tile_width: 320,
            tile_height: 180,
            columns,
            rows,
            tile_count,
        }
    }

    #[test]
    fn the_sheets_hold_every_tile_the_last_one_part_filled() {
        let cases = [
            (layout(10, 10, 0), 0),
            (layout(10, 10, 1), 1),
            (layout(10, 10, 100), 1),
            (layout(10, 10, 101), 2),
            (layout(10, 10, 720), 8),
            (layout(4, 3, 25), 3),
            (layout(0, 10, 5), 0),
        ];
        for (layout, sheets) in cases {
            assert_eq!(layout.sheet_count(), sheets, "{layout:?}");
        }
    }
}
