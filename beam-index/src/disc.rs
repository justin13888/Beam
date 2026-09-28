//! Reading a DVD or Blu-ray disc structure copied whole for its main title
//! (issue #234).
//!
//! A disc structure is one source of the title its enclosing folder names
//! ([`beam_domain::utils::media_path::infer_media`]). Which of its stream
//! files that source plays is the disc's to say, not a path's:
//!
//! * **DVD** (`VIDEO_TS/`): the main title set is the one whose longest
//!   program chain lasts longest, as its `VTS_nn_0.IFO` records it; its
//!   `VTS_nn_1.VOB`, `VTS_nn_2.VOB`, ... are played in turn. When any title
//!   set's IFO cannot be parsed, the title set with the most bytes is taken
//!   instead -- the film is almost always the largest -- rather than
//!   comparing durations some sets lack.
//! * **Blu-ray** (`BDMV/`): the main playlist is the `PLAYLIST/*.mpls` whose
//!   play items last longest, among those whose every clip is in `STREAM/`;
//!   its clips are played in the playlist's order, each once. With no such
//!   playlist, the largest clip alone is taken.
//!
//! Only the folders the choice needs are listed: `VIDEO_TS/` itself, or a
//! Blu-ray's `PLAYLIST/` and `STREAM/`. A disc one of those cannot be listed
//! in is not read; a `BACKUP/`, `JAR/` or `AUXDATA/` folder is never looked
//! into, so one that cannot be read changes nothing.
//!
//! Everything here reads, and nothing writes: a disc is listed with no link
//! followed, its stream files are stat'ed with a [`StatCursor`], and an IFO
//! or a playlist is read through the no-follow opener ([`LibraryFile`]).

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{debug, warn};
use walkdir::WalkDir;

use beam_domain::utils::path_policy::{
    DiscKind, PathDisposition, PathPolicy, disc_stream_kind, dvd_title_file,
};

use crate::library_file::{LibraryFile, StatCursor, is_refusal};

/// The largest IFO read: a title set's IFO is tens to hundreds of KiB, and a
/// file larger than this is no IFO.
const MAX_IFO_BYTES: u64 = 4 * 1024 * 1024;

/// The largest playlist read: an MPLS is a few KiB.
const MAX_PLAYLIST_BYTES: u64 = 1024 * 1024;

/// A DVD sector: the unit an IFO's pointers count in.
const DVD_SECTOR_BYTES: usize = 2048;

/// The clock an MPLS play item's in and out times count in.
const MPLS_TICKS_PER_SEC: u64 = 45_000;

/// What reading a disc structure found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DiscRead {
    /// The stream files the disc's main title plays, in the order it plays
    /// them. Empty when the disc offers none, or could not be read whole.
    pub title: Vec<PathBuf>,
    /// How many stream files the disc holds, played, ignored or neither: the
    /// video files the empty-root guard counts.
    pub streams: usize,
    /// Whether some of the disc could not be read -- a folder that failed to
    /// list, a file that failed to stat or read for a reason that says
    /// nothing about it. `title` is then empty: a title chosen from part of
    /// a disc may not be its main title.
    pub failed: bool,
}

/// The part a file of a disc's main title is: its place in `title`, from 1,
/// when the title plays more than one file. A title of one file is played
/// whole, and a file not in the title is none of it.
pub(crate) fn part_in(title: &[PathBuf], path: &Path) -> Option<u32> {
    if title.len() < 2 {
        return None;
    }
    let at = title.iter().position(|file| file == path)?;
    u32::try_from(at + 1).ok()
}

/// A stream file of a disc, and its size.
#[derive(Debug, Clone)]
struct Stream {
    path: PathBuf,
    size: u64,
}

/// Read the disc structure of `kind` rooted at `disc` -- a `VIDEO_TS/` or
/// `BDMV/` folder beneath the library `root` -- for its main title. A stream
/// file `policy` does not offer as one of this disc's
/// ([`PathDisposition::DiscStream`]) -- one an ignore pattern matches -- is
/// neither counted nor played.
pub(crate) fn read_disc(root: &Path, disc: &Path, kind: DiscKind, policy: &PathPolicy) -> DiscRead {
    let mut cursor = StatCursor::new(root);
    let mut streams: Vec<Stream> = Vec::new();
    let mut metadata: Vec<PathBuf> = Vec::new();
    let mut failed = false;
    let mut seen = 0usize;
    // A DVD's files are all in `VIDEO_TS/`; a Blu-ray's main title needs
    // only its `PLAYLIST/` and `STREAM/` folders, so no other is listed.
    let walk = WalkDir::new(disc)
        .follow_links(false)
        .follow_root_links(false)
        .min_depth(1)
        .max_depth(match kind {
            DiscKind::Dvd => 1,
            DiscKind::BluRay => 2,
        })
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() != 1
                || !entry.file_type().is_dir()
                || entry.file_name().eq_ignore_ascii_case("playlist")
                || entry.file_name().eq_ignore_ascii_case("stream")
        });
    for entry in walk {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warn!(disc = %disc.display(), error = %err, "could not read part of a disc structure");
                failed = true;
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        let rel = path.strip_prefix(root).unwrap_or(&path);
        // Counted as the walk counts any video file: before the policy has
        // its say, so a disc of ignored streams is still a mounted one.
        if disc_stream_kind(rel) == Some(kind) {
            seen += 1;
        }
        match policy.disposition(rel) {
            PathDisposition::DiscStream(found) if found == kind => match cursor.stat(&path) {
                Ok(meta) => streams.push(Stream {
                    path,
                    size: meta.size(),
                }),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound || is_refusal(&err) => {}
                Err(err) => {
                    warn!(path = %path.display(), error = %err, "could not stat a stream file of a disc structure");
                    failed = true;
                }
            },
            _ => {
                if is_title_metadata(kind, disc, &path) {
                    metadata.push(path);
                }
            }
        }
    }
    let count = seen;
    if failed {
        return DiscRead {
            title: Vec::new(),
            streams: count,
            failed,
        };
    }
    let chosen = match kind {
        DiscKind::Dvd => dvd_main_title(root, streams, &metadata),
        DiscKind::BluRay => blu_ray_main_title(root, streams, &metadata),
    };
    match chosen {
        Ok(title) => DiscRead {
            title,
            streams: count,
            failed: false,
        },
        Err(err) => {
            warn!(disc = %disc.display(), error = %err, "could not read a disc structure's title information");
            DiscRead {
                title: Vec::new(),
                streams: count,
                failed: true,
            }
        }
    }
}

/// Whether `path`, a file of the disc rooted at `disc`, describes its
/// titles: a DVD title set's `VTS_nn_0.IFO`, or a Blu-ray's
/// `PLAYLIST/*.mpls`.
fn is_title_metadata(kind: DiscKind, disc: &Path, path: &Path) -> bool {
    let Some(name) = path.file_name().map(|name| name.to_string_lossy()) else {
        return false;
    };
    if name.starts_with('.') {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    match kind {
        DiscKind::Dvd => parent == disc && dvd_ifo_title_set(&name).is_some(),
        DiscKind::BluRay => {
            parent.parent() == Some(disc)
                && parent
                    .file_name()
                    .is_some_and(|dir| dir.eq_ignore_ascii_case("playlist"))
                && name.to_ascii_lowercase().ends_with(".mpls")
        }
    }
}

/// The title set a DVD's `VTS_nn_0.IFO` describes, in any case.
fn dvd_ifo_title_set(file_name: &str) -> Option<u32> {
    let upper = file_name.to_ascii_uppercase();
    let set = upper.strip_prefix("VTS_")?.strip_suffix("_0.IFO")?;
    if set.len() != 2 {
        return None;
    }
    set.parse::<u32>().ok().filter(|set| *set >= 1)
}

/// Read the file at `path` beneath `root` through the no-follow opener, up
/// to `cap` bytes: `None` when it is larger, or is no regular file of the
/// library. Any other failure is the caller's to report.
fn read_capped(root: &Path, path: &Path, cap: u64) -> std::io::Result<Option<Vec<u8>>> {
    let opened = match LibraryFile::open(root, path) {
        Ok(opened) => opened,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound || is_refusal(&err) => {
            return Ok(None);
        }
        Err(err) => return Err(err),
    };
    if opened.metadata().len() > cap {
        return Ok(None);
    }
    let (file, _, _) = opened.into_parts();
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// A DVD's main title: see the module documentation.
fn dvd_main_title(
    root: &Path,
    streams: Vec<Stream>,
    metadata: &[PathBuf],
) -> std::io::Result<Vec<PathBuf>> {
    // Each title set's files by part; only the run from part 1 up to the
    // first missing part is played, as a stack is (issue #233).
    let mut sets: BTreeMap<u32, BTreeMap<u32, Stream>> = BTreeMap::new();
    for stream in streams {
        let Some((set, part)) = stream
            .path
            .file_name()
            .and_then(|name| dvd_title_file(&name.to_string_lossy()))
        else {
            continue;
        };
        sets.entry(set).or_default().insert(part, stream);
    }
    let ifos: HashMap<u32, &PathBuf> = metadata
        .iter()
        .filter_map(|path| {
            let set = dvd_ifo_title_set(&path.file_name()?.to_string_lossy())?;
            Some((set, path))
        })
        .collect();

    struct Candidate {
        set: u32,
        files: Vec<PathBuf>,
        bytes: u64,
        duration: Option<Duration>,
    }
    let mut candidates = Vec::new();
    for (set, parts) in sets {
        let run: Vec<&Stream> = parts
            .iter()
            .enumerate()
            .take_while(|(at, (part, _))| u64::from(**part) == *at as u64 + 1)
            .map(|(_, (_, stream))| stream)
            .collect();
        if run.is_empty() {
            continue;
        }
        let duration = match ifos.get(&set) {
            Some(ifo) => read_capped(root, ifo, MAX_IFO_BYTES)?
                .as_deref()
                .and_then(dvd_title_set_duration),
            None => None,
        };
        candidates.push(Candidate {
            set,
            files: run.iter().map(|stream| stream.path.clone()).collect(),
            bytes: run.iter().map(|stream| stream.size).sum(),
            duration,
        });
    }
    let by_duration = candidates
        .iter()
        .all(|candidate| candidate.duration.is_some());
    if !by_duration {
        debug!("a DVD title set's IFO could not be read; choosing the main title by size");
    }
    let chosen = candidates.into_iter().max_by_key(|candidate| {
        (
            if by_duration {
                candidate.duration
            } else {
                None
            },
            candidate.bytes,
            Reverse(candidate.set),
        )
    });
    Ok(chosen.map(|candidate| candidate.files).unwrap_or_default())
}

/// The longest program chain a DVD title set's IFO (`VTS_nn_0.IFO`)
/// records: `None` when the bytes are no title set IFO, or record no
/// playback time.
///
/// The IFO starts `DVDVIDEO-VTS`; the 32-bit big-endian word at `0xCC` is the
/// sector of its program chain information table. That table starts with
/// the number of program chains (16 bits), and from byte 8 holds an 8-byte
/// entry per chain whose second word is the chain's offset from the table.
/// A chain's playback time is the four BCD bytes at its offset 4: hours,
/// minutes, seconds, and frames in the low six bits of the last, whose top
/// two bits give the frame rate (`01` 25 fps, `11` 30 fps).
pub(crate) fn dvd_title_set_duration(ifo: &[u8]) -> Option<Duration> {
    if ifo.get(..12)? != b"DVDVIDEO-VTS" {
        return None;
    }
    let sector = usize::try_from(be_u32(ifo, 0xCC)?).ok()?;
    let table = sector.checked_mul(DVD_SECTOR_BYTES)?;
    let chains = be_u16(ifo, table)?;
    (0..usize::from(chains))
        .filter_map(|chain| {
            let entry = table.checked_add(8 + 8 * chain)?;
            let offset = usize::try_from(be_u32(ifo, entry.checked_add(4)?)?).ok()?;
            let time = table.checked_add(offset)?.checked_add(4)?;
            playback_time(ifo.get(time..time.checked_add(4)?)?)
        })
        .max()
        .filter(|longest| !longest.is_zero())
}

/// A DVD playback time: four BCD bytes (see [`dvd_title_set_duration`]).
fn playback_time(bytes: &[u8]) -> Option<Duration> {
    let bcd = |byte: u8| -> Option<u64> {
        let (high, low) = (byte >> 4, byte & 0x0F);
        (high <= 9 && low <= 9).then_some(u64::from(high) * 10 + u64::from(low))
    };
    let [hours, minutes, seconds, frames] = bytes else {
        return None;
    };
    let (hours, minutes, seconds) = (bcd(*hours)?, bcd(*minutes)?, bcd(*seconds)?);
    if minutes > 59 || seconds > 59 {
        return None;
    }
    let fps = match frames >> 6 {
        0b01 => Some(25),
        0b11 => Some(30),
        _ => None,
    };
    let frame_millis = match fps {
        Some(fps) => bcd(frames & 0x3F)? * 1000 / fps,
        None => 0,
    };
    Some(
        Duration::from_secs(hours * 3600 + minutes * 60 + seconds)
            + Duration::from_millis(frame_millis),
    )
}

/// A Blu-ray playlist: the clips it plays, in order, and how long its play
/// items last together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Playlist {
    /// Each play item's clip, as its five-digit name (`00800`), in play
    /// order; a clip played twice is listed twice.
    pub clips: Vec<String>,
    pub duration: Duration,
}

/// What a Blu-ray playlist (`PLAYLIST/*.mpls`) plays: `None` when the bytes
/// are no playlist.
///
/// The file starts `MPLS`; the 32-bit big-endian word at byte 8 is where its
/// play list starts. There, after a 32-bit length and two reserved bytes,
/// come the number of play items (16 bits) and of sub-paths (16 bits), then
/// the play items. Each starts with its own 16-bit length (not counting
/// itself), then the clip's five-character name and four-character codec
/// identifier, two bytes of flags and one of STC id, then its in and out
/// times: 32-bit counts of a 45 kHz clock.
pub(crate) fn blu_ray_playlist(mpls: &[u8]) -> Option<Playlist> {
    if mpls.get(..4)? != b"MPLS" {
        return None;
    }
    let start = usize::try_from(be_u32(mpls, 8)?).ok()?;
    let items = be_u16(mpls, start.checked_add(6)?)?;
    let mut at = start.checked_add(10)?;
    let mut clips = Vec::with_capacity(usize::from(items));
    let mut ticks: u64 = 0;
    for _ in 0..items {
        let length = usize::from(be_u16(mpls, at)?);
        let name = mpls.get(at.checked_add(2)?..at.checked_add(7)?)?;
        if !name.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let in_time = be_u32(mpls, at.checked_add(14)?)?;
        let out_time = be_u32(mpls, at.checked_add(18)?)?;
        ticks = ticks.saturating_add(u64::from(out_time.saturating_sub(in_time)));
        clips.push(String::from_utf8_lossy(name).into_owned());
        at = at.checked_add(2)?.checked_add(length)?;
    }
    if clips.is_empty() {
        return None;
    }
    Some(Playlist {
        clips,
        duration: Duration::from_millis(ticks.saturating_mul(1000) / MPLS_TICKS_PER_SEC),
    })
}

/// A Blu-ray's main title: see the module documentation.
fn blu_ray_main_title(
    root: &Path,
    streams: Vec<Stream>,
    metadata: &[PathBuf],
) -> std::io::Result<Vec<PathBuf>> {
    // Each clip by its name, in any case: `00800.m2ts` and `00800.M2TS`.
    let by_clip: HashMap<String, &Stream> = streams
        .iter()
        .filter_map(|stream| {
            let stem = stream
                .path
                .file_stem()?
                .to_string_lossy()
                .to_ascii_uppercase();
            Some((stem, stream))
        })
        .collect();

    let mut best: Option<(Duration, u64, Reverse<&Path>, Vec<PathBuf>)> = None;
    for playlist_path in metadata {
        let Some(bytes) = read_capped(root, playlist_path, MAX_PLAYLIST_BYTES)? else {
            continue;
        };
        let Some(playlist) = blu_ray_playlist(&bytes) else {
            continue;
        };
        let mut files: Vec<PathBuf> = Vec::new();
        let mut bytes_played = 0u64;
        let mut complete = true;
        for clip in &playlist.clips {
            match by_clip.get(&clip.to_ascii_uppercase()) {
                Some(stream) => {
                    if !files.contains(&stream.path) {
                        bytes_played += stream.size;
                        files.push(stream.path.clone());
                    }
                }
                None => {
                    complete = false;
                    break;
                }
            }
        }
        if !complete {
            continue;
        }
        let key = (
            playlist.duration,
            bytes_played,
            Reverse(playlist_path.as_path()),
        );
        let better = best
            .as_ref()
            .is_none_or(|(duration, bytes, name, _)| key > (*duration, *bytes, *name));
        if better {
            best = Some((key.0, key.1, key.2, files));
        }
    }
    if let Some((_, _, _, files)) = best {
        return Ok(files);
    }
    debug!("no playlist of a Blu-ray could be read whole; taking its largest clip");
    Ok(streams
        .into_iter()
        .max_by_key(|stream| (stream.size, Reverse(stream.path.clone())))
        .map(|stream| vec![stream.path])
        .unwrap_or_default())
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(test)]
#[path = "disc_fixtures.rs"]
pub(crate) mod fixtures;

#[cfg(test)]
#[path = "disc_tests.rs"]
mod tests;
