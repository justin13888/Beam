//! Minimal DVD and Blu-ray disc structures for tests (issue #234): the
//! bytes of a title set IFO and of a playlist, laid out as the parsers in
//! [`super`] document them, and folders of them on disc.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A title set IFO (`VTS_nn_0.IFO`) whose program chains last `chains`,
/// each written at 25 fps to the frame.
pub(crate) fn ifo(chains: &[Duration]) -> Vec<u8> {
    fn bcd(value: u64) -> u8 {
        u8::try_from((value / 10) << 4 | (value % 10)).expect("two decimal digits")
    }
    let table = 2048;
    let mut bytes = vec![0u8; table];
    bytes[..12].copy_from_slice(b"DVDVIDEO-VTS");
    bytes[0xCC..0xD0].copy_from_slice(&1u32.to_be_bytes());
    let count = u16::try_from(chains.len()).expect("few chains");
    let mut body = vec![0u8; 8 + 8 * chains.len()];
    body[..2].copy_from_slice(&count.to_be_bytes());
    for (at, duration) in chains.iter().enumerate() {
        let offset = u32::try_from(body.len()).expect("small table");
        body[8 + 8 * at + 4..8 + 8 * at + 8].copy_from_slice(&offset.to_be_bytes());
        let secs = duration.as_secs();
        let frames = u64::from(duration.subsec_millis()) * 25 / 1000;
        body.extend_from_slice(&[0, 0, 1, 1]);
        body.extend_from_slice(&[
            bcd(secs / 3600),
            bcd(secs / 60 % 60),
            bcd(secs % 60),
            0b0100_0000 | bcd(frames),
        ]);
    }
    bytes.extend_from_slice(&body);
    bytes
}

/// A playlist (`*.mpls`) whose play items play `items`: each a clip's
/// five-digit name, and its in and out times on the 45 kHz clock.
pub(crate) fn mpls(items: &[(&str, u32, u32)]) -> Vec<u8> {
    let start = 40u32;
    let mut bytes = b"MPLS0200".to_vec();
    bytes.extend_from_slice(&start.to_be_bytes());
    bytes.resize(start as usize, 0);
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&u16::try_from(items.len()).expect("few items").to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    for (clip, in_time, out_time) in items {
        assert_eq!(clip.len(), 5, "a clip is named by five digits");
        bytes.extend_from_slice(&20u16.to_be_bytes());
        bytes.extend_from_slice(clip.as_bytes());
        bytes.extend_from_slice(b"M2TS");
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&in_time.to_be_bytes());
        bytes.extend_from_slice(&out_time.to_be_bytes());
    }
    bytes
}

/// `secs` on a playlist's 45 kHz clock.
pub(crate) fn ticks(secs: u32) -> u32 {
    secs * 45_000
}

/// Write `size` bytes at `path`, each spelling where it is, so no two files
/// of a fixture hash alike.
pub(crate) fn write_sized(path: &Path, size: usize) {
    std::fs::create_dir_all(path.parent().expect("a file has a folder")).unwrap();
    let seed = path.to_string_lossy();
    let bytes: Vec<u8> = seed.bytes().cycle().take(size.max(1)).collect();
    std::fs::write(path, bytes).unwrap();
}

/// One title set of a DVD: its number, the size of each of its parts from
/// part 1, and how long its IFO says its longest chain lasts (no IFO at all
/// when `None`).
pub(crate) struct TitleSet<'a> {
    pub set: u32,
    pub parts: &'a [usize],
    pub duration: Option<Duration>,
}

/// A DVD's `VIDEO_TS/` folder in `folder`, with the disc's own files and each
/// title set's menu beside its title sets. Returns the `VIDEO_TS/` folder.
pub(crate) fn write_dvd(folder: &Path, sets: &[TitleSet<'_>]) -> PathBuf {
    let disc = folder.join("VIDEO_TS");
    write_sized(&disc.join("VIDEO_TS.IFO"), 64);
    write_sized(&disc.join("VIDEO_TS.VOB"), 512);
    for TitleSet {
        set,
        parts,
        duration,
    } in sets
    {
        write_sized(&disc.join(format!("VTS_{set:02}_0.VOB")), 256);
        if let Some(duration) = duration {
            std::fs::write(disc.join(format!("VTS_{set:02}_0.IFO")), ifo(&[*duration])).unwrap();
        }
        for (at, size) in parts.iter().enumerate() {
            write_sized(&disc.join(format!("VTS_{set:02}_{}.VOB", at + 1)), *size);
        }
    }
    disc
}

/// A Blu-ray's `BDMV/` folder in `folder`: each clip in `STREAM/` at its
/// size, and each playlist in `PLAYLIST/`. Returns the `BDMV/` folder.
pub(crate) fn write_blu_ray(
    folder: &Path,
    clips: &[(&str, usize)],
    playlists: &[(&str, Vec<u8>)],
) -> PathBuf {
    let disc = folder.join("BDMV");
    write_sized(&disc.join("index.bdmv"), 64);
    write_sized(&disc.join("CLIPINF").join("00000.clpi"), 64);
    for (clip, size) in clips {
        write_sized(&disc.join("STREAM").join(format!("{clip}.m2ts")), *size);
    }
    for (name, bytes) in playlists {
        let path = disc.join("PLAYLIST").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    disc
}
