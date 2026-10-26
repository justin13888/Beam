//! Wire shapes for the caller's own language preferences (issue #264).

use beam_domain::models::language_preferences::{self, LanguagePreferences};
use kynos::Schema;
use serde::{Deserialize, Serialize};

/// Whether the caller wants subtitles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleMode {
    /// Never start on a subtitle track.
    Off,
    /// Only the forced track in the audio's language: the subtitles a film
    /// carries for its foreign-language dialogue.
    ForcedOnly,
    /// Full subtitles in the first of `subtitle_languages` a source carries
    /// -- with none listed, the full track the file marks default -- else
    /// what `forced_only` picks.
    On,
}

impl From<language_preferences::SubtitleMode> for SubtitleMode {
    fn from(mode: language_preferences::SubtitleMode) -> Self {
        match mode {
            language_preferences::SubtitleMode::Off => Self::Off,
            language_preferences::SubtitleMode::ForcedOnly => Self::ForcedOnly,
            language_preferences::SubtitleMode::On => Self::On,
        }
    }
}

impl From<SubtitleMode> for language_preferences::SubtitleMode {
    fn from(mode: SubtitleMode) -> Self {
        match mode {
            SubtitleMode::Off => Self::Off,
            SubtitleMode::ForcedOnly => Self::ForcedOnly,
            SubtitleMode::On => Self::On,
        }
    }
}

/// The audio and subtitle languages the caller prefers, and whether they
/// want subtitles: what `GET /v1/media/{id}/sources` reads to name each
/// source's default audio and subtitle track.
///
/// A language is sent as an ISO 639-1 or 639-2 code (`en`, `eng`), an
/// English name (`English`) or a BCP 47 tag (`en-GB`), and is answered as
/// the ISO 639-2/B code a track is tagged with (`eng`; `ger` for German,
/// whether sent as `de`, `deu` or `ger`). A language listed twice keeps its
/// first place. Each list holds at most 16 languages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct LanguagePreferenceSettings {
    /// Audio languages, most preferred first. Empty: the file's default
    /// track.
    #[schema(max_items = 16)]
    pub audio_languages: Vec<String>,
    /// Subtitle languages, most preferred first.
    #[schema(max_items = 16)]
    pub subtitle_languages: Vec<String>,
    pub subtitle_mode: SubtitleMode,
}

impl From<&LanguagePreferences> for LanguagePreferenceSettings {
    fn from(preferences: &LanguagePreferences) -> Self {
        let codes = |codes: &[language_preferences::LanguageCode]| {
            codes.iter().map(|code| code.as_str().to_owned()).collect()
        };
        Self {
            audio_languages: codes(preferences.audio_languages()),
            subtitle_languages: codes(preferences.subtitle_languages()),
            subtitle_mode: preferences.subtitle_mode().into(),
        }
    }
}
