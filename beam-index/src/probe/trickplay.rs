//! Trickplay sheets from a video file's frames (issue #265).
//!
//! A sheet is a JPEG grid of tiles, each the frame at `n * interval`,
//! scaled to [`TrickplaySpec::tile_width`] wide. Each tile is the keyframe at
//! or before its time: the demuxer seeks back to it and the first frame the
//! decoder gives is taken, so a tile costs one group of pictures read and
//! one frame decoded -- not the whole file decoded, which for a long HEVC
//! remux takes as long as watching it.
//!
//! The file is read through its open handle and nothing else, as the probe
//! reads it ([`super::metadata::open_input`]), and only `out_dir` is written:
//! never the library (FR-202).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use ffmpeg_next as ffmpeg;
use thiserror::Error;
use tracing::{debug, trace};

use beam_domain::models::trickplay::TrickplayLayout;

use crate::library_file::LibraryFile;
use crate::probe::metadata::{MetadataError, open_input};

/// How a file's sheets are made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrickplaySpec {
    /// The time between two tiles, in milliseconds.
    pub interval_ms: u32,
    /// A tile's width in pixels; its height keeps the picture's shape.
    pub tile_width: u32,
    /// Tiles across a sheet.
    pub columns: u32,
    /// Tiles down a sheet.
    pub rows: u32,
    /// The most tiles one file is given: a longer file's last tiles are
    /// dropped rather than its sheets growing without bound.
    pub max_tiles: u32,
}

impl Default for TrickplaySpec {
    /// A tile every 10 seconds, 320 pixels wide, 100 to a sheet, for files of
    /// up to ten hours. A two-hour film is 720 tiles on 8 sheets, each sheet
    /// 3200 pixels wide and a few hundred kilobytes.
    fn default() -> Self {
        Self {
            interval_ms: 10_000,
            tile_width: 320,
            columns: 10,
            rows: 10,
            max_tiles: 3_600,
        }
    }
}

#[derive(Debug, Error)]
pub enum TrickplayError {
    #[error("FFmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg::Error),
    #[error("failed to open the file: {0}")]
    Open(#[from] MetadataError),
    #[error("failed to read the file: {0}")]
    Read(std::io::Error),
    #[error("failed to write a sheet: {0}")]
    Write(#[from] std::io::Error),
    #[error("making the sheets was cancelled")]
    Cancelled,
}

impl TrickplayError {
    /// Whether the error is a property of the file's content -- it is not
    /// media FFmpeg can demux, or its video has no decoder -- and so will
    /// recur until the content changes. Anything else (a failed rewind, a
    /// read error from a flaky mount, Beam's own folder not written, a
    /// cancellation) says nothing about the file, and is worth another try.
    pub fn is_content_fault(&self) -> bool {
        fn of_content(error: &ffmpeg::Error) -> bool {
            matches!(
                error,
                ffmpeg::Error::InvalidData
                    | ffmpeg::Error::DemuxerNotFound
                    | ffmpeg::Error::DecoderNotFound
                    | ffmpeg::Error::StreamNotFound
            )
        }
        match self {
            TrickplayError::Ffmpeg(error) => of_content(error),
            TrickplayError::Open(MetadataError::FfmpegError(error)) => of_content(error),
            TrickplayError::Open(MetadataError::InvalidMetadata(_)) => true,
            TrickplayError::Open(MetadataError::UnknownError(_))
            | TrickplayError::Read(_)
            | TrickplayError::Write(_)
            | TrickplayError::Cancelled => false,
        }
    }
}

/// A demuxer read error as the I/O error it is: FFmpeg's `AVERROR(errno)`
/// back to its `errno`, anything else wrapped.
fn read_error(error: ffmpeg::Error) -> std::io::Error {
    match error {
        ffmpeg::Error::Other { errno } => std::io::Error::from_raw_os_error(errno),
        other => std::io::Error::other(other),
    }
}

/// The largest number of packets read after a seek while waiting for the
/// decoder's first frame. A stream that gives none in this many is not
/// decodable from there, and the tile keeps the one before it.
const MAX_PACKETS_PER_TILE: usize = 2_000;

/// The height of a tile `tile_width` wide showing a picture `width` by
/// `height` stored pixels with sample aspect ratio `sar`: even, as 4:2:0
/// chroma needs, and at least 2.
fn tile_height(tile_width: u32, width: u32, height: u32, sar: (i32, i32)) -> u32 {
    let (num, den) = match sar {
        (num, den) if num > 0 && den > 0 => (f64::from(num), f64::from(den)),
        _ => (1.0, 1.0),
    };
    let display_width = f64::from(width) * num / den;
    if display_width <= 0.0 || height == 0 {
        return 2;
    }
    let exact = f64::from(tile_width) * f64::from(height) / display_width;
    // At most the tile width ten times over: no real picture is taller.
    let clamped = exact.clamp(2.0, f64::from(tile_width) * 10.0);
    ((clamped / 2.0).round() as u32).max(1) * 2
}

/// How many tiles a file `duration_ms` long is given: one for each interval
/// it starts, and at most `spec.max_tiles`.
fn tile_count(duration_ms: u64, spec: &TrickplaySpec) -> u32 {
    let interval = u64::from(spec.interval_ms.max(1));
    let count = duration_ms.div_ceil(interval).max(1);
    u32::try_from(count)
        .unwrap_or(u32::MAX)
        .min(spec.max_tiles.max(1))
}

/// Make `file`'s sheets in `out_dir`, as `0.jpg`, `1.jpg` and so on, and
/// return their layout -- or `None` for a file with no video stream to make
/// them from, or no duration to space them over. Blocking: run it off the
/// async runtime.
///
/// `cancel` is read before each tile: once it is set the work stops there
/// with [`TrickplayError::Cancelled`], so a server shutting down waits for
/// one tile rather than a whole file's sheets.
pub fn generate_sheets(
    file: LibraryFile,
    out_dir: &Path,
    spec: &TrickplaySpec,
    cancel: &AtomicBool,
) -> Result<Option<TrickplayLayout>, TrickplayError> {
    let (mut input, _metadata, path) = open_input(file)?;
    trace!(path = %path.display(), "making trickplay sheets");

    let Some(stream) = input
        .streams()
        .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
        .find(|stream| {
            !stream
                .disposition()
                .contains(ffmpeg::format::stream::Disposition::ATTACHED_PIC)
        })
    else {
        debug!(path = %path.display(), "no video stream to make trickplay sheets from");
        return Ok(None);
    };
    let stream_index = stream.index();
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .video()?;

    let duration_ms = match input.duration() {
        duration if duration > 0 => {
            u64::try_from(duration).unwrap_or(0)
                / (i64::from(ffmpeg::ffi::AV_TIME_BASE) as u64 / 1000)
        }
        _ => 0,
    };
    if duration_ms == 0 || decoder.width() == 0 || decoder.height() == 0 {
        debug!(path = %path.display(), "no duration or picture size to space trickplay tiles over");
        return Ok(None);
    }

    let aspect = decoder.aspect_ratio();
    let layout = TrickplayLayout {
        interval_ms: spec.interval_ms.max(1),
        tile_width: spec.tile_width.max(2) & !1,
        tile_height: tile_height(
            spec.tile_width.max(2) & !1,
            decoder.width(),
            decoder.height(),
            (aspect.numerator(), aspect.denominator()),
        ),
        columns: spec.columns.max(1),
        rows: spec.rows.max(1),
        tile_count: tile_count(duration_ms, spec),
    };

    // Tile times are from the file's start, which for MPEG-TS and M2TS is
    // seldom timestamp zero: seek targets are offset by the container's
    // start time, as `ffmpeg -ss` offsets them.
    // SAFETY: `input` owns a live `AVFormatContext`; `start_time` is a plain
    // integer field, `AV_NOPTS_VALUE` when the demuxer found none.
    let start_us = match unsafe { (*input.as_ptr()).start_time } {
        ffmpeg::ffi::AV_NOPTS_VALUE => 0,
        start => start,
    };

    let mut tiles = TileSheets::new(layout)?;
    let mut scaler: Option<ffmpeg::software::scaling::Context> = None;
    let mut decoded = ffmpeg::frame::Video::empty();
    let mut scaled = ffmpeg::frame::Video::empty();
    let mut have_frame = false;

    for tile in 0..layout.tile_count {
        if cancel.load(Ordering::Relaxed) {
            debug!(path = %path.display(), tile, "making trickplay sheets cancelled");
            return Err(TrickplayError::Cancelled);
        }
        let target_us =
            start_us.saturating_add(i64::from(tile) * i64::from(layout.interval_ms) * 1_000);
        // Back to the keyframe at or before the tile's time: the range's end
        // is `avformat_seek_file`'s inclusive `max_ts`. A seek that fails
        // keeps whatever the demuxer was reading, and the next frame decoded
        // from there stands in.
        if let Err(err) = input.seek(target_us, ..target_us) {
            trace!(tile, %err, "seek for a trickplay tile failed");
        }
        decoder.flush();
        if next_frame(&mut input, &mut decoder, stream_index, &mut decoded)? {
            let scaler = match &mut scaler {
                Some(scaler)
                    if scaler.input().format == decoded.format()
                        && scaler.input().width == decoded.width()
                        && scaler.input().height == decoded.height() =>
                {
                    scaler
                }
                slot => slot.insert(ffmpeg::software::scaling::Context::get(
                    decoded.format(),
                    decoded.width(),
                    decoded.height(),
                    ffmpeg::format::Pixel::YUVJ420P,
                    layout.tile_width,
                    layout.tile_height,
                    ffmpeg::software::scaling::Flags::BILINEAR,
                )?),
            };
            scaler.run(&decoded, &mut scaled)?;
            have_frame = true;
        }
        // A tile with no frame of its own repeats the one before it, so the
        // tiles keep their times; one before any frame stays black.
        if have_frame {
            tiles.place(tile, &scaled);
        }
        if tiles.is_full(tile) {
            tiles.write(out_dir)?;
        }
    }

    if !have_frame {
        debug!(path = %path.display(), "no frame of the video stream decoded");
        return Ok(None);
    }
    Ok(Some(layout))
}

/// Read packets of the stream `stream_index` into `decoder` until it gives a
/// frame, into `frame`; `false` when the stream ends, or
/// [`MAX_PACKETS_PER_TILE`] pass, with none.
fn next_frame(
    input: &mut ffmpeg::format::context::Input,
    decoder: &mut ffmpeg::decoder::Video,
    stream_index: usize,
    frame: &mut ffmpeg::frame::Video,
) -> Result<bool, TrickplayError> {
    let mut packet = ffmpeg::Packet::empty();
    for _ in 0..MAX_PACKETS_PER_TILE {
        match packet.read(input) {
            Ok(()) => {}
            Err(ffmpeg::Error::Eof) => {
                // Drain what the decoder still holds.
                let _ = decoder.send_eof();
                return Ok(decoder.receive_frame(frame).is_ok());
            }
            // One corrupt packet: the demuxer resyncs past it.
            Err(ffmpeg::Error::InvalidData) => continue,
            Err(err) => return Err(TrickplayError::Read(read_error(err))),
        }
        if packet.stream() != stream_index {
            continue;
        }
        // A packet the decoder refuses is one broken packet, not a broken
        // stream: the next keyframe decodes.
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        if decoder.receive_frame(frame).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The sheet being filled, and how many have been written.
struct TileSheets {
    layout: TrickplayLayout,
    sheet: ffmpeg::frame::Video,
    encoder: ffmpeg::encoder::Video,
    written: u32,
}

impl TileSheets {
    fn new(layout: TrickplayLayout) -> Result<Self, TrickplayError> {
        let width = layout.tile_width * layout.columns;
        let height = layout.tile_height * layout.rows;
        let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::MJPEG)
            .ok_or(ffmpeg::Error::EncoderNotFound)?;
        let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()?;
        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(ffmpeg::format::Pixel::YUVJ420P);
        encoder.set_time_base((1, 1));
        // A fixed quantiser, rather than a bit rate: every sheet the same
        // quality, about JPEG quality 75.
        encoder.set_flags(ffmpeg::codec::Flags::QSCALE);
        encoder.set_global_quality(5 * ffmpeg::ffi::FF_QP2LAMBDA);
        let encoder = encoder.open()?;
        let mut sheets = Self {
            layout,
            sheet: ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUVJ420P, width, height),
            encoder,
            written: 0,
        };
        sheets.clear();
        Ok(sheets)
    }

    /// Paint the sheet black: full-range luma 0, neutral chroma.
    fn clear(&mut self) {
        self.sheet.data_mut(0).fill(0);
        self.sheet.data_mut(1).fill(128);
        self.sheet.data_mut(2).fill(128);
    }

    /// Copy `tile`, a frame scaled to the tile's size, into its place on the
    /// sheet: left to right, then top to bottom.
    fn place(&mut self, tile: u32, frame: &ffmpeg::frame::Video) {
        let per_sheet = self.layout.tiles_per_sheet();
        let slot = tile % per_sheet;
        let x = (slot % self.layout.columns * self.layout.tile_width) as usize;
        let y = (slot / self.layout.columns * self.layout.tile_height) as usize;
        let (width, height) = (
            self.layout.tile_width as usize,
            self.layout.tile_height as usize,
        );
        for plane in 0..3 {
            // 4:2:0: chroma planes are half the size each way.
            let shift = usize::from(plane > 0);
            let (px, py, pw, ph) = (x >> shift, y >> shift, width >> shift, height >> shift);
            let source_stride = frame.stride(plane);
            let sheet_stride = self.sheet.stride(plane);
            let source = frame.data(plane);
            let sheet = self.sheet.data_mut(plane);
            for row in 0..ph {
                let from = row * source_stride;
                let to = (py + row) * sheet_stride + px;
                sheet[to..to + pw].copy_from_slice(&source[from..from + pw]);
            }
        }
    }

    /// Whether `tile` is the last of its sheet, or of the file.
    fn is_full(&self, tile: u32) -> bool {
        let per_sheet = self.layout.tiles_per_sheet();
        tile % per_sheet == per_sheet - 1 || tile + 1 == self.layout.tile_count
    }

    /// Encode the sheet to `out_dir/{n}.jpg`, and start the next one black.
    fn write(&mut self, out_dir: &Path) -> Result<(), TrickplayError> {
        self.sheet.set_pts(Some(i64::from(self.written)));
        self.encoder.send_frame(&self.sheet)?;
        let mut packet = ffmpeg::Packet::empty();
        self.encoder.receive_packet(&mut packet)?;
        let bytes = packet.data().unwrap_or_default();
        std::fs::write(out_dir.join(format!("{}.jpg", self.written)), bytes)?;
        self.written += 1;
        self.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_keeps_the_pictures_shape_at_an_even_height() {
        let cases = [
            // 16:9 square pixels.
            ((320, 1920, 1080, (1, 1)), 180),
            // 2.39:1 scope.
            ((320, 1920, 804, (1, 1)), 134),
            // 4:3.
            ((320, 640, 480, (1, 1)), 240),
            // Anamorphic DVD: 720x480 stored, 16:9 shown (SAR 32:27).
            ((320, 720, 480, (32, 27)), 180),
            // An unset aspect ratio (0:1) is square pixels.
            ((320, 1920, 1080, (0, 1)), 180),
            // A one-pixel-high picture still gets a tile.
            ((320, 1920, 1, (1, 1)), 2),
            // A tall picture is bounded.
            ((320, 10, 100_000, (1, 1)), 3_200),
        ];
        for ((tile_width, width, height, sar), expected) in cases {
            let got = tile_height(tile_width, width, height, sar);
            assert_eq!(got, expected, "{width}x{height} sar {sar:?}");
            assert_eq!(got % 2, 0);
        }
    }

    #[test]
    fn a_file_gets_a_tile_for_each_interval_it_starts_up_to_the_cap() {
        let spec = TrickplaySpec {
            max_tiles: 100,
            ..TrickplaySpec::default()
        };
        let cases = [
            (1, 1),
            (9_999, 1),
            (10_000, 1),
            (10_001, 2),
            (7_200_000, 100),
        ];
        for (duration_ms, expected) in cases {
            assert_eq!(tile_count(duration_ms, &spec), expected, "{duration_ms}");
        }
        assert_eq!(tile_count(7_200_000, &TrickplaySpec::default()), 720);
    }
}
