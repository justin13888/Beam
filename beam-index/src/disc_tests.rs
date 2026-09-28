//! Reading a disc structure for its main title (issue #234), over real
//! folders of minimal IFOs, playlists and stream files.

use super::fixtures::{TitleSet, mpls, ticks, write_blu_ray, write_dvd, write_sized};
use super::*;
use proptest::prelude::*;
use tempfile::TempDir;

const MINUTE: Duration = Duration::from_secs(60);

fn read(root: &Path, disc: &Path, kind: DiscKind) -> DiscRead {
    read_disc(root, disc, kind, &PathPolicy::default())
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn a_dvd_plays_the_title_set_its_ifo_says_lasts_longest() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // The film is set 2; set 1, an extra, is larger (a higher bit rate)
    // but shorter, so bytes alone would pick the wrong one.
    let disc = write_dvd(
        &root.join("Heat (1995)"),
        &[
            TitleSet {
                set: 1,
                parts: &[4000, 4000],
                duration: Some(10 * MINUTE),
            },
            TitleSet {
                set: 2,
                parts: &[1000, 1000, 500],
                duration: Some(170 * MINUTE),
            },
        ],
    );

    let found = read(root, &disc, DiscKind::Dvd);

    assert_eq!(
        names(&found.title),
        ["VTS_02_1.VOB", "VTS_02_2.VOB", "VTS_02_3.VOB"]
    );
    assert_eq!(
        found.streams, 5,
        "every title file is a stream, played or not"
    );
    assert!(!found.failed);
}

#[test]
fn a_dvd_whose_ifos_cannot_all_be_read_plays_its_largest_title_set() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let disc = write_dvd(
        &root.join("Heat (1995)"),
        &[
            TitleSet {
                set: 1,
                parts: &[4000, 4000],
                duration: Some(10 * MINUTE),
            },
            TitleSet {
                set: 2,
                parts: &[1000, 1000],
                duration: None,
            },
        ],
    );
    // A second title set with an IFO that is no IFO.
    write_sized(&disc.join("VTS_03_1.VOB"), 100);
    std::fs::write(disc.join("VTS_03_0.IFO"), b"not an ifo").unwrap();

    let found = read(root, &disc, DiscKind::Dvd);

    assert_eq!(names(&found.title), ["VTS_01_1.VOB", "VTS_01_2.VOB"]);
}

#[test]
fn a_dvd_plays_a_title_set_only_up_to_its_first_missing_part() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let disc = write_dvd(
        &root.join("Heat (1995)"),
        &[TitleSet {
            set: 1,
            parts: &[1000, 1000, 1000],
            duration: Some(100 * MINUTE),
        }],
    );
    std::fs::remove_file(disc.join("VTS_01_2.VOB")).unwrap();

    let found = read(root, &disc, DiscKind::Dvd);

    assert_eq!(names(&found.title), ["VTS_01_1.VOB"]);
}

#[test]
fn a_blu_ray_plays_its_longest_playlist_in_order_each_clip_once() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let disc = write_blu_ray(
        &root.join("Heat (1995)"),
        &[("00001", 3000), ("00002", 2000), ("00003", 9000)],
        &[
            // A trailer on the largest clip.
            ("00000.mpls", mpls(&[("00003", 0, ticks(120))])),
            // The film: clip 2, then clip 1, then clip 2 again.
            (
                "00800.mpls",
                mpls(&[
                    ("00002", 0, ticks(3000)),
                    ("00001", ticks(10), ticks(3010)),
                    ("00002", ticks(3000), ticks(4000)),
                ]),
            ),
        ],
    );

    let found = read(root, &disc, DiscKind::BluRay);

    assert_eq!(names(&found.title), ["00002.m2ts", "00001.m2ts"]);
    assert_eq!(found.streams, 3);
}

#[test]
fn a_blu_ray_passes_over_a_playlist_whose_clips_are_not_all_there() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let disc = write_blu_ray(
        &root.join("Heat (1995)"),
        &[("00001", 3000), ("00002", 2000)],
        &[
            ("00001.mpls", mpls(&[("00001", 0, ticks(600))])),
            (
                "00002.mpls",
                mpls(&[("00002", 0, ticks(6000)), ("00009", 0, ticks(60))]),
            ),
        ],
    );

    let found = read(root, &disc, DiscKind::BluRay);

    assert_eq!(names(&found.title), ["00001.m2ts"]);
}

#[test]
fn a_blu_ray_with_no_readable_playlist_plays_its_largest_clip() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let disc = write_blu_ray(
        &root.join("Heat (1995)"),
        &[("00001", 3000), ("00002", 9000), ("00003", 2000)],
        &[("00001.mpls", b"MPLS but truncated".to_vec())],
    );

    let found = read(root, &disc, DiscKind::BluRay);

    assert_eq!(names(&found.title), ["00002.m2ts"]);
}

#[test]
fn an_empty_disc_plays_nothing() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let disc = write_dvd(&root.join("Heat (1995)"), &[]);

    let found = read(root, &disc, DiscKind::Dvd);

    assert_eq!(found, DiscRead::default());
}

/// A stream file an administrator ignored is not played, though it is the
/// largest; it still shows the disc is there.
#[test]
fn an_ignored_stream_file_is_not_played() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let disc = write_blu_ray(
        &root.join("Heat (1995)"),
        &[("00001", 3000), ("00002", 9000)],
        &[],
    );
    let policy = PathPolicy::new(["**/00002.m2ts"]).unwrap();

    let found = read_disc(root, &disc, DiscKind::BluRay, &policy);

    assert_eq!(names(&found.title), ["00001.m2ts"]);
    assert_eq!(found.streams, 2);
}

/// Nothing is read through a link: a stream file or a playlist that is one
/// is no file of the disc, and a disc root that is one is no disc.
#[cfg(unix)]
#[test]
fn a_disc_follows_no_link() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("library");
    let outside = dir.path().join("outside");
    let elsewhere = write_blu_ray(
        &outside,
        &[("00001", 9000)],
        &[("00001.mpls", mpls(&[("00001", 0, ticks(9000))]))],
    );
    let disc = write_blu_ray(
        &root.join("Heat (1995)"),
        &[("00002", 1000), ("00003", 2000)],
        &[("00002.mpls", mpls(&[("00002", 0, ticks(600))]))],
    );
    std::os::unix::fs::symlink(
        elsewhere.join("STREAM/00001.m2ts"),
        disc.join("STREAM/00001.m2ts"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        elsewhere.join("PLAYLIST/00001.mpls"),
        disc.join("PLAYLIST/00001.mpls"),
    )
    .unwrap();

    let found = read(&root, &disc, DiscKind::BluRay);
    assert_eq!(names(&found.title), ["00002.m2ts"]);
    assert_eq!(found.streams, 2);

    let linked = root.join("Ronin (1998)");
    std::fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink(&elsewhere, linked.join("BDMV")).unwrap();
    assert_eq!(
        read(&root, &linked.join("BDMV"), DiscKind::BluRay),
        DiscRead::default()
    );
}

#[test]
fn a_file_is_a_part_only_of_a_title_of_several_files() {
    let title = [
        PathBuf::from("/d/VTS_01_1.VOB"),
        PathBuf::from("/d/VTS_01_2.VOB"),
    ];
    let cases: [(&[PathBuf], &str, Option<u32>); 4] = [
        (&title, "/d/VTS_01_1.VOB", Some(1)),
        (&title, "/d/VTS_01_2.VOB", Some(2)),
        (&title, "/d/VTS_02_1.VOB", None),
        (&title[..1], "/d/VTS_01_1.VOB", None),
    ];
    for (title, path, expected) in cases {
        assert_eq!(part_in(title, Path::new(path)), expected, "{path}");
    }
}

#[test]
fn a_playback_time_is_read_to_the_frame() {
    let cases: [([u8; 4], Option<Duration>); 6] = [
        // 1:23:45 and 12 frames at 25 fps.
        (
            [0x01, 0x23, 0x45, 0x52],
            Some(Duration::from_millis(5_025_480)),
        ),
        // 15 frames at 30 fps.
        ([0x00, 0x00, 0x01, 0xD5], Some(Duration::from_millis(1_500))),
        // No frame rate: the frames are not counted.
        ([0x02, 0x00, 0x00, 0x12], Some(Duration::from_secs(7200))),
        ([0x0A, 0x00, 0x00, 0x40], None),
        ([0x00, 0x60, 0x00, 0x40], None),
        ([0x00, 0x00, 0x60, 0x40], None),
    ];
    for (bytes, expected) in cases {
        assert_eq!(playback_time(&bytes), expected, "{bytes:02x?}");
    }
}

proptest! {
    /// What a fixture writes, the parser reads: an IFO's longest chain, and
    /// a playlist's clips and total.
    #[test]
    fn a_written_ifo_and_playlist_read_back(
        chains in prop::collection::vec(0u64..(99 * 3600), 1..6),
        items in prop::collection::vec((0u32..100_000, 0u32..20_000, 0u32..20_000), 1..8),
    ) {
        let chains: Vec<Duration> = chains.into_iter().map(Duration::from_secs).collect();
        let longest = chains.iter().max().copied().filter(|d| !d.is_zero());
        prop_assert_eq!(dvd_title_set_duration(&super::fixtures::ifo(&chains)), longest);

        let named: Vec<(String, u32, u32)> = items
            .iter()
            .map(|(clip, a, b)| (format!("{clip:05}"), ticks(*a.min(b)), ticks(*a.max(b))))
            .collect();
        let borrowed: Vec<(&str, u32, u32)> =
            named.iter().map(|(clip, a, b)| (clip.as_str(), *a, *b)).collect();
        let playlist = blu_ray_playlist(&mpls(&borrowed)).unwrap();
        let secs: u64 = named.iter().map(|(_, a, b)| u64::from((b - a) / 45_000)).sum();
        prop_assert_eq!(playlist.duration, Duration::from_secs(secs));
        prop_assert_eq!(
            playlist.clips,
            named.into_iter().map(|(clip, _, _)| clip).collect::<Vec<_>>()
        );
    }

    /// An IFO whose pointers point anywhere -- inside it, at its end, past
    /// it, at the largest values their widths hold -- is read without a
    /// panic, and any duration it yields is one four BCD bytes can spell.
    #[test]
    fn an_ifo_with_wild_pointers_reads_safely(
        sector in prop_oneof![Just(0u32), Just(1), Just(2), 0u32..8, Just(u32::MAX)],
        chains in prop_oneof![Just(0u16), 1u16..8, Just(u16::MAX)],
        offsets in prop::collection::vec(
            prop_oneof![0u32..64, 2040u32..2100, 4000u32..5000, Just(u32::MAX)],
            0..8,
        ),
        len in prop_oneof![0usize..0xD0, 0xD0usize..2100, 2100usize..6000],
        filler in any::<u8>(),
    ) {
        let mut ifo = vec![filler; len.max(12)];
        ifo[..12].copy_from_slice(b"DVDVIDEO-VTS");
        let mut put = |at: usize, bytes: &[u8]| {
            if let Some(slot) = ifo.get_mut(at..at + bytes.len()) {
                slot.copy_from_slice(bytes);
            }
        };
        put(0xCC, &sector.to_be_bytes());
        let table = sector as usize * DVD_SECTOR_BYTES;
        put(table, &chains.to_be_bytes());
        for (at, offset) in offsets.iter().enumerate() {
            put(table + 8 + 8 * at + 4, &offset.to_be_bytes());
        }
        if let Some(longest) = dvd_title_set_duration(&ifo) {
            prop_assert!(longest < Duration::from_secs(100 * 3600));
            prop_assert!(!longest.is_zero());
        }
    }

    /// A playlist whose play list starts, counts and lengths point anywhere
    /// is read without a panic, and whatever it yields names each play item
    /// it read by five digits, never more items than it counts.
    #[test]
    fn a_playlist_with_wild_pointers_reads_safely(
        start in prop_oneof![0u32..16, 16u32..200, 200u32..400, Just(u32::MAX)],
        items in prop_oneof![Just(0u16), 1u16..6, Just(u16::MAX)],
        lengths in prop::collection::vec(prop_oneof![Just(0u16), 18u16..24, Just(u16::MAX)], 0..6),
        len in prop_oneof![0usize..16, 16usize..200, 200usize..600],
        digits in any::<bool>(),
    ) {
        let mut playlist = vec![if digits { b'7' } else { 0 }; len.max(4)];
        playlist[..4].copy_from_slice(b"MPLS");
        let mut put = |at: usize, bytes: &[u8]| {
            if let Some(slot) = playlist.get_mut(at..at + bytes.len()) {
                slot.copy_from_slice(bytes);
            }
        };
        put(8, &start.to_be_bytes());
        let list = start as usize;
        put(list.saturating_add(6), &items.to_be_bytes());
        let mut at = list.saturating_add(10);
        for length in &lengths {
            put(at, &length.to_be_bytes());
            at = at.saturating_add(2 + usize::from(*length));
        }
        if let Some(read) = blu_ray_playlist(&playlist) {
            prop_assert!(!read.clips.is_empty());
            prop_assert!(read.clips.len() <= usize::from(items));
            for clip in &read.clips {
                prop_assert!(clip.len() == 5 && clip.bytes().all(|b| b.is_ascii_digit()));
            }
        }
    }
}

/// Only the folders the choice needs are listed: a Blu-ray folder that
/// cannot be read and that the main title does not need (`BACKUP/`, `JAR/`)
/// changes nothing, while an unreadable `PLAYLIST/` or `VIDEO_TS/` fails the
/// disc, so its rows are left as they are.
#[cfg(unix)]
#[test]
fn only_a_folder_the_main_title_needs_can_fail_a_disc() {
    use std::os::unix::fs::PermissionsExt;
    let lock = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let blu_ray = write_blu_ray(
        &root.join("Heat (1995)"),
        &[("00001", 1000), ("00800", 4000)],
        &[("00800.mpls", mpls(&[("00800", 0, ticks(6000))]))],
    );
    for companion in ["BACKUP", "JAR", "AUXDATA"] {
        write_sized(&blu_ray.join(companion).join("00800.mpls"), 64);
        lock(&blu_ray.join(companion), 0o000);
    }
    if std::fs::read_dir(blu_ray.join("BACKUP")).is_ok() {
        for companion in ["BACKUP", "JAR", "AUXDATA"] {
            lock(&blu_ray.join(companion), 0o755);
        }
        eprintln!("skipped: running as root, which ignores file permissions");
        return;
    }

    let found = read(root, &blu_ray, DiscKind::BluRay);
    assert_eq!(names(&found.title), ["00800.m2ts"]);
    assert!(
        !found.failed,
        "an unreadable BACKUP, JAR or AUXDATA is never read"
    );

    lock(&blu_ray.join("PLAYLIST"), 0o000);
    let found = read(root, &blu_ray, DiscKind::BluRay);
    lock(&blu_ray.join("PLAYLIST"), 0o755);
    assert!(found.failed, "the playlists decide the main title");
    assert!(found.title.is_empty());

    let dvd = write_dvd(
        &root.join("Ronin (1998)"),
        &[TitleSet {
            set: 1,
            parts: &[1000],
            duration: Some(100 * MINUTE),
        }],
    );
    lock(&dvd, 0o000);
    let found = read(root, &dvd, DiscKind::Dvd);
    lock(&dvd, 0o755);
    assert!(found.failed);
    for companion in ["BACKUP", "JAR", "AUXDATA"] {
        lock(&blu_ray.join(companion), 0o755);
    }
}
