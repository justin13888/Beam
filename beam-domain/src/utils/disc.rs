//! DVD and Blu-ray disc structures, as far as a path can tell them (issue
//! #234): which folder roots one ([`DiscKind`]), which of its files are
//! stream files a main title may play ([`disc_stream_kind`]), and where a
//! disc sits in a set of discs of one film ([`disc_set_place`]).
//!
//! Pure: nothing is read from disk. Which stream files a disc's main title
//! actually plays is read from the disc itself, by the indexer.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::utils::filename::part_token;

/// A DVD or Blu-ray disc structure copied whole: a `VIDEO_TS/` or `BDMV/`
/// folder (issue #234).
///
/// What is inside one is not a set of titles -- a DVD splits one film into
/// `VTS_01_1.VOB`, `VTS_01_2.VOB`, ...; a Blu-ray's `BDMV/STREAM/` holds
/// `00001.m2ts` and friends -- so no file in it is judged by its own name,
/// which would invent films named `VTS 01 1` or `00001` and merge every
/// disc's same-numbered file into one. The disc is one source of the title
/// its enclosing folder names, and which of its stream files that source
/// plays -- its main title -- is read from the disc itself, which a path
/// cannot say. So the policy only marks a disc's candidate stream files
/// ([`crate::utils::path_policy::PathDisposition::DiscStream`]) and keeps
/// the rest of it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscKind {
    /// A `VIDEO_TS/` folder: its main title set's `VTS_nn_1.VOB`,
    /// `VTS_nn_2.VOB`, ... play in turn.
    Dvd,
    /// A `BDMV/` folder: the clips in `BDMV/STREAM/` its main playlist plays.
    BluRay,
}

impl DiscKind {
    /// The kind of disc structure a folder named `name` is the root of, in
    /// any case.
    pub fn of_folder(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("video_ts") {
            Some(DiscKind::Dvd)
        } else if name.eq_ignore_ascii_case("bdmv") {
            Some(DiscKind::BluRay)
        } else {
            None
        }
    }
}

/// The title set and part a DVD title file is named for, in any case:
/// `VTS_03_2.VOB` is part 2 of title set 3. Only a title set's content
/// files (parts 1 to 9) are named; its menu (`VTS_03_0.VOB`) and the disc's
/// own (`VIDEO_TS.VOB`) are not, and nor is a name of any other shape, such
/// as an AppleDouble `._VTS_01_1.VOB`.
pub fn dvd_title_file(file_name: &str) -> Option<(u32, u32)> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("vob") {
        return None;
    }
    let prefix = stem.get(..4)?;
    if !prefix.eq_ignore_ascii_case("vts_") {
        return None;
    }
    let (set, part) = stem[4..].split_once('_')?;
    if set.len() != 2 || part.len() != 1 {
        return None;
    }
    let set = set.parse::<u32>().ok().filter(|set| *set >= 1)?;
    let part = part.parse::<u32>().ok().filter(|part| *part >= 1)?;
    Some((set, part))
}

/// The kind of disc a stream file at `path` belongs to, judged from where
/// it sits alone: a DVD title file ([`dvd_title_file`]) directly in a
/// `VIDEO_TS/` folder, or an `.m2ts` clip directly in a `BDMV/STREAM/`
/// folder. `path` may be relative to a library root or absolute.
pub fn disc_stream_kind(path: &Path) -> Option<DiscKind> {
    let file_name = path.file_name()?.to_string_lossy();
    let parent = path.parent()?;
    let parent_name = parent.file_name()?.to_string_lossy();
    if DiscKind::of_folder(&parent_name) == Some(DiscKind::Dvd) {
        return dvd_title_file(&file_name).map(|_| DiscKind::Dvd);
    }
    let disc_folder = parent.parent()?.file_name()?.to_string_lossy();
    let is_clip = parent_name.eq_ignore_ascii_case("stream")
        && DiscKind::of_folder(&disc_folder) == Some(DiscKind::BluRay)
        && !file_name.starts_with('.')
        && Path::new(&*file_name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("m2ts"));
    is_clip.then_some(DiscKind::BluRay)
}

/// A folder named for nothing but one disc of a set: `Disc 1`, `DISC1`,
/// `CD2`, `Disk_2`.
static BARE_DISC_FOLDER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:cd|disc|disk)[ ._-]?(\d{1,2})$").expect("valid regex"));

/// The disc a folder's name is nothing but: `Disc 2` is disc 2.
fn bare_disc_number(name: &str) -> Option<u32> {
    let caps = BARE_DISC_FOLDER_REGEX.captures(name.trim())?;
    caps[1].parse().ok().filter(|disc| *disc >= 1)
}

/// Where a disc folder sits in a set of discs of one film (decision
/// D234-7): which disc of the set it is, and what every disc of the set
/// shares -- the folder they are in, and what their names say besides the
/// disc number. `Heat (1995)/Disc 2` is disc 2 of the set of bare `Disc N`
/// folders in `Heat (1995)`; `Movies/Heat (1995) - CD2` is disc 2 of the set
/// of `Heat (1995) - CDn` folders in `Movies`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DiscSetPlace {
    /// The folder the set's disc folders are in, as the path given was
    /// (relative to a library root, or absolute).
    pub container: PathBuf,
    /// What the disc folder's name says besides its disc number, folded so
    /// that `Heat (1995) - Disc 1` and `Heat.1995.DISC2` agree: empty for a
    /// bare `Disc N`.
    pub name: String,
    /// Which disc of the set it is, from 1.
    pub disc: u32,
}

/// Where `folder` -- a folder enclosing a disc structure's root, relative to
/// a library root or absolute -- sits in a set of discs, if its name numbers
/// a disc: a bare `Disc 2`, `DISC2`, `CD2` or `Disk 2`, or a film's name with
/// a disc token (`Heat (1995) - Disc 2`, `Heat.1995.CD2`) as a filename's
/// part token is read ([`crate::utils::filename::parse_media_filename`]).
pub fn disc_set_member(folder: &Path) -> Option<DiscSetPlace> {
    let name = folder.file_name()?.to_string_lossy();
    let container = folder.parent()?.to_path_buf();
    if let Some(disc) = bare_disc_number(&name) {
        return Some(DiscSetPlace {
            container,
            name: String::new(),
            disc,
        });
    }
    let token = part_token(&name).filter(|token| token.confirmed)?;
    // Every run of letters and digits, lowercased: separators, brackets and
    // dashes differ between the discs of one set as often as not.
    let folded: Vec<String> = token
        .stripped
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    (!folded.is_empty()).then(|| DiscSetPlace {
        container,
        name: folded.join(" "),
        disc: token.number,
    })
}

/// Where the disc structure rooted at `disc_root` (a `VIDEO_TS/` or `BDMV/`
/// folder) sits in a set of discs: [`disc_set_member`] of the folder
/// enclosing it.
pub fn disc_set_place(disc_root: &Path) -> Option<DiscSetPlace> {
    disc_set_member(disc_root.parent()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dvd_title_file_names_its_title_set_and_part() {
        let cases: [(&str, Option<(u32, u32)>); 12] = [
            ("VTS_01_1.VOB", Some((1, 1))),
            ("vts_12_9.vob", Some((12, 9))),
            ("VTS_99_2.Vob", Some((99, 2))),
            // A menu, the disc's own files and the title set's IFO.
            ("VTS_01_0.VOB", None),
            ("VIDEO_TS.VOB", None),
            ("VTS_01_0.IFO", None),
            ("VTS_00_1.VOB", None),
            ("VTS_1_1.VOB", None),
            ("VTS_01_10.VOB", None),
            ("VTS_01_1.mkv", None),
            ("._VTS_01_1.VOB", None),
            ("VTS-01-1.VOB", None),
        ];
        for (name, expected) in cases {
            assert_eq!(dvd_title_file(name), expected, "{name}");
        }
    }

    /// Where a disc folder sits in a set of discs (decision D234-7): its disc
    /// number, and what the discs of its set share -- the folder they are in and
    /// what their names say besides the number.
    #[test]
    fn a_disc_folders_place_in_its_set() {
        let place = |container: &str, name: &str, disc: u32| {
            Some(DiscSetPlace {
                container: PathBuf::from(container),
                name: name.to_string(),
                disc,
            })
        };
        let cases = [
            ("Heat (1995)/Disc 1", place("Heat (1995)", "", 1)),
            ("Heat (1995)/DISC2", place("Heat (1995)", "", 2)),
            ("Heat (1995)/cd_3", place("Heat (1995)", "", 3)),
            ("Heat (1995)/Disk 12", place("Heat (1995)", "", 12)),
            (
                "Movies/Heat (1995) - Disc 2",
                place("Movies", "heat 1995", 2),
            ),
            ("Movies/Heat.1995.DISC1", place("Movies", "heat 1995", 1)),
            ("Movies/Heat (1995) CD1", place("Movies", "heat 1995", 1)),
            // `part` numbers a piece of a film only after its year (D233-2).
            (
                "Movies/Heat (1995) - Part 2",
                place("Movies", "heat 1995", 2),
            ),
            ("Movies/Che Part 2", None),
            // Not a disc of a set.
            ("Heat (1995)", None),
            ("Heat (1995)/DVD9", None),
            ("Heat (1995)/Disc 0", None),
            ("Heat (1995)/Disc", None),
            ("Heat (1995)/Discovery", None),
            ("Disc 1", place("", "", 1)),
        ];
        for (folder, expected) in cases {
            assert_eq!(disc_set_member(Path::new(folder)), expected, "{folder}");
        }
        assert_eq!(
            disc_set_place(Path::new("Heat (1995)/Disc 2/VIDEO_TS")),
            place("Heat (1995)", "", 2)
        );
    }
}
