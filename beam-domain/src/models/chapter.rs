//! A video file's chapters, and the intro and credits markers they name
//! (issue #265).
//!
//! Chapters are read from the container when the file is probed -- a
//! Matroska `Chapters` element, an MP4 chapter track -- and stored per file:
//! two files of one title can be cut differently, so a chapter is always on
//! one file's timeline.
//!
//! An intro or credits marker is a span a client offers to skip. Beam reads
//! them from the chapters a file carries ([`markers_from_chapters`]): a
//! chapter titled `Opening` or `Intro` is the intro, one titled `Ending` or
//! `End Credits` the credits. A file without such chapters has no markers.

/// One chapter of a file, on that file's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    /// The chapter's position among the file's chapters, from 0, in the
    /// order the container lists them.
    pub index: u32,
    /// Where the chapter starts, in milliseconds from the file's start.
    pub start_ms: u64,
    /// Where it ends, in milliseconds from the file's start; never before
    /// `start_ms`.
    pub end_ms: u64,
    /// The chapter's own title (`Opening`, `Chapter 3`); `None` when the
    /// container names none.
    pub title: Option<String>,
}

/// What a [`PlaybackMarker`] marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkerKind {
    /// The opening titles or theme, which a viewer skips into the episode.
    Intro,
    /// The closing credits, which a viewer skips to the next episode.
    Credits,
}

/// A span of a file a client offers to skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackMarker {
    pub kind: MarkerKind,
    /// Where the span starts, in milliseconds from the file's start.
    pub start_ms: u64,
    /// Where it ends; always after `start_ms`.
    pub end_ms: u64,
}

/// The chapter titles that name an intro, compared after
/// [`normalised_title`].
const INTRO_TITLES: &[&str] = &[
    "intro",
    "introduction",
    "opening",
    "opening credits",
    "opening song",
    "opening theme",
    "opening titles",
    "op",
    "title sequence",
    "main titles",
];

/// The chapter titles that name the credits, compared after
/// [`normalised_title`].
const CREDITS_TITLES: &[&str] = &[
    "credits",
    "end credits",
    "end titles",
    "ending",
    "ending credits",
    "ending song",
    "ending theme",
    "closing credits",
    "ed",
    "outro",
];

/// A title as the marker tables compare it: lowercase, its surrounding
/// whitespace and punctuation trimmed, and every run of whitespace, `-` or
/// `_` inside it one space -- so `End-Credits`, ` end credits ` and
/// `END_CREDITS` all read `end credits`.
fn normalised_title(title: &str) -> String {
    title
        .trim_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The kind of span a chapter titled `title` marks, if any.
pub fn marker_kind_of_title(title: &str) -> Option<MarkerKind> {
    let title = normalised_title(title);
    if INTRO_TITLES.contains(&title.as_str()) {
        Some(MarkerKind::Intro)
    } else if CREDITS_TITLES.contains(&title.as_str()) {
        Some(MarkerKind::Credits)
    } else {
        None
    }
}

/// The intro and credits markers `chapters` name: for each kind, the first
/// chapter -- in the file's order -- whose title names it and which lasts
/// longer than nothing. At most one marker of each kind, the intro first.
pub fn markers_from_chapters(chapters: &[Chapter]) -> Vec<PlaybackMarker> {
    [MarkerKind::Intro, MarkerKind::Credits]
        .into_iter()
        .filter_map(|kind| {
            chapters
                .iter()
                .filter(|chapter| chapter.end_ms > chapter.start_ms)
                .find(|chapter| {
                    chapter
                        .title
                        .as_deref()
                        .and_then(marker_kind_of_title)
                        .is_some_and(|found| found == kind)
                })
                .map(|chapter| PlaybackMarker {
                    kind,
                    start_ms: chapter.start_ms,
                    end_ms: chapter.end_ms,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(index: u32, start_ms: u64, end_ms: u64, title: Option<&str>) -> Chapter {
        Chapter {
            index,
            start_ms,
            end_ms,
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn titles_name_the_kind_of_span_they_mark_whatever_their_case_and_separators() {
        let cases: &[(&str, Option<MarkerKind>)] = &[
            ("Opening", Some(MarkerKind::Intro)),
            ("OP", Some(MarkerKind::Intro)),
            ("  intro  ", Some(MarkerKind::Intro)),
            ("Opening-Credits", Some(MarkerKind::Intro)),
            ("Title_Sequence", Some(MarkerKind::Intro)),
            ("End Credits", Some(MarkerKind::Credits)),
            ("END_CREDITS", Some(MarkerKind::Credits)),
            ("Ending.", Some(MarkerKind::Credits)),
            ("ED", Some(MarkerKind::Credits)),
            ("Outro", Some(MarkerKind::Credits)),
            ("Chapter 1", None),
            ("Prologue", None),
            ("Opening Scene of the Battle", None),
            ("Credits Roll Over the Finale", None),
            ("Preview", None),
            ("", None),
        ];
        for (title, expected) in cases {
            assert_eq!(marker_kind_of_title(title), *expected, "{title:?}");
        }
    }

    #[test]
    fn the_intro_and_credits_chapters_become_markers_on_the_files_timeline() {
        let chapters = [
            chapter(0, 0, 90_000, Some("Cold Open")),
            chapter(1, 90_000, 180_000, Some("Opening")),
            chapter(2, 180_000, 1_200_000, Some("Part A")),
            chapter(3, 1_200_000, 1_290_000, Some("Ending")),
            chapter(4, 1_290_000, 1_320_000, Some("Preview")),
        ];
        assert_eq!(
            markers_from_chapters(&chapters),
            vec![
                PlaybackMarker {
                    kind: MarkerKind::Intro,
                    start_ms: 90_000,
                    end_ms: 180_000,
                },
                PlaybackMarker {
                    kind: MarkerKind::Credits,
                    start_ms: 1_200_000,
                    end_ms: 1_290_000,
                },
            ]
        );
    }

    #[test]
    fn the_first_chapter_of_a_kind_wins_and_an_empty_one_is_passed_over() {
        let chapters = [
            chapter(0, 5_000, 5_000, Some("Intro")),
            chapter(1, 10_000, 40_000, Some("Intro")),
            chapter(2, 50_000, 80_000, Some("Opening")),
        ];
        assert_eq!(
            markers_from_chapters(&chapters),
            vec![PlaybackMarker {
                kind: MarkerKind::Intro,
                start_ms: 10_000,
                end_ms: 40_000,
            }]
        );
    }

    #[test]
    fn untitled_and_unrelated_chapters_mark_nothing() {
        let chapters = [
            chapter(0, 0, 60_000, None),
            chapter(1, 60_000, 120_000, Some("Chapter 2")),
        ];
        assert!(markers_from_chapters(&chapters).is_empty());
        assert!(markers_from_chapters(&[]).is_empty());
    }
}
