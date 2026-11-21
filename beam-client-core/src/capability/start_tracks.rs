//! The audio and subtitle track a part starts on, as the platform player can
//! find them (issue #329).
//!
//! The server names each source's default tracks for the signed-in viewer
//! (issue #264) as positions in the lists it serves: `audio_tracks`, and
//! `subtitle_tracks` -- the file's own subtitle streams first, by stream
//! index, then the subtitle files beside it. A platform player does not see
//! those lists. Media3 exposes the file's own streams in container order and
//! each sidecar under the identity it was loaded with, so the position is
//! translated here, once, into the terms a player can match.

use super::select::{MediaSourceView, SubtitleOrigin, SubtitleTrackView};

/// The tracks the server names for one part, in terms the player can match.
///
/// A part carries one only when the server's source list was read: a part
/// planned without it (the lookup failed or timed out, or the title is not
/// known) carries none, and the player keeps its own choice.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StartTracks {
    /// The audio track's position among the file's audio streams, which is
    /// also its index in the source's `audio_tracks`; absent when the server
    /// names none (a file without audio).
    pub audio_index: Option<u32>,
    /// The subtitle track to start on; absent to start without subtitles.
    pub subtitle: Option<StartSubtitle>,
}

/// A subtitle track to start on, as the platform player finds it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum StartSubtitle {
    /// One of the file's own subtitle streams.
    Embedded {
        /// Its position among the file's subtitle streams, from 0, in
        /// container order: how many of the file's own subtitle tracks the
        /// server lists before it.
        position: u32,
    },
    /// A subtitle file beside the media file.
    Sidecar {
        /// The subtitle file's identifier, as the part's sidecar list
        /// carries it.
        sidecar_id: String,
    },
}

/// The start tracks for a part whose subtitle tracks are `subtitles`, of which
/// the server names `subtitle_index`, in `source`.
///
/// The audio track is the source's: every part of a source is cut from the
/// same rip, so the server names it once. A subtitle index the list does not
/// hold, or a sidecar without an identifier, is treated as no subtitle rather
/// than guessed at.
#[must_use]
pub fn start_tracks(
    source: &MediaSourceView,
    subtitles: &[SubtitleTrackView],
    subtitle_index: Option<u32>,
) -> StartTracks {
    let subtitle = subtitle_index
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| {
            let track = subtitles.get(index)?;
            match track.origin {
                SubtitleOrigin::Embedded => {
                    let before = subtitles[..index]
                        .iter()
                        .filter(|earlier| earlier.origin == SubtitleOrigin::Embedded)
                        .count();
                    u32::try_from(before)
                        .ok()
                        .map(|position| StartSubtitle::Embedded { position })
                }
                SubtitleOrigin::Sidecar => track
                    .sidecar_id
                    .clone()
                    .map(|sidecar_id| StartSubtitle::Sidecar { sidecar_id }),
            }
        });
    StartTracks {
        audio_index: source.default_audio_track_index,
        subtitle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::builders::MediaSourceBuilder;

    fn embedded(index: u32) -> SubtitleTrackView {
        SubtitleTrackView {
            origin: SubtitleOrigin::Embedded,
            index: Some(index),
            sidecar_id: None,
            codec: "subrip".to_owned(),
            language: Some("eng".to_owned()),
            title: None,
            is_default: false,
            is_forced: false,
            is_hearing_impaired: false,
            is_text: true,
            url: None,
            webvtt_url: None,
        }
    }

    fn sidecar(id: Option<&str>) -> SubtitleTrackView {
        SubtitleTrackView {
            origin: SubtitleOrigin::Sidecar,
            index: None,
            sidecar_id: id.map(str::to_owned),
            url: Some("https://beam.test/v1/files/f/subtitles/s".to_owned()),
            ..embedded(0)
        }
    }

    #[test]
    fn an_embedded_track_is_found_by_its_place_among_the_files_own_streams() {
        // Stream indexes 2, 5 and 7: the third is the file's third subtitle
        // stream, whatever its stream index, and that is how the player
        // counts it.
        let tracks = [embedded(2), embedded(5), embedded(7), sidecar(Some("s1"))];
        let source = MediaSourceBuilder::new("f").build();

        let start = start_tracks(&source, &tracks, Some(2));

        assert_eq!(
            start.subtitle,
            Some(StartSubtitle::Embedded { position: 2 })
        );
    }

    #[test]
    fn a_sidecar_is_found_by_its_identifier() {
        let tracks = [embedded(2), sidecar(Some("s1")), sidecar(Some("s2"))];
        let source = MediaSourceBuilder::new("f").build();

        let start = start_tracks(&source, &tracks, Some(2));

        assert_eq!(
            start.subtitle,
            Some(StartSubtitle::Sidecar {
                sidecar_id: "s2".to_owned()
            })
        );
    }

    #[test]
    fn an_index_the_list_does_not_hold_starts_without_subtitles() {
        let tracks = [embedded(2)];
        let source = MediaSourceBuilder::new("f").build();

        assert_eq!(start_tracks(&source, &tracks, Some(1)).subtitle, None);
        assert_eq!(start_tracks(&source, &tracks, None).subtitle, None);
        assert_eq!(
            start_tracks(&source, &[sidecar(None)], Some(0)).subtitle,
            None,
            "a sidecar the player cannot name is not guessed at"
        );
    }

    #[test]
    fn the_audio_track_is_the_one_the_source_names() {
        let mut source = MediaSourceBuilder::new("f").build();
        source.default_audio_track_index = Some(1);

        assert_eq!(start_tracks(&source, &[], None).audio_index, Some(1));

        source.default_audio_track_index = None;
        assert_eq!(start_tracks(&source, &[], None).audio_index, None);
    }
}
