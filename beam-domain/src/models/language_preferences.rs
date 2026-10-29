//! Which audio and subtitle languages a user wants (issue #264).
//!
//! A user lists the audio languages they prefer and the subtitle languages
//! they read, each most preferred first, and says whether they want
//! subtitles at all. The sources of a title then name, per user, the audio
//! and subtitle track a client starts on
//! ([`crate::utils::track_selection`]). A user who never said anything has
//! [`LanguagePreferences::default`]: no preferred language, and forced
//! subtitles only -- the file's own choices stand.

use crate::utils::sidecar::language_code;

/// The most languages one list may hold. A person reads a handful; a list
/// past this is a client bug, refused rather than stored and scanned on
/// every sources read.
pub const MAX_PREFERRED_LANGUAGES: usize = 16;

/// A language as tracks are tagged with it: an ISO 639-2/B code (`eng`,
/// `ger`, `chi`), lowercase.
///
/// [`Self::parse`] accepts every spelling a subtitle's name may use -- a
/// 639-1 code (`de`), either 639-2 code (`ger`, `deu`), an English name
/// (`German`) or a BCP 47 tag (`de-AT`) -- and stores the bibliographic
/// code, so `de` and `deu` are one preference. Any other three-letter code
/// is kept as given, lowercased: ISO 639-2 has hundreds of languages and
/// Beam's spelling table names the common ones only.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageCode(String);

impl LanguageCode {
    /// The language `raw` names, or `None` when it names none: empty, or
    /// neither a spelling Beam knows nor three ASCII letters. `und` (ISO
    /// 639-2's "undetermined") is no language and is refused too.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if let Some(code) = language_code(trimmed) {
            return Some(Self(code.to_owned()));
        }
        let lower = trimmed.to_ascii_lowercase();
        (lower.len() == 3 && lower.bytes().all(|b| b.is_ascii_lowercase()) && lower != "und")
            .then_some(Self(lower))
    }

    /// The code as stored and sent: three lowercase letters.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether a track tagged `tag` is in this language. A track's tag is
    /// read through [`Self::parse`] too, so a file tagging German `deu`
    /// matches a preference stored as `ger`; an untagged or `und` track is
    /// in no language.
    #[must_use]
    pub fn matches(&self, tag: Option<&str>) -> bool {
        tag.and_then(Self::parse)
            .is_some_and(|parsed| parsed == *self)
    }
}

/// Whether a user wants subtitles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SubtitleMode {
    /// Never start on a subtitle track.
    Off,
    /// Only the forced track for the audio's language: the subtitles a
    /// film carries for its foreign-language dialogue. The default.
    #[default]
    ForcedOnly,
    /// Full subtitles in the first preferred subtitle language a source
    /// carries.
    On,
}

impl SubtitleMode {
    /// Every mode.
    pub const ALL: [Self; 3] = [Self::Off, Self::ForcedOnly, Self::On];

    /// The spelling the store and the wire carry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::ForcedOnly => "forced_only",
            Self::On => "on",
        }
    }

    /// The inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == value)
    }
}

/// One user's language preferences.
///
/// Built through [`Self::new`], which holds each list to
/// [`MAX_PREFERRED_LANGUAGES`] distinct languages, so a stored value never
/// needs checking again.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LanguagePreferences {
    audio_languages: Vec<LanguageCode>,
    subtitle_languages: Vec<LanguageCode>,
    subtitle_mode: SubtitleMode,
}

/// Which lists named more than [`MAX_PREFERRED_LANGUAGES`] distinct
/// languages. At least one of the two is `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a language list names more than {MAX_PREFERRED_LANGUAGES} languages")]
pub struct TooManyLanguages {
    pub audio: bool,
    pub subtitle: bool,
}

impl LanguagePreferences {
    /// Preferences listing `audio_languages` and `subtitle_languages` in the
    /// order given. A language listed twice keeps its first place: `en`
    /// then `eng` is one preference for English.
    ///
    /// # Errors
    ///
    /// [`TooManyLanguages`] for a list of more than
    /// [`MAX_PREFERRED_LANGUAGES`] distinct languages.
    pub fn new(
        audio_languages: Vec<LanguageCode>,
        subtitle_languages: Vec<LanguageCode>,
        subtitle_mode: SubtitleMode,
    ) -> Result<Self, TooManyLanguages> {
        let audio_languages = distinct(audio_languages);
        let subtitle_languages = distinct(subtitle_languages);
        let too_many = TooManyLanguages {
            audio: audio_languages.len() > MAX_PREFERRED_LANGUAGES,
            subtitle: subtitle_languages.len() > MAX_PREFERRED_LANGUAGES,
        };
        if too_many.audio || too_many.subtitle {
            return Err(too_many);
        }
        Ok(Self {
            audio_languages,
            subtitle_languages,
            subtitle_mode,
        })
    }

    /// The audio languages, most preferred first.
    #[must_use]
    pub fn audio_languages(&self) -> &[LanguageCode] {
        &self.audio_languages
    }

    /// The subtitle languages, most preferred first.
    #[must_use]
    pub fn subtitle_languages(&self) -> &[LanguageCode] {
        &self.subtitle_languages
    }

    #[must_use]
    pub const fn subtitle_mode(&self) -> SubtitleMode {
        self.subtitle_mode
    }
}

/// `languages` without repeats, first place kept.
fn distinct(languages: Vec<LanguageCode>) -> Vec<LanguageCode> {
    let mut kept: Vec<LanguageCode> = Vec::with_capacity(languages.len());
    for language in languages {
        if !kept.contains(&language) {
            kept.push(language);
        }
    }
    kept
}

/// Builders for tests, beside the type they build.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use super::{LanguageCode, LanguagePreferences, SubtitleMode};

    /// The code `raw` parses to; panics on one that names no language.
    #[must_use]
    pub fn lang(raw: &str) -> LanguageCode {
        LanguageCode::parse(raw).unwrap_or_else(|| panic!("{raw:?} names no language"))
    }

    /// Preferences from codes, which must parse and be few enough.
    #[must_use]
    pub fn prefs(audio: &[&str], subtitle: &[&str], mode: SubtitleMode) -> LanguagePreferences {
        LanguagePreferences::new(
            audio.iter().map(|raw| lang(raw)).collect(),
            subtitle.iter().map(|raw| lang(raw)).collect(),
            mode,
        )
        .expect("valid preferences")
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::lang;
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn every_spelling_of_a_language_is_one_code() {
        for spelling in ["de", "ger", "deu", "German", "de-AT", " DEU "] {
            assert_eq!(lang(spelling).as_str(), "ger", "{spelling:?}");
        }
    }

    #[test]
    fn an_unlisted_three_letter_code_is_kept_lowercased() {
        assert_eq!(lang("HAW").as_str(), "haw");
    }

    #[test]
    fn what_names_no_language_is_refused() {
        for raw in ["", "  ", "und", "UND", "xx", "abcd", "e1g", "日本語"] {
            assert_eq!(LanguageCode::parse(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn a_track_matches_whatever_code_its_file_tags_it_with() {
        let german = lang("ger");
        assert!(german.matches(Some("deu")));
        assert!(german.matches(Some("ger")));
        assert!(!german.matches(Some("eng")));
        assert!(!german.matches(Some("und")));
        assert!(!german.matches(None));
    }

    #[test]
    fn a_repeated_language_keeps_its_first_place() {
        let prefs = LanguagePreferences::new(
            vec![lang("jpn"), lang("en"), lang("ja"), lang("eng")],
            vec![],
            SubtitleMode::On,
        )
        .unwrap();
        assert_eq!(prefs.audio_languages(), [lang("jpn"), lang("eng")]);
    }

    #[test]
    fn a_list_is_held_to_its_bound_after_repeats_go() {
        // Every three-letter code from `aaa` on, so none repeats.
        let codes = |n: usize| -> Vec<LanguageCode> {
            (0..n)
                .map(|i| {
                    let b = u8::try_from(i).unwrap();
                    lang(&format!(
                        "q{}{}",
                        char::from(b'a' + b / 26),
                        char::from(b'a' + b % 26)
                    ))
                })
                .collect()
        };
        assert!(
            LanguagePreferences::new(codes(MAX_PREFERRED_LANGUAGES), vec![], SubtitleMode::Off)
                .is_ok()
        );
        let mut doubled = codes(MAX_PREFERRED_LANGUAGES);
        doubled.extend(codes(MAX_PREFERRED_LANGUAGES));
        assert!(LanguagePreferences::new(vec![], doubled, SubtitleMode::Off).is_ok());

        assert_eq!(
            LanguagePreferences::new(
                codes(MAX_PREFERRED_LANGUAGES + 1),
                codes(MAX_PREFERRED_LANGUAGES + 1),
                SubtitleMode::Off
            ),
            Err(TooManyLanguages {
                audio: true,
                subtitle: true
            })
        );
        assert_eq!(
            LanguagePreferences::new(
                vec![],
                codes(MAX_PREFERRED_LANGUAGES + 1),
                SubtitleMode::Off
            ),
            Err(TooManyLanguages {
                audio: false,
                subtitle: true
            })
        );
        assert_eq!(
            LanguagePreferences::new(
                codes(MAX_PREFERRED_LANGUAGES + 1),
                vec![],
                SubtitleMode::Off
            ),
            Err(TooManyLanguages {
                audio: true,
                subtitle: false
            })
        );
    }

    proptest! {
        /// Whatever `parse` is given it does not panic, and whatever it
        /// accepts it stores as three lowercase letters other than `und`
        /// that parse back to themselves -- so a stored preference reads the
        /// same on every load, and a track tagged with a stored code matches
        /// it.
        #[test]
        fn a_parsed_code_is_a_fixed_point_of_parse(
            raw in prop_oneof![
                ".*",
                "[ ]?[a-zA-Z]{2,3}([-_][A-Za-z]{2})?[ ]?",
                proptest::sample::select(vec!["German", "english", "Japanese", "French"])
                    .prop_map(str::to_owned),
            ],
        ) {
            if let Some(code) = LanguageCode::parse(&raw) {
                let stored = code.as_str();
                prop_assert_eq!(stored.len(), 3, "{:?} stored as {:?}", raw, stored);
                prop_assert!(stored.bytes().all(|b| b.is_ascii_lowercase()), "{stored:?}");
                prop_assert_ne!(stored, "und");
                prop_assert_eq!(LanguageCode::parse(stored), Some(code.clone()));
                prop_assert!(code.matches(Some(stored)));
            }
        }
    }

    #[test]
    fn a_mode_reads_back_from_its_spelling() {
        for mode in SubtitleMode::ALL {
            assert_eq!(SubtitleMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(SubtitleMode::parse("forced"), None);
    }
}
