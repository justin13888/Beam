//! Which video a subtitle file beside it belongs to, and what its name says
//! (issue #184).
//!
//! The convention Plex, Jellyfin, Kodi and Emby share: a subtitle is named
//! after its video's filename stem, then any number of dot-separated tokens,
//! then its extension -- `Movie (1999).en.forced.srt`,
//! `Movie (1999).English.SDH.srt`, `Movie (1999).Commentary.en.srt`. Tokens
//! name a language, a flag (`forced`, `sdh`, `cc`, `default`), or otherwise
//! are the track's title. A `Subs/` or `Subtitles/` folder beside the video
//! holds subtitles too, named the same way or -- when the folder above holds
//! only one video -- named for nothing but their language (`Subs/English.srt`).
//!
//! Pure: the caller lists the candidate videos.

use std::path::Path;

use crate::models::sidecar::{SidecarInfo, SubtitleFormat};

/// Languages a subtitle name may spell, as `(ISO 639-1, ISO 639-2/B,
/// ISO 639-2/T, English names)`. A name is matched in any of these spellings
/// and stored as the 639-2/B code.
///
/// Source: the Library of Congress ISO 639-2 registration authority's code
/// list, <https://www.loc.gov/standards/iso639-2/php/code_list.php>. The
/// languages are the ones subtitle releases are commonly published in; a
/// language outside the list is kept as part of the title rather than guessed.
const LANGUAGES: &[(&str, &str, &str, &[&str])] = &[
    ("ar", "ara", "ara", &["arabic"]),
    ("bg", "bul", "bul", &["bulgarian"]),
    ("bn", "ben", "ben", &["bengali"]),
    ("bs", "bos", "bos", &["bosnian"]),
    ("ca", "cat", "cat", &["catalan"]),
    ("cs", "cze", "ces", &["czech"]),
    ("cy", "wel", "cym", &["welsh"]),
    ("da", "dan", "dan", &["danish"]),
    ("de", "ger", "deu", &["german"]),
    ("el", "gre", "ell", &["greek"]),
    ("en", "eng", "eng", &["english"]),
    ("es", "spa", "spa", &["spanish", "castilian"]),
    ("et", "est", "est", &["estonian"]),
    ("eu", "baq", "eus", &["basque"]),
    ("fa", "per", "fas", &["persian", "farsi"]),
    ("fi", "fin", "fin", &["finnish"]),
    ("fr", "fre", "fra", &["french"]),
    ("ga", "gle", "gle", &["irish"]),
    ("gl", "glg", "glg", &["galician"]),
    ("he", "heb", "heb", &["hebrew"]),
    ("hi", "hin", "hin", &["hindi"]),
    ("hr", "hrv", "hrv", &["croatian"]),
    ("hu", "hun", "hun", &["hungarian"]),
    ("hy", "arm", "hye", &["armenian"]),
    ("id", "ind", "ind", &["indonesian"]),
    ("is", "ice", "isl", &["icelandic"]),
    ("it", "ita", "ita", &["italian"]),
    ("ja", "jpn", "jpn", &["japanese"]),
    ("ka", "geo", "kat", &["georgian"]),
    ("ko", "kor", "kor", &["korean"]),
    ("la", "lat", "lat", &["latin"]),
    ("lt", "lit", "lit", &["lithuanian"]),
    ("lv", "lav", "lav", &["latvian"]),
    ("mk", "mac", "mkd", &["macedonian"]),
    ("ms", "may", "msa", &["malay"]),
    ("nb", "nob", "nob", &["bokmal", "bokmål"]),
    ("nl", "dut", "nld", &["dutch", "flemish"]),
    ("nn", "nno", "nno", &["nynorsk"]),
    ("no", "nor", "nor", &["norwegian"]),
    ("pl", "pol", "pol", &["polish"]),
    ("pt", "por", "por", &["portuguese"]),
    ("ro", "rum", "ron", &["romanian"]),
    ("ru", "rus", "rus", &["russian"]),
    ("sk", "slo", "slk", &["slovak"]),
    ("sl", "slv", "slv", &["slovenian"]),
    ("sq", "alb", "sqi", &["albanian"]),
    ("sr", "srp", "srp", &["serbian"]),
    ("sv", "swe", "swe", &["swedish"]),
    ("ta", "tam", "tam", &["tamil"]),
    ("te", "tel", "tel", &["telugu"]),
    ("th", "tha", "tha", &["thai"]),
    ("tl", "tgl", "tgl", &["tagalog"]),
    ("tr", "tur", "tur", &["turkish"]),
    ("uk", "ukr", "ukr", &["ukrainian"]),
    ("ur", "urd", "urd", &["urdu"]),
    ("vi", "vie", "vie", &["vietnamese"]),
    ("zh", "chi", "zho", &["chinese"]),
];

/// The ISO 639-2/B code of the language `token` spells -- as a 639-1 or
/// 639-2 code, an English name, or a BCP 47 tag whose primary subtag is one
/// of those (`pt-BR`, `zh_Hans`) -- in any case.
pub fn language_code(token: &str) -> Option<&'static str> {
    let primary = token.split(['-', '_']).next().unwrap_or(token);
    let lower = primary.to_lowercase();
    LANGUAGES
        .iter()
        .find(|(one, bibliographic, terminologic, names)| {
            lower == *one
                || lower == *bibliographic
                || lower == *terminologic
                || names.contains(&lower.as_str())
        })
        .map(|(_, bibliographic, _, _)| *bibliographic)
}

/// Whether `token` is a BCP 47 tag with more than its primary language
/// subtag (`pt-BR`): the rest is kept as the track's title.
fn has_subtags(token: &str) -> bool {
    token.contains(['-', '_'])
}

/// What the tokens after a subtitle's video stem say.
fn read_tokens<'a>(
    format: SubtitleFormat,
    tokens: impl IntoIterator<Item = &'a str>,
) -> SidecarInfo {
    let mut info = SidecarInfo {
        format,
        language: None,
        title: None,
        is_forced: false,
        is_sdh: false,
        is_default: false,
    };
    let mut title: Vec<&str> = Vec::new();
    for token in tokens.into_iter().filter(|t| !t.is_empty()) {
        match token.to_ascii_lowercase().as_str() {
            "forced" | "foreign" => info.is_forced = true,
            "sdh" | "cc" => info.is_sdh = true,
            // `hi` is Hindi on its own, and "hearing impaired" after a
            // language (`Movie.en.hi.srt`).
            "hi" if info.language.is_some() => info.is_sdh = true,
            "default" => info.is_default = true,
            _ => match language_code(token).filter(|_| info.language.is_none()) {
                Some(code) => {
                    info.language = Some(code.to_string());
                    if has_subtags(token) {
                        title.push(token);
                    }
                }
                None => title.push(token),
            },
        }
    }
    info.title = (!title.is_empty()).then(|| title.join(" "));
    info
}

/// The stem and extension of a file name: `("Movie.en", "srt")`.
fn stem_and_extension(file_name: &str) -> Option<(&str, &str)> {
    let (stem, extension) = file_name.rsplit_once('.')?;
    (!stem.is_empty()).then_some((stem, extension))
}

/// What the subtitle file `subtitle_name` says, if it is named for the video
/// whose filename stem is `video_stem`: that stem, compared ignoring ASCII
/// case, then nothing or `.` and its tokens, then a subtitle extension.
/// `None` for another video's subtitle, or a file that is not a text
/// subtitle.
pub fn infer_sidecar(video_stem: &str, subtitle_name: &str) -> Option<SidecarInfo> {
    let (stem, extension) = stem_and_extension(subtitle_name)?;
    let format = SubtitleFormat::from_extension(extension)?;
    if stem.eq_ignore_ascii_case(video_stem) {
        return Some(read_tokens(format, []));
    }
    let prefix_len = video_stem.len();
    if video_stem.is_empty()
        || !stem.is_char_boundary(prefix_len)
        || stem.len() <= prefix_len
        || !stem[..prefix_len].eq_ignore_ascii_case(video_stem)
        || !stem[prefix_len..].starts_with('.')
    {
        return None;
    }
    Some(read_tokens(format, stem[prefix_len + 1..].split('.')))
}

/// Whether a folder is a `Subs/` or `Subtitles/` folder, in any case.
fn is_subtitle_folder(name: &str) -> bool {
    name.eq_ignore_ascii_case("subs") || name.eq_ignore_ascii_case("subtitles")
}

fn file_stem(path: &Path) -> Option<&str> {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(stem_and_extension)
        .map(|(stem, _)| stem)
}

/// Which of `videos` the subtitle at `subtitle` belongs to, and what its name
/// says. Both are paths in the same space (both absolute, or both relative to
/// one root).
///
/// A video in the subtitle's folder -- or, when that folder is `Subs/` or
/// `Subtitles/`, in the folder above -- whose stem the subtitle's name starts
/// with owns it; of several, the longest stem (`Movie.Extended.en.srt`
/// belongs to `Movie.Extended.mkv`, not `Movie.mkv`). A subtitle in a
/// `Subs/` folder that names no video belongs to the only video above it, if
/// there is exactly one, and its whole name is read for tokens. Anything
/// else belongs to no video.
pub fn match_sidecar<'a>(subtitle: &Path, videos: &[&'a Path]) -> Option<(&'a Path, SidecarInfo)> {
    let name = subtitle.file_name()?.to_str()?;
    let folder = subtitle.parent()?;
    let in_subtitle_folder = folder
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_subtitle_folder);
    let above = folder.parent().filter(|_| in_subtitle_folder);
    let beside = |video: &Path| {
        let parent = video.parent();
        parent == Some(folder) || (above.is_some() && parent == above)
    };

    // Of the videos the name starts with, the one with the longest stem.
    let mut named: Option<(usize, &'a Path, SidecarInfo)> = None;
    for &video in videos {
        if !beside(video) {
            continue;
        }
        let Some(stem) = file_stem(video) else {
            continue;
        };
        if let Some(info) = infer_sidecar(stem, name)
            && named.as_ref().is_none_or(|(len, _, _)| stem.len() > *len)
        {
            named = Some((stem.len(), video, info));
        }
    }
    if let Some((_, video, info)) = named {
        return Some((video, info));
    }

    let above = above?;
    let mut only = videos.iter().copied().filter(|v| v.parent() == Some(above));
    let (Some(video), None) = (only.next(), only.next()) else {
        return None;
    };
    let (stem, extension) = stem_and_extension(name)?;
    let format = SubtitleFormat::from_extension(extension)?;
    Some((video, read_tokens(format, stem.split(['.', '_']))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn info(
        format: SubtitleFormat,
        language: Option<&str>,
        title: Option<&str>,
        forced: bool,
        sdh: bool,
    ) -> SidecarInfo {
        SidecarInfo {
            format,
            language: language.map(str::to_string),
            title: title.map(str::to_string),
            is_forced: forced,
            is_sdh: sdh,
            is_default: false,
        }
    }

    #[test]
    fn a_subtitle_named_for_its_video_says_its_language_and_flags() {
        use SubtitleFormat::*;
        let cases: &[(&str, Option<SidecarInfo>)] = &[
            ("Movie.srt", Some(info(Srt, None, None, false, false))),
            (
                "Movie.en.srt",
                Some(info(Srt, Some("eng"), None, false, false)),
            ),
            (
                "Movie.eng.forced.srt",
                Some(info(Srt, Some("eng"), None, true, false)),
            ),
            (
                "Movie.English.SDH.srt",
                Some(info(Srt, Some("eng"), None, false, true)),
            ),
            (
                "Movie.pt-BR.vtt",
                Some(info(Vtt, Some("por"), Some("pt-BR"), false, false)),
            ),
            (
                "Movie.deu.ass",
                Some(info(Ass, Some("ger"), None, false, false)),
            ),
            (
                "Movie.ger.ssa",
                Some(info(Ssa, Some("ger"), None, false, false)),
            ),
            // `hi` alone is Hindi; after a language it is "hearing impaired".
            (
                "Movie.hi.srt",
                Some(info(Srt, Some("hin"), None, false, false)),
            ),
            (
                "Movie.en.hi.srt",
                Some(info(Srt, Some("eng"), None, false, true)),
            ),
            (
                "Movie.en.cc.srt",
                Some(info(Srt, Some("eng"), None, false, true)),
            ),
            ("Movie.forced.srt", Some(info(Srt, None, None, true, false))),
            (
                "Movie.Commentary.en.srt",
                Some(info(Srt, Some("eng"), Some("Commentary"), false, false)),
            ),
            (
                "MOVIE.EN.SRT",
                Some(info(Srt, Some("eng"), None, false, false)),
            ),
            // Another video's subtitle, or no text subtitle at all.
            ("Movies.en.srt", None),
            ("Movie2.en.srt", None),
            ("Other.en.srt", None),
            ("Movie.en.sub", None),
            ("Movie.en.sup", None),
            ("Movie.nfo", None),
        ];
        for (name, expected) in cases {
            assert_eq!(&infer_sidecar("Movie", name), expected, "{name}");
        }
        let default = infer_sidecar("Movie", "Movie.fr.default.srt").unwrap();
        assert!(default.is_default);
        assert_eq!(default.language.as_deref(), Some("fre"));
    }

    #[test]
    fn a_video_stem_with_dots_and_a_year_is_matched_whole() {
        let info = infer_sidecar("The.Matrix.1999.1080p", "The.Matrix.1999.1080p.es.srt").unwrap();
        assert_eq!(info.language.as_deref(), Some("spa"));
        assert_eq!(info.title, None);
        assert_eq!(
            infer_sidecar("The.Matrix.1999", "The.Matrix.1999.1080p.es.srt")
                .unwrap()
                .title
                .as_deref(),
            Some("1080p")
        );
    }

    #[test]
    fn every_spelling_of_a_listed_language_stores_its_bibliographic_code() {
        // An invariant of the table rather than a copy of it: whichever
        // spelling a name uses, the stored code reads back as itself.
        for (one, bibliographic, terminologic, names) in LANGUAGES {
            for spelling in [*one, *bibliographic, *terminologic]
                .into_iter()
                .chain(names.iter().copied())
            {
                assert_eq!(language_code(spelling), Some(*bibliographic), "{spelling}");
                assert_eq!(
                    language_code(&spelling.to_uppercase()),
                    Some(*bibliographic)
                );
            }
            assert_eq!(language_code(bibliographic), Some(*bibliographic));
        }
    }

    #[test]
    fn no_spelling_names_two_languages() {
        let mut seen = std::collections::HashMap::new();
        for (one, bibliographic, terminologic, names) in LANGUAGES {
            for spelling in [*one, *bibliographic, *terminologic]
                .into_iter()
                .chain(names.iter().copied())
            {
                if let Some(other) = seen.insert(spelling, *bibliographic) {
                    assert_eq!(other, *bibliographic, "{spelling} names two languages");
                }
            }
        }
    }

    #[test]
    fn the_longest_video_stem_owns_a_subtitle() {
        let plain = Path::new("/lib/Movie/Movie.mkv");
        let extended = Path::new("/lib/Movie/Movie.Extended.mkv");
        let videos = [plain, extended];
        let (owner, info) =
            match_sidecar(Path::new("/lib/Movie/Movie.Extended.en.srt"), &videos).unwrap();
        assert_eq!(owner, extended);
        assert_eq!(info.language.as_deref(), Some("eng"));
        let (owner, _) = match_sidecar(Path::new("/lib/Movie/Movie.en.srt"), &videos).unwrap();
        assert_eq!(owner, plain);
    }

    #[test]
    fn a_subtitle_never_belongs_to_a_video_in_another_folder() {
        let videos = [Path::new("/lib/A/Movie.mkv")];
        assert_eq!(
            match_sidecar(Path::new("/lib/B/Movie.en.srt"), &videos),
            None
        );
        assert_eq!(
            match_sidecar(Path::new("/lib/A/Extras/Movie.en.srt"), &videos),
            None
        );
    }

    #[test]
    fn a_subs_folder_holds_subtitles_named_for_a_video_or_for_the_only_one() {
        let one = [Path::new("/lib/Movie (1999)/Movie (1999).mkv")];
        let (owner, info) =
            match_sidecar(Path::new("/lib/Movie (1999)/Subs/English.srt"), &one).unwrap();
        assert_eq!(owner, one[0]);
        assert_eq!(info.language.as_deref(), Some("eng"));
        let (_, plex) = match_sidecar(
            Path::new("/lib/Movie (1999)/Subtitles/2_English.forced.srt"),
            &one,
        )
        .unwrap();
        assert_eq!(
            (
                plex.language.as_deref(),
                plex.is_forced,
                plex.title.as_deref()
            ),
            (Some("eng"), true, Some("2"))
        );

        let two = [
            Path::new("/lib/Show/S01E01.mkv"),
            Path::new("/lib/Show/S01E02.mkv"),
        ];
        let (owner, _) = match_sidecar(Path::new("/lib/Show/Subs/S01E02.fr.srt"), &two).unwrap();
        assert_eq!(owner, two[1]);
        assert_eq!(
            match_sidecar(Path::new("/lib/Show/Subs/English.srt"), &two),
            None,
            "with two videos a bare language names neither"
        );
    }

    proptest! {
        #[test]
        fn inference_never_panics_and_a_language_is_a_known_code(
            stem in "\\PC{0,12}",
            name in "\\PC{0,30}",
        ) {
            if let Some(info) = infer_sidecar(&stem, &name)
                && let Some(language) = info.language
            {
                prop_assert_eq!(language_code(&language), Some(language.as_str()));
            }
        }

        #[test]
        fn a_name_that_does_not_start_with_the_stem_is_never_matched(
            stem in "[A-Za-z]{1,10}",
            other in "[A-Za-z]{1,10}",
            tokens in "(\\.[a-z]{2,3}){0,3}",
        ) {
            prop_assume!(!other.to_ascii_lowercase().starts_with(&stem.to_ascii_lowercase()));
            prop_assert_eq!(infer_sidecar(&stem, &format!("{other}{tokens}.srt")), None);
        }

        #[test]
        fn a_name_that_is_the_stem_and_tokens_is_always_matched(
            stem in "[A-Za-z0-9 ()]{1,16}",
            tokens in "(\\.[a-zA-Z]{1,8}){0,4}",
            format in prop::sample::select(SubtitleFormat::ALL.to_vec()),
        ) {
            let name = format!("{stem}{tokens}.{}", format.as_str());
            let info = infer_sidecar(&stem, &name);
            prop_assert_eq!(info.map(|i| i.format), Some(format));
        }
    }
}
