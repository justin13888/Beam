//! The audio and subtitle track a client starts a source on, for one user
//! (issue #264).
//!
//! Pure: it reads a source's tracks, as their position in the source's list,
//! and the user's [`LanguagePreferences`], and names a position. The server
//! states the result on each source so every client starts on the same
//! tracks instead of each reimplementing the choice.
//!
//! **Audio.** The first preferred audio language the source carries wins;
//! within it, the track the file marks default, else the first. With no
//! preferred language carried, the file's default track, else its first.
//!
//! **Subtitles**, by [`SubtitleMode`]:
//! - `Off`: none.
//! - `ForcedOnly`: the forced track in the chosen audio track's language --
//!   the subtitles for a film's foreign-language dialogue. None when the
//!   audio language is unknown or the source carries no such track.
//! - `On`: a full (not forced) track in the first preferred subtitle
//!   language the source carries; with no preferred language listed, the
//!   full track the file marks default. Failing both, what `ForcedOnly`
//!   picks.
//!
//! Among several candidates in one language: the file's default first, then
//! one that is not for the hearing impaired, then text over an image format,
//! then list order -- so a tie never depends on anything but the source.

use crate::models::language_preferences::{LanguageCode, LanguagePreferences, SubtitleMode};

/// What the choice reads of an audio track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioCandidate<'a> {
    /// The language the file tags the track with, as tagged.
    pub language: Option<&'a str>,
    pub is_default: bool,
}

/// What the choice reads of a subtitle track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtitleCandidate<'a> {
    pub language: Option<&'a str>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_hearing_impaired: bool,
    pub is_text: bool,
}

/// The position in `tracks` of the audio track to start on; `None` only for
/// a source with no audio.
#[must_use]
pub fn default_audio(prefs: &LanguagePreferences, tracks: &[AudioCandidate<'_>]) -> Option<usize> {
    let best_of = |positions: &mut dyn Iterator<Item = usize>| {
        positions.min_by_key(|&position| (!tracks[position].is_default, position))
    };
    prefs
        .audio_languages()
        .iter()
        .find_map(|language| {
            best_of(&mut (0..tracks.len()).filter(|&p| language.matches(tracks[p].language)))
        })
        .or_else(|| best_of(&mut (0..tracks.len())))
}

/// The position in `subtitles` of the subtitle track to start on, beside the
/// audio track tagged `audio_language`; `None` to start without subtitles.
#[must_use]
pub fn default_subtitle(
    prefs: &LanguagePreferences,
    audio_language: Option<&str>,
    subtitles: &[SubtitleCandidate<'_>],
) -> Option<usize> {
    let best = |keep: &dyn Fn(&SubtitleCandidate<'_>) -> bool| {
        (0..subtitles.len())
            .filter(|&position| keep(&subtitles[position]))
            .min_by_key(|&position| {
                let track = &subtitles[position];
                (
                    !track.is_default,
                    track.is_hearing_impaired,
                    !track.is_text,
                    position,
                )
            })
    };
    let forced = || {
        let audio = audio_language.and_then(LanguageCode::parse)?;
        best(&|track| track.is_forced && audio.matches(track.language))
    };
    match prefs.subtitle_mode() {
        SubtitleMode::Off => None,
        SubtitleMode::ForcedOnly => forced(),
        SubtitleMode::On => {
            let full = if prefs.subtitle_languages().is_empty() {
                best(&|track| !track.is_forced && track.is_default)
            } else {
                prefs.subtitle_languages().iter().find_map(|language| {
                    best(&|track| !track.is_forced && language.matches(track.language))
                })
            };
            full.or_else(forced)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::language_preferences::test_utils::prefs;
    use proptest::prelude::*;

    fn audio(language: Option<&str>, is_default: bool) -> AudioCandidate<'_> {
        AudioCandidate {
            language,
            is_default,
        }
    }

    /// A full, text, not-SDH, not-default subtitle in `language`.
    fn sub(language: Option<&str>) -> SubtitleCandidate<'_> {
        SubtitleCandidate {
            language,
            is_default: false,
            is_forced: false,
            is_hearing_impaired: false,
            is_text: true,
        }
    }

    fn forced(language: Option<&str>) -> SubtitleCandidate<'_> {
        SubtitleCandidate {
            is_forced: true,
            ..sub(language)
        }
    }

    #[test]
    fn audio_takes_the_first_preferred_language_the_source_carries() {
        let tracks = [
            audio(Some("eng"), true),
            audio(Some("jpn"), false),
            audio(Some("fre"), false),
        ];
        let cases: [(&[&str], Option<usize>); 6] = [
            (&[], Some(0)),
            (&["ja"], Some(1)),
            (&["ger", "fra"], Some(2)),
            (&["fre", "jpn"], Some(2)),
            (&["kor"], Some(0)),
            (&["kor", "jpn", "eng"], Some(1)),
        ];
        for (languages, expected) in cases {
            assert_eq!(
                default_audio(&prefs(languages, &[], SubtitleMode::Off), &tracks),
                expected,
                "{languages:?}"
            );
        }
    }

    #[test]
    fn within_a_language_the_files_default_wins_then_the_first() {
        let tracks = [
            audio(Some("eng"), false),
            audio(Some("jpn"), false),
            audio(Some("eng"), true),
        ];
        let english = prefs(&["eng"], &[], SubtitleMode::Off);
        assert_eq!(default_audio(&english, &tracks), Some(2));
        let japanese = prefs(&["jpn"], &[], SubtitleMode::Off);
        assert_eq!(default_audio(&japanese, &tracks), Some(1));

        let no_default = [audio(None, false), audio(Some("eng"), false)];
        assert_eq!(
            default_audio(&prefs(&["kor"], &[], SubtitleMode::Off), &no_default),
            Some(0)
        );
        assert_eq!(default_audio(&english, &[]), None);
    }

    #[test]
    fn subtitles_off_never_picks_one() {
        let subtitles = [forced(Some("eng")), sub(Some("eng"))];
        assert_eq!(
            default_subtitle(
                &prefs(&[], &["eng"], SubtitleMode::Off),
                Some("eng"),
                &subtitles
            ),
            None
        );
    }

    #[test]
    fn forced_only_follows_the_audio_language() {
        let subtitles = [
            sub(Some("eng")),
            forced(Some("fre")),
            forced(Some("eng")),
            forced(None),
        ];
        let forced_only = prefs(&[], &["fre"], SubtitleMode::ForcedOnly);
        assert_eq!(
            default_subtitle(&forced_only, Some("eng"), &subtitles),
            Some(2)
        );
        assert_eq!(
            default_subtitle(&forced_only, Some("fra"), &subtitles),
            Some(1)
        );
        assert_eq!(
            default_subtitle(&forced_only, Some("jpn"), &subtitles),
            None
        );
        assert_eq!(default_subtitle(&forced_only, None, &subtitles), None);
        assert_eq!(
            default_subtitle(&forced_only, Some("und"), &subtitles),
            None
        );
    }

    #[test]
    fn on_takes_the_first_preferred_language_carried_as_full_subtitles() {
        let subtitles = [
            forced(Some("eng")),
            sub(Some("spa")),
            sub(Some("eng")),
            forced(Some("ger")),
        ];
        let cases: [(&[&str], Option<&str>, Option<usize>); 6] = [
            (&["eng", "spa"], Some("jpn"), Some(2)),
            (&["kor", "es"], Some("jpn"), Some(1)),
            // Only a forced German track: no full subtitles in German, so
            // the forced track for the audio stands in.
            (&["ger"], Some("eng"), Some(0)),
            (&["ger"], Some("ger"), Some(3)),
            (&["kor"], Some("jpn"), None),
            (&["kor"], None, None),
        ];
        for (languages, audio_language, expected) in cases {
            assert_eq!(
                default_subtitle(
                    &prefs(&[], languages, SubtitleMode::On),
                    audio_language,
                    &subtitles
                ),
                expected,
                "{languages:?} beside {audio_language:?}"
            );
        }
    }

    #[test]
    fn on_with_no_language_listed_takes_the_files_default_full_track() {
        let marked = SubtitleCandidate {
            is_default: true,
            ..sub(Some("spa"))
        };
        let on = prefs(&[], &[], SubtitleMode::On);
        assert_eq!(
            default_subtitle(&on, Some("eng"), &[sub(Some("eng")), marked]),
            Some(1)
        );
        assert_eq!(
            default_subtitle(&on, Some("eng"), &[sub(Some("eng")), forced(Some("eng"))]),
            Some(1),
            "no default full track: the forced track for the audio"
        );
        let marked_forced = SubtitleCandidate {
            is_default: true,
            ..forced(Some("fre"))
        };
        assert_eq!(default_subtitle(&on, Some("eng"), &[marked_forced]), None);
    }

    #[test]
    fn a_tie_in_one_language_prefers_default_then_not_sdh_then_text() {
        let on = prefs(&[], &["eng"], SubtitleMode::On);
        let sdh = SubtitleCandidate {
            is_hearing_impaired: true,
            ..sub(Some("eng"))
        };
        let image = SubtitleCandidate {
            is_text: false,
            ..sub(Some("eng"))
        };
        let default_sdh = SubtitleCandidate {
            is_default: true,
            ..sdh
        };
        assert_eq!(
            default_subtitle(&on, None, &[sdh, image, sub(Some("eng"))]),
            Some(2)
        );
        assert_eq!(default_subtitle(&on, None, &[sdh, image]), Some(1));
        assert_eq!(
            default_subtitle(&on, None, &[image, sdh, default_sdh]),
            Some(2)
        );
        assert_eq!(default_subtitle(&on, None, &[sdh, sdh]), Some(0));
    }

    fn language() -> impl Strategy<Value = Option<&'static str>> {
        prop::option::of(prop::sample::select(vec![
            "eng", "jpn", "ger", "deu", "und",
        ]))
    }

    fn mode() -> impl Strategy<Value = SubtitleMode> {
        prop::sample::select(SubtitleMode::ALL.to_vec())
    }

    proptest! {
        /// A pick is always a track of the source; audio is picked whenever
        /// there is any; a subtitle pick honours the mode.
        #[test]
        fn a_pick_is_a_track_of_the_source_and_honours_the_mode(
            audio_tracks in prop::collection::vec((language(), any::<bool>()), 0..6),
            subtitle_tracks in prop::collection::vec(
                (language(), any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()),
                0..8,
            ),
            audio_prefs in prop::collection::vec(prop::sample::select(vec!["eng", "jpn", "ger"]), 0..3),
            subtitle_prefs in prop::collection::vec(prop::sample::select(vec!["eng", "jpn", "ger"]), 0..3),
            mode in mode(),
        ) {
            let prefs = prefs(&audio_prefs, &subtitle_prefs, mode);
            let audio: Vec<_> = audio_tracks
                .iter()
                .map(|&(language, is_default)| AudioCandidate { language, is_default })
                .collect();
            let subtitles: Vec<_> = subtitle_tracks
                .iter()
                .map(|&(language, is_default, is_forced, is_hearing_impaired, is_text)| {
                    SubtitleCandidate { language, is_default, is_forced, is_hearing_impaired, is_text }
                })
                .collect();

            let chosen_audio = default_audio(&prefs, &audio);
            prop_assert_eq!(chosen_audio.is_some(), !audio.is_empty());
            prop_assert!(chosen_audio.is_none_or(|p| p < audio.len()));

            let audio_language = chosen_audio.and_then(|p| audio[p].language);
            let chosen = default_subtitle(&prefs, audio_language, &subtitles);
            prop_assert!(chosen.is_none_or(|p| p < subtitles.len()));
            match mode {
                SubtitleMode::Off => prop_assert_eq!(chosen, None),
                SubtitleMode::ForcedOnly => {
                    let for_the_audio = chosen.is_none_or(|p| {
                        subtitles[p].is_forced
                            && LanguageCode::parse(audio_language.unwrap_or_default())
                                .is_some_and(|audio| audio.matches(subtitles[p].language))
                    });
                    prop_assert!(for_the_audio);
                }
                SubtitleMode::On => {}
            }
        }
    }
}
