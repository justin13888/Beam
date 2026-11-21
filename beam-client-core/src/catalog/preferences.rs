//! The viewer's language preferences (issue #264): what the server reads to
//! name each source's default audio and subtitle track.

use crate::api::types;

/// Whether the viewer wants subtitles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SubtitleMode {
    /// Never start on a subtitle track.
    Off,
    /// Only the forced track in the audio's language: the subtitles a film
    /// carries for its foreign-language dialogue.
    ForcedOnly,
    /// Full subtitles in the first of the subtitle languages a source
    /// carries -- with none listed, the full track the file marks default --
    /// else what [`SubtitleMode::ForcedOnly`] picks.
    On,
}

/// The audio and subtitle languages the viewer prefers, most preferred first,
/// and whether they want subtitles.
///
/// A language is sent as an ISO 639-1 or 639-2 code (`en`, `eng`), an English
/// name (`English`) or a BCP 47 tag (`en-GB`); the server answers each as the
/// ISO 639-2/B code a track is tagged with (`eng`, `ger`), keeps a language
/// listed twice at its first place, and refuses more than 16 in a list.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LanguagePreferences {
    /// Audio languages, most preferred first. Empty: the file's default track.
    pub audio_languages: Vec<String>,
    /// Subtitle languages, most preferred first.
    pub subtitle_languages: Vec<String>,
    /// Whether the viewer wants subtitles.
    pub subtitle_mode: SubtitleMode,
}

impl LanguagePreferences {
    /// Normalise the generated settings into the core's own record.
    #[must_use]
    pub fn from_generated(settings: types::LanguagePreferenceSettings) -> Self {
        let types::LanguagePreferenceSettings {
            audio_languages,
            subtitle_languages,
            subtitle_mode,
        } = settings;
        Self {
            audio_languages,
            subtitle_languages,
            subtitle_mode: match subtitle_mode {
                types::SubtitleMode::Off => SubtitleMode::Off,
                types::SubtitleMode::ForcedOnly => SubtitleMode::ForcedOnly,
                types::SubtitleMode::On => SubtitleMode::On,
            },
        }
    }

    /// The wire body that stores these preferences.
    #[must_use]
    pub fn to_generated(&self) -> types::LanguagePreferenceSettings {
        let Self {
            audio_languages,
            subtitle_languages,
            subtitle_mode,
        } = self;
        types::LanguagePreferenceSettings {
            audio_languages: audio_languages.clone(),
            subtitle_languages: subtitle_languages.clone(),
            subtitle_mode: match subtitle_mode {
                SubtitleMode::Off => types::SubtitleMode::Off,
                SubtitleMode::ForcedOnly => types::SubtitleMode::ForcedOnly,
                SubtitleMode::On => types::SubtitleMode::On,
            },
        }
    }
}
