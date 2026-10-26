//! A video file's chapters, and the intro and credits markers they name
//! (issue #265).
//!
//! Chapters are read from the container when the file is probed -- a
//! Matroska `Chapters` element, an MP4 chapter track -- and stored per file:
//! two files of one title can be cut differently, so a chapter is always on
//! one file's timeline.
//!
//! A marker is a span a client offers to skip: the intro, the credits, a
//! recap of earlier episodes, a preview of the next. Beam reads them from the
//! chapters a file carries ([`markers_from_chapters`]): a chapter titled
//! `Opening` or `Intro` is the intro, one titled `Ending` or `End Credits`
//! the credits, `Recap` or `Previously` a recap, `Preview` or `Next Episode`
//! a preview. A file without such chapters has no markers.

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
    /// A recap of earlier episodes (`Previously on...`).
    Recap,
    /// The opening titles or theme, which a viewer skips into the episode.
    Intro,
    /// The closing credits, which a viewer skips to the next episode.
    Credits,
    /// A preview of the next episode, after the credits.
    Preview,
}

impl MarkerKind {
    /// Every kind, in the order a file usually plays them.
    pub const ALL: [MarkerKind; 4] = [
        MarkerKind::Recap,
        MarkerKind::Intro,
        MarkerKind::Credits,
        MarkerKind::Preview,
    ];

    /// The chapter titles that name this kind, compared after
    /// [`normalised_title`].
    fn titles(self) -> &'static [&'static str] {
        match self {
            MarkerKind::Recap => RECAP_TITLES,
            MarkerKind::Intro => INTRO_TITLES,
            MarkerKind::Credits => CREDITS_TITLES,
            MarkerKind::Preview => PREVIEW_TITLES,
        }
    }
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

/// The chapter titles that name a recap, compared after
/// [`normalised_title`].
const RECAP_TITLES: &[&str] = &["recap", "previously", "previously on", "last time"];

/// The chapter titles that name a preview, compared after
/// [`normalised_title`].
const PREVIEW_TITLES: &[&str] = &[
    "preview",
    "next episode",
    "next episode preview",
    "next time",
    "next time on",
];

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
    MarkerKind::ALL
        .into_iter()
        .find(|kind| kind.titles().contains(&title.as_str()))
}

/// The markers `chapters` name: for each kind, the first chapter -- in the
/// file's order -- whose title names it and which lasts longer than nothing.
/// At most one marker of each kind, in the order they start.
pub fn markers_from_chapters(chapters: &[Chapter]) -> Vec<PlaybackMarker> {
    let mut markers: Vec<PlaybackMarker> = MarkerKind::ALL
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
        .collect();
    markers.sort_by_key(|marker| (marker.start_ms, marker.end_ms));
    markers
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
            ("Previously On", Some(MarkerKind::Recap)),
            ("Recap", Some(MarkerKind::Recap)),
            ("Next Episode Preview", Some(MarkerKind::Preview)),
            ("Preview", Some(MarkerKind::Preview)),
            ("Previews of Coming Attractions", None),
            ("", None),
        ];
        for (title, expected) in cases {
            assert_eq!(marker_kind_of_title(title), *expected, "{title:?}");
        }
    }

    #[test]
    fn the_chapters_titled_for_a_span_become_markers_in_the_order_they_start() {
        let chapters = [
            chapter(0, 0, 30_000, Some("Previously On")),
            chapter(1, 30_000, 90_000, Some("Cold Open")),
            chapter(2, 90_000, 180_000, Some("Opening")),
            chapter(3, 180_000, 1_200_000, Some("Part A")),
            chapter(4, 1_200_000, 1_290_000, Some("Ending")),
            chapter(5, 1_290_000, 1_320_000, Some("Preview")),
        ];
        let marker = |kind, start_ms, end_ms| PlaybackMarker {
            kind,
            start_ms,
            end_ms,
        };
        assert_eq!(
            markers_from_chapters(&chapters),
            vec![
                marker(MarkerKind::Recap, 0, 30_000),
                marker(MarkerKind::Intro, 90_000, 180_000),
                marker(MarkerKind::Credits, 1_200_000, 1_290_000),
                marker(MarkerKind::Preview, 1_290_000, 1_320_000),
            ]
        );
    }

    #[test]
    fn markers_follow_the_files_order_not_the_usual_one() {
        // A preview before the intro, as some broadcasts air it.
        let chapters = [
            chapter(0, 0, 20_000, Some("Next Time")),
            chapter(1, 20_000, 60_000, Some("OP")),
        ];
        assert_eq!(
            markers_from_chapters(&chapters)
                .iter()
                .map(|marker| marker.kind)
                .collect::<Vec<_>>(),
            vec![MarkerKind::Preview, MarkerKind::Intro]
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
