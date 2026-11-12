//! Language preferences (`/v1/me/language-preferences`, #264): stored per
//! viewer in SQL, and read by `GET /v1/media/{id}/sources` to name the
//! subtitle track each viewer starts on.

use beam_test_support::e2e::client::api;
use uuid::Uuid;

use crate::common::{Admin, VIEWER, Video, assert_refused, first_source};

const FRENCH: &[u8] = b"1\n00:00:01,000 --> 00:00:02,000\nBonjour\n\n";
const ENGLISH: &[u8] = b"1\n00:00:01,000 --> 00:00:02,000\nHello\n\n";

/// The language of the subtitle track `client` is told to start `title` on.
async fn chosen_subtitle(client: &api::Client, title: Uuid) -> Option<String> {
    let source = first_source(client, title).await;
    let index = source.default_subtitle_track_index?;
    let position = usize::try_from(index).expect("a track index fits");
    let track = source
        .subtitle_tracks
        .get(position)
        .unwrap_or_else(|| panic!("index {index} names no track of {source:?}"));
    Some(
        track
            .language
            .clone()
            .expect("each sidecar names its language"),
    )
}

fn settings(
    subtitle_languages: &[&str],
    mode: api::types::SubtitleMode,
) -> api::types::LanguagePreferenceSettings {
    api::types::LanguagePreferenceSettings {
        audio_languages: Vec::new(),
        subtitle_languages: subtitle_languages.iter().map(|&l| l.to_owned()).collect(),
        subtitle_mode: mode,
    }
}

/// A viewer's preferences are stored as the codes tracks are tagged with,
/// read back over SQL, and decide which of a title's subtitle files their
/// sources start on -- theirs alone: another viewer's sources do not move.
#[tokio::test]
async fn language_preferences_are_stored_per_viewer_and_choose_the_subtitle_track() {
    let mut admin = Admin::start().await;
    let tree = &mut admin.stack.library;
    tree.add_video("films/Amelie (2001)/Amelie (2001).mp4", &Video::new(440));
    tree.write("films/Amelie (2001)/Amelie (2001).fr.srt", FRENCH);
    tree.write("films/Amelie (2001)/Amelie (2001).en.srt", ENGLISH);
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;
    let amelie = admin.title("Amelie").await.id;
    let tracks: Vec<Option<String>> = first_source(&admin.api, amelie)
        .await
        .subtitle_tracks
        .into_iter()
        .map(|track| track.language)
        .collect();
    assert_eq!(tracks.len(), 2, "both sidecars are tracks: {tracks:?}");

    let viewer = admin.stack.client(&admin.stack.sign_in(VIEWER).await);
    let admins_choice = chosen_subtitle(&admin.api, amelie).await;

    // Stored as ISO 639-2/B, whatever form each language was sent in, and
    // read back as stored.
    let stored = viewer
        .set_language_preferences(&settings(&["fr", "English"], api::types::SubtitleMode::On))
        .await
        .expect("set the viewer's preferences")
        .into_inner();
    assert_eq!(stored.subtitle_languages, ["fre", "eng"]);
    assert_eq!(stored.subtitle_mode, api::types::SubtitleMode::On);
    let read_back = viewer
        .get_language_preferences()
        .await
        .expect("read the viewer's preferences")
        .into_inner();
    assert_eq!(
        (read_back.subtitle_languages, read_back.subtitle_mode),
        (
            vec!["fre".to_owned(), "eng".to_owned()],
            api::types::SubtitleMode::On
        )
    );
    assert_eq!(
        chosen_subtitle(&viewer, amelie).await.as_deref(),
        Some("fre"),
        "the first preferred language the title has"
    );

    viewer
        .set_language_preferences(&settings(&["ger", "eng"], api::types::SubtitleMode::On))
        .await
        .expect("change the viewer's preferences");
    assert_eq!(
        chosen_subtitle(&viewer, amelie).await.as_deref(),
        Some("eng"),
        "a preferred language the title lacks is passed over"
    );

    viewer
        .set_language_preferences(&settings(&["fre"], api::types::SubtitleMode::Off))
        .await
        .expect("turn the viewer's subtitles off");
    assert_eq!(
        chosen_subtitle(&viewer, amelie).await,
        None,
        "subtitles off"
    );

    // A language Beam cannot match a track against is refused, and the
    // preferences stand as they were.
    assert_refused(
        "a language that is not one",
        422,
        viewer
            .set_language_preferences(&settings(&["not a language"], api::types::SubtitleMode::On))
            .await,
    );
    let unchanged = viewer
        .get_language_preferences()
        .await
        .expect("read the viewer's preferences")
        .into_inner();
    assert_eq!(
        (unchanged.subtitle_languages, unchanged.subtitle_mode),
        (vec!["fre".to_owned()], api::types::SubtitleMode::Off)
    );

    // The administrator set nothing, and starts where they always did.
    assert_eq!(chosen_subtitle(&admin.api, amelie).await, admins_choice);

    admin.finish().await;
}
