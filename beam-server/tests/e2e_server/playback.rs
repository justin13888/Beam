//! Playing: progress, watched state and resuming across sources, next-up and
//! last-part completion (#188), and subtitle delivery, as stored and as
//! WebVTT (#189).

use beam_test_support::e2e::client::api;
use uuid::Uuid;

use crate::common::{Admin, Video, assert_refused, continue_watching, raw_get, report, show_of};

/// The viewer's state for `title`.
async fn progress(admin: &Admin, title: Uuid) -> api::types::UserTitleState {
    admin
        .api
        .get_title_progress(title)
        .await
        .expect("read the title's progress")
        .into_inner()
}

/// Progress is kept per title, whichever source it was reported on; the end
/// of the file plays the title, a rewind never unplays it, and marking it
/// watched or unwatched is the viewer's own say.
#[tokio::test]
async fn progress_follows_the_title_across_sources_and_watched_state_sticks() {
    let mut admin = Admin::start().await;
    let tree = &mut admin.stack.library;
    tree.add_video("films/Arrival (2016)/Arrival (2016).mp4", &Video::new(440));
    tree.add_video(
        "films/Arrival (2016)/Arrival (2016) - Director's Cut.mp4",
        &Video::new(550),
    );
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;
    let arrival = admin.title("Arrival").await.id;
    let sources = admin
        .api
        .get_media_sources(arrival)
        .await
        .expect("read Arrival's sources")
        .into_inner()
        .items;
    assert_eq!(sources.len(), 2, "two editions, two sources");
    assert!(sources[0].is_primary && !sources[1].is_primary);
    assert_eq!(sources[1].edition.as_deref(), Some("Director's Cut"));
    let (primary, cut) = (sources[0].file_id, sources[1].file_id);

    let untouched = progress(&admin, arrival).await;
    assert!(!untouched.played);
    assert_eq!(
        (
            untouched.position_secs,
            untouched.play_count,
            untouched.last_file_id
        ),
        (0.0, 0, None)
    );

    // Started on the cut: the title resumes there, not on the primary.
    report(&admin.api, cut, 2.0, Some(6.0)).await;
    let state = progress(&admin, arrival).await;
    assert_eq!((state.position_secs, state.last_file_id), (2.0, Some(cut)));
    let shelf = continue_watching(&admin.api).await;
    assert_eq!(shelf.len(), 1);
    assert_eq!(shelf[0].reason, api::types::ContinueWatchingReason::Resume);
    assert_eq!((shelf[0].media_id, shelf[0].file_id), (arrival, cut));
    assert_eq!(shelf[0].position_secs, 2.0);

    // Carried on with the primary: one title, one position, now there.
    report(&admin.api, primary, 3.0, Some(6.0)).await;
    let state = progress(&admin, arrival).await;
    assert_eq!(
        (state.position_secs, state.last_file_id),
        (3.0, Some(primary))
    );

    // A position past the end is refused, and changes nothing.
    assert_refused(
        "a position far past the end",
        422,
        admin
            .api
            .report_playback_progress(
                primary,
                &api::types::ReportProgressRequest {
                    position_secs: 60.0,
                    duration_secs: Some(6.0),
                },
            )
            .await,
    );
    assert_eq!(progress(&admin, arrival).await.position_secs, 3.0);

    // The end: played, back to the start, off the shelf.
    let finished = report(&admin.api, primary, 5.8, Some(6.0)).await;
    assert!(finished.played, "95% of the way is the end");
    assert_eq!((finished.position_secs, finished.play_count), (0.0, 1));
    assert!(continue_watching(&admin.api).await.is_empty());

    // A rewind is a rewatch, not an unplay.
    let rewatch = report(&admin.api, primary, 1.0, Some(6.0)).await;
    assert!(rewatch.played);
    assert_eq!(rewatch.position_secs, 1.0);
    assert_eq!(continue_watching(&admin.api).await.len(), 1);

    admin
        .api
        .mark_unwatched(arrival)
        .await
        .expect("mark Arrival unwatched");
    let cleared = progress(&admin, arrival).await;
    assert!(!cleared.played);
    assert_eq!((cleared.position_secs, cleared.play_count), (0.0, 0));
    admin
        .api
        .mark_watched(arrival)
        .await
        .expect("mark Arrival watched");
    let marked = progress(&admin, arrival).await;
    assert!(marked.played);
    assert_eq!(marked.play_count, 1);

    let history = admin
        .api
        .get_history(api::GetHistoryParams {
            first: None,
            after: None,
        })
        .await
        .expect("read the history")
        .into_inner()
        .items;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].media_id, arrival);

    // Another viewer's state is their own.
    let viewer = admin
        .stack
        .client(&admin.stack.sign_in(crate::common::VIEWER).await);
    let theirs = viewer
        .get_title_progress(arrival)
        .await
        .expect("the viewer reads their own progress")
        .into_inner();
    assert!(!theirs.played);

    admin.finish().await;
}

/// Finishing an episode offers the next one, from its start; finishing the
/// first part of a multi-part movie only resumes it in that part, and the
/// last part's end is what plays the movie.
#[tokio::test]
async fn an_episode_offers_the_next_and_only_the_last_part_finishes_a_movie() {
    let mut admin = Admin::start().await;
    let tree = &mut admin.stack.library;
    tree.add_video(
        "shows/Severance/Season 01/Severance - S01E01.mp4",
        &Video::new(440),
    );
    tree.add_video(
        "shows/Severance/Season 01/Severance - S01E02.mp4",
        &Video::new(550),
    );
    tree.add_video("films/Heat (1995)/Heat (1995) - CD1.mp4", &Video::new(660));
    tree.add_video("films/Heat (1995)/Heat (1995) - CD2.mp4", &Video::new(770));
    let shows = admin.add_library("Shows", "shows").await;
    let films = admin.add_library("Films", "films").await;
    admin.scan_ok(shows).await;
    admin.scan_ok(films).await;

    let severance = admin.title("Severance").await.id;
    let show = show_of(
        admin
            .api
            .get_media_detail(severance)
            .await
            .expect("read Severance")
            .into_inner(),
    );
    let episodes = &show.seasons[0].episodes;
    assert_eq!(episodes.len(), 2);
    let (e01, e02) = (&episodes[0], &episodes[1]);
    let e01_file = e01.file_id.expect("E01 has a file");
    let e02_file = e02.file_id.expect("E02 has a file");

    let finished = report(&admin.api, e01_file, 5.9, Some(6.0)).await;
    assert!(finished.played);
    let shelf = continue_watching(&admin.api).await;
    assert_eq!(shelf.len(), 1, "{shelf:?}");
    let next = &shelf[0];
    assert_eq!(next.reason, api::types::ContinueWatchingReason::NextUp);
    assert_eq!(
        (next.media_id, next.episode_id, next.file_id),
        (severance, Some(e02.id), e02_file)
    );
    assert_eq!(next.position_secs, 0.0, "the next episode from its start");
    assert_eq!(next.media_type, api::types::TitleKind::Show);

    let heat = admin.title("Heat").await.id;
    let parts: Vec<Uuid> = admin
        .api
        .get_media_sources(heat)
        .await
        .expect("read Heat's sources")
        .into_inner()
        .items
        .remove(0)
        .parts
        .into_iter()
        .map(|part| part.file_id)
        .collect();
    assert_eq!(parts.len(), 2);

    let first_part = report(&admin.api, parts[0], 5.9, Some(6.0)).await;
    assert!(!first_part.played, "the end of part 1 is not the movie's");
    assert_eq!(first_part.position_secs, 5.9);
    let shelf = continue_watching(&admin.api).await;
    let resume = shelf
        .iter()
        .find(|item| item.media_id == heat)
        .expect("Heat is being watched");
    assert_eq!(resume.reason, api::types::ContinueWatchingReason::Resume);
    assert_eq!(
        resume.file_id, parts[0],
        "resumed in the part it was left in"
    );

    let last_part = report(&admin.api, parts[1], 5.8, Some(6.0)).await;
    assert!(last_part.played, "the last part's end plays the movie");
    assert_eq!(last_part.play_count, 1);
    assert!(
        continue_watching(&admin.api)
            .await
            .iter()
            .all(|item| item.media_id != heat),
        "a played movie leaves the shelf"
    );

    admin.finish().await;
}

const SRT: &[u8] = b"1\n00:00:01,000 --> 00:00:02,500\nHello\n\n";
const SRT_AS_WEBVTT: &str = "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\nHello\n";

/// Subtitle files beside a video are its tracks: listed with the language
/// and flags their names give, served exactly as stored, and converted from
/// SubRip to the WebVTT a browser reads -- never without a session.
#[tokio::test]
async fn subtitle_files_are_served_as_stored_and_as_webvtt() {
    let mut admin = Admin::start().await;
    let tree = &mut admin.stack.library;
    tree.add_video("films/Arrival (2016)/Arrival (2016).mp4", &Video::new(440));
    tree.write("films/Arrival (2016)/Arrival (2016).en.srt", SRT);
    tree.write(
        "films/Arrival (2016)/Arrival (2016).eng.forced.srt",
        b"1\n00:00:03,000 --> 00:00:04,000\n<i>Forced</i>\n\n",
    );
    let library = admin.add_library("Films", "films").await;
    admin.scan_ok(library).await;
    let arrival = admin.title("Arrival").await.id;
    let source = admin
        .api
        .get_media_sources(arrival)
        .await
        .expect("read Arrival's sources")
        .into_inner()
        .items
        .remove(0);

    let tracks: Vec<(api::types::SubtitleOrigin, &str, Option<&str>, bool)> = source
        .subtitle_tracks
        .iter()
        .map(|track| {
            (
                track.origin.clone(),
                track.codec.as_str(),
                track.language.as_deref(),
                track.is_forced,
            )
        })
        .collect();
    assert_eq!(
        tracks,
        [
            (
                api::types::SubtitleOrigin::Sidecar,
                "subrip",
                Some("eng"),
                false
            ),
            (
                api::types::SubtitleOrigin::Sidecar,
                "subrip",
                Some("eng"),
                true
            ),
        ],
        "the two files beside the video, unforced first"
    );
    let track = &source.subtitle_tracks[0];
    assert!(track.is_text);
    let stored_url = track.url.clone().expect("a sidecar has a URL");
    let webvtt_url = track.webvtt_url.clone().expect("a SubRip file converts");

    let base = admin.stack.base_url().to_owned();
    let stored = raw_get(&base, &stored_url, Some(&admin.session), None).await;
    assert_eq!(stored.status().as_u16(), 200);
    assert_eq!(
        stored
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/x-subrip")
    );
    assert_eq!(stored.bytes().await.expect("the body").as_ref(), SRT);

    let ranged = raw_get(&base, &stored_url, Some(&admin.session), Some("bytes=0-0")).await;
    assert_eq!(ranged.status().as_u16(), 206);
    assert_eq!(ranged.bytes().await.expect("the body").as_ref(), b"1");

    let webvtt = raw_get(&base, &webvtt_url, Some(&admin.session), None).await;
    assert_eq!(webvtt.status().as_u16(), 200);
    assert_eq!(
        webvtt
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/vtt; charset=utf-8")
    );
    assert_eq!(webvtt.text().await.expect("the body"), SRT_AS_WEBVTT);

    let anonymous = raw_get(&base, &webvtt_url, None, None).await;
    assert_eq!(
        anonymous.status().as_u16(),
        401,
        "subtitles are never served without a session"
    );

    admin.finish().await;
}
