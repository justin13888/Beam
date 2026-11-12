//! Subcutaneous tests for the continue-watching shelf (issue #188):
//! `GET /v1/continue-watching` and `DELETE /v1/continue-watching/{id}`.
//!
//! The same real playback and metadata services over in-memory stores as
//! `playback_tests.rs`, whose fixture these share.

use beam_domain::models::file::MediaFileContent;
use beam_domain::repositories::FileRepository;
use kynos::http::StatusCode;
use kynos::test::TestClient;
use uuid::Uuid;

use super::playback_tests::{
    Fixture, client, continue_watching, fixture, progress, session, set_watched, watch,
};
use crate::models::TitleKind;
use crate::models::playback::{ContinueWatchingConnection, ContinueWatchingReason};
use crate::services::playback::CANDIDATE_CEILING;
use crate::state::AppState;

#[tokio::test]
async fn a_finished_episode_offers_the_next_with_what_the_row_displays() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (_, f1) = fixture.episode(show, 1, 1).await;
    let (e2, _) = fixture.episode(show, 1, 2).await;
    // A second, larger source of E2: the primary, which a next-up plays.
    let e2_primary = fixture
        .file(MediaFileContent::episode(e2), 9_000, 100)
        .await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, f1, 99.0).await;

    let shelf = continue_watching(&client, &token).await;
    assert_eq!(shelf.items.len(), 1);
    let item = &shelf.items[0];
    assert_eq!(item.reason, ContinueWatchingReason::NextUp);
    assert_eq!((item.media_id, item.media_type), (show, TitleKind::Show));
    assert_eq!(item.episode_id, Some(e2));
    assert_eq!(item.file_id, e2_primary);
    assert_eq!(item.position_secs, 0.0);
    assert_eq!(
        (item.season_number, item.episode_number),
        (Some(1), Some(2))
    );
    assert_eq!(item.episode_title.as_deref(), Some("Episode 2"));
    assert!(item.title.starts_with("Dark"));
    assert_eq!(
        item.poster_url.as_deref(),
        Some(format!("/v1/artwork/show/{show}/poster").as_str())
    );
    assert!(!shelf.page_info.has_next_page);
}

#[tokio::test]
async fn a_show_is_one_row_however_many_episodes_are_started() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let movie = fixture.movie("Heat").await;
    let movie_file = fixture.movie_file(movie, 1_000).await;
    let (_, f1) = fixture.episode(show, 1, 1).await;
    let (e2, f2) = fixture.episode(show, 1, 2).await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, f1, 30.0).await;
    fixture.later();
    watch(&client, &token, movie_file, 30.0).await;
    fixture.later();
    watch(&client, &token, f2, 50.0).await;

    let shelf = continue_watching(&client, &token).await;
    let rows: Vec<(Uuid, Option<Uuid>, f64)> = shelf
        .items
        .iter()
        .map(|i| (i.media_id, i.episode_id, i.position_secs))
        .collect();
    assert_eq!(
        rows,
        vec![(show, Some(e2), 50.0), (movie, None, 30.0)],
        "the show once, at the episode touched last, newest first"
    );
}

#[tokio::test]
async fn next_up_steps_over_an_episode_with_no_file_and_ends_with_the_show() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (_, f1) = fixture.episode(show, 1, 1).await;
    fixture.bare_episode(show, 1, 2).await;
    let (e3, f3) = fixture.episode(show, 1, 3).await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, f1, 99.0).await;
    assert_eq!(
        continue_watching(&client, &token).await.items[0].episode_id,
        Some(e3)
    );

    fixture.later();
    watch(&client, &token, f3, 99.0).await;
    assert!(continue_watching(&client, &token).await.items.is_empty());
}

#[tokio::test]
async fn a_missing_last_file_falls_back_to_the_primary() {
    let fixture = fixture();
    let movie = fixture.movie("Heat").await;
    let small = fixture.movie_file(movie, 1_000).await;
    let large = fixture.movie_file(movie, 4_000).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    watch(&client, &token, small, 30.0).await;

    fixture
        .files
        .mark_missing(vec![small], chrono::Utc::now())
        .await
        .unwrap();

    let shelf = continue_watching(&client, &token).await;
    assert_eq!(shelf.items[0].file_id, large);
    assert_eq!(shelf.items[0].position_secs, 30.0);

    fixture
        .files
        .mark_missing(vec![large], chrono::Utc::now())
        .await
        .unwrap();
    assert!(
        continue_watching(&client, &token).await.items.is_empty(),
        "nothing left to play it from"
    );
}

#[tokio::test]
async fn a_dismissed_title_stays_away_until_it_is_played_again() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (e1, f1) = fixture.episode(show, 1, 1).await;
    let (_, f2) = fixture.episode(show, 1, 2).await;
    let client = client(&fixture);
    let token = session(&fixture).await;
    watch(&client, &token, f1, 30.0).await;

    // An episode id dismisses its show.
    let response = client
        .delete(&format!("/v1/continue-watching/{e1}"))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(continue_watching(&client, &token).await.items.is_empty());
    assert_eq!(
        progress(&client, &token, e1).await.position_secs,
        30.0,
        "dismissing keeps the resume point"
    );

    fixture.later();
    watch(&client, &token, f2, 10.0).await;
    assert_eq!(continue_watching(&client, &token).await.items.len(), 1);

    let unknown = client
        .delete(&format!("/v1/continue-watching/{}", Uuid::new_v4()))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_multi_episode_file_watched_to_its_end_plays_its_whole_run() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let e1 = fixture.bare_episode(show, 1, 1).await;
    let e2 = fixture.bare_episode(show, 1, 2).await;
    let e3 = fixture.bare_episode(show, 1, 3).await;
    let (e4, _) = fixture.episode(show, 1, 4).await;
    let run = fixture
        .file(
            MediaFileContent::Episode {
                episode_id: e1,
                last_episode_number: Some(3),
            },
            1_000,
            100,
        )
        .await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, run, 98.0).await;

    for episode in [e1, e2, e3] {
        assert!(progress(&client, &token, episode).await.played, "{episode}");
    }
    assert!(!progress(&client, &token, e4).await.played);
    assert_eq!(
        continue_watching(&client, &token).await.items[0].episode_id,
        Some(e4),
        "next up is after the run"
    );
}

/// Going back to an earlier episode does not lose the one left part-way
/// after it: when next-up lands on it, the row resumes it, on the source it
/// was being played from (FR-507).
#[tokio::test]
async fn next_up_onto_an_episode_under_way_resumes_it_from_its_source() {
    let fixture = fixture();
    let show = fixture.show("Dark").await;
    let (_, f1) = fixture.episode(show, 1, 1).await;
    let (e2, f2) = fixture.episode(show, 1, 2).await;
    // A second, larger source of E2: the primary, which the viewer did not
    // choose.
    let e2_primary = fixture
        .file(MediaFileContent::episode(e2), 9_000, 100)
        .await;
    let client = client(&fixture);
    let token = session(&fixture).await;

    watch(&client, &token, f2, 60.0).await;
    fixture.later();
    // Back to E1, watched to its end: the anchor, and E2 is next.
    watch(&client, &token, f1, 99.0).await;

    let shelf = continue_watching(&client, &token).await;
    assert_eq!(shelf.items.len(), 1);
    let item = &shelf.items[0];
    assert_eq!(item.episode_id, Some(e2));
    assert_eq!(item.reason, ContinueWatchingReason::Resume);
    assert_eq!(item.position_secs, 60.0, "the resume point stands");
    assert_eq!(item.duration_secs, Some(100.0));
    assert_eq!(
        item.file_id, f2,
        "the source it was played from, not the primary ({e2_primary})"
    );

    // With no position to resume, the same episode is offered from the start.
    let fresh = session(&fixture).await;
    watch(&client, &fresh, f1, 99.0).await;
    let item = &continue_watching(&client, &fresh).await.items[0];
    assert_eq!(
        (
            item.reason,
            item.episode_id,
            item.position_secs,
            item.file_id
        ),
        (ContinueWatchingReason::NextUp, Some(e2), 0.0, e2_primary)
    );
}

#[tokio::test]
async fn continue_watching_is_bounded_by_first() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    for title in ["A", "B", "C"] {
        let movie = fixture.movie(title).await;
        let file = fixture.movie_file(movie, 1_000).await;
        watch(&client, &token, file, 10.0).await;
        fixture.later();
    }

    let response = client
        .get("/v1/continue-watching?first=2")
        .cookie("beam_session", &token)
        .send()
        .await;
    let shelf: ContinueWatchingConnection = response.json();
    assert_eq!(shelf.items.len(), 2);
    assert!(
        shelf.page_info.has_next_page,
        "a title past a full shelf may offer a row"
    );
    let whole = continue_watching(&client, &token).await;
    assert_eq!(whole.items.len(), 3);
    assert!(!whole.page_info.has_next_page, "every candidate was read");
    for first in ["0", "51"] {
        let response = client
            .get(&format!("/v1/continue-watching?first={first}"))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "first={first}");
    }
}

/// Mark `count` new shows of one episode each watched, all after everything
/// played so far.
async fn caught_up_shows(
    fixture: &Fixture,
    client: &TestClient<AppState>,
    token: &str,
    count: u64,
) {
    fixture.later();
    for n in 0..count {
        let show = fixture
            .show(&format!("Caught up {n} {}", Uuid::new_v4()))
            .await;
        fixture.episode(show, 1, 1).await;
        assert_eq!(
            set_watched(client, token, show, true).await,
            StatusCode::NO_CONTENT
        );
    }
}

/// A bulk mark makes caught-up shows by the hundred, each played more
/// recently than anything in progress. They are read past, a page of
/// candidates after another, not counted against the shelf.
#[tokio::test]
async fn caught_up_shows_newer_than_an_unfinished_title_never_crowd_it_out() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture.movie("Heat").await;
    let file = fixture.movie_file(movie, 1_000).await;
    watch(&client, &token, file, 30.0).await;
    // More than one page of candidates, so the movie is found by reading on.
    caught_up_shows(&fixture, &client, &token, 250).await;

    for first in [1, 2] {
        let response = client
            .get(&format!("/v1/continue-watching?first={first}"))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{}", response.text());
        let shelf: ContinueWatchingConnection = response.json();
        let rows: Vec<Uuid> = shelf.items.iter().map(|i| i.media_id).collect();
        assert_eq!(rows, vec![movie], "first={first}");
        assert!(!shelf.page_info.has_next_page, "first={first}");
    }
}

/// However many caught-up shows a viewer has, one load examines at most
/// [`CANDIDATE_CEILING`] titles. A title just inside it is listed; one just
/// past it is not, and the shelf says more may exist.
#[tokio::test]
async fn a_load_examines_candidates_up_to_the_ceiling_and_says_when_more_may_exist() {
    let fixture = fixture();
    let client = client(&fixture);
    let token = session(&fixture).await;
    let movie = fixture.movie("Heat").await;
    let file = fixture.movie_file(movie, 1_000).await;
    watch(&client, &token, file, 30.0).await;
    caught_up_shows(&fixture, &client, &token, CANDIDATE_CEILING - 1).await;

    let inside = continue_watching(&client, &token).await;
    let rows: Vec<Uuid> = inside.items.iter().map(|i| i.media_id).collect();
    assert_eq!(rows, vec![movie], "the last title the ceiling reaches");
    assert!(!inside.page_info.has_next_page);

    caught_up_shows(&fixture, &client, &token, 1).await;
    let past = continue_watching(&client, &token).await;
    assert!(past.items.is_empty(), "the movie is past the ceiling");
    assert!(
        past.page_info.has_next_page,
        "a title past the ceiling may offer a row"
    );
    assert_eq!(
        progress(&client, &token, movie).await.position_secs,
        30.0,
        "past the ceiling, the title keeps its place"
    );
}
