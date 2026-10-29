use super::*;

/// What `GET /v1/media/{id}` answers for [`MEDIA_ID`].
const MOVIE_DETAIL: &str = r#"{"Movie":{"id":"00000000-0000-0000-0000-000000000007","title":{"original":"Le Samourai"},"genres":[],"user_state":{"played":false,"position_secs":0.0,"play_count":0}}}"#;

/// The same movie, once the viewer has marked it watched.
const WATCHED_MOVIE_DETAIL: &str = r#"{"Movie":{"id":"00000000-0000-0000-0000-000000000007","title":{"original":"Le Samourai"},"genres":[],"user_state":{"played":true,"position_secs":0.0,"play_count":1}}}"#;

/// Marking a title watched and reading it back shows it watched: the mark
/// is a `PUT` on the title's `watched`, and the read after it reaches the
/// server rather than a copy taken before the mark (issue #188).
#[tokio::test]
async fn a_watched_mark_shows_on_the_next_detail_read() {
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering_in_turn(vec![
        (200, "application/json", MOVIE_DETAIL),
        (204, "application/json", ""),
        (200, "application/json", WATCHED_MOVIE_DETAIL),
    ]));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let before = client
        .media_detail(MEDIA_ID.to_owned())
        .await
        .expect("the canned detail is served");
    client
        .mark_watched(MEDIA_ID.to_owned())
        .await
        .expect("the canned 204 accepts the mark");
    let after = client
        .media_detail(MEDIA_ID.to_owned())
        .await
        .expect("the canned detail is served again");

    assert!(!before.summary().played);
    assert!(after.summary().played, "the read after the mark is fresh");
    let recorded = backend.recorded();
    assert_eq!(recorded[1].method, reqwest::Method::PUT);
    assert_eq!(
        recorded[1].url.path(),
        format!("/v1/media/{MEDIA_ID}/watched")
    );
    assert_eq!(recorded[2].method, reqwest::Method::GET);
}

#[tokio::test]
async fn an_unwatched_mark_is_a_delete_of_the_titles_watched_state() {
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering(204, "application/json", ""));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    client
        .mark_unwatched(MEDIA_ID.to_owned())
        .await
        .expect("the canned 204 accepts the mark");

    let recorded = backend.recorded();
    assert_eq!(recorded[0].method, reqwest::Method::DELETE);
    assert_eq!(
        recorded[0].url.path(),
        format!("/v1/media/{MEDIA_ID}/watched")
    );
    assert!(matches!(
        client.mark_watched("not-a-uuid".to_owned()).await,
        Err(BeamError::BadRequest { .. })
    ));
}

const SHOW_ID: &str = "5a5a5a5a-0000-4000-8000-000000000001";
const CURRENT_EPISODE: &str = "e0000001-0000-4000-8000-000000000001";
const NEXT_EPISODE: &str = "e0000005-0000-4000-8000-000000000005";

/// `GET /v1/episodes/{id}` for an episode of `show` whose next-up is
/// `next` (JSON `null` for none).
fn episode_detail(episode: &str, show: &str, next: &str) -> String {
    format!(
        r#"{{"episode":{{"id":"{episode}","episode_number":4,"title":"Return","source_count":1,
            "user_state":{{"played":false,"position_secs":0.0,"play_count":0}},
            "file_id":"f4f4f4f4-0000-4000-8000-000000000004"}},
           "season_id":"5e5e5e5e-0000-4000-8000-000000000001","season_number":2,
           "show":{{"id":"{show}","title":{{"original":"Dark"}}}},
           "next_episode_id":{next}}}"#
    )
}

/// Auto-advance follows the server's `next_episode_id`, and returns the
/// next episode as the server describes it.
#[tokio::test]
async fn up_next_is_the_episode_the_server_names_next() {
    let current: &'static str = Box::leak(
        episode_detail(CURRENT_EPISODE, SHOW_ID, &format!("\"{NEXT_EPISODE}\"")).into_boxed_str(),
    );
    let next: &'static str =
        Box::leak(episode_detail(NEXT_EPISODE, SHOW_ID, "null").into_boxed_str());
    let (client, backend) = client_answering_in_turn(vec![
        (200, "application/json", current),
        (200, "application/json", next),
    ])
    .await;

    let episode = client
        .up_next_in_show(SHOW_ID.to_owned(), CURRENT_EPISODE.to_owned())
        .await
        .expect("both episodes decode")
        .expect("the server names a next episode");

    assert_eq!(episode.id, NEXT_EPISODE);
    assert_eq!(
        episode.file_id.as_deref(),
        Some("f4f4f4f4-0000-4000-8000-000000000004")
    );
    let paths: Vec<String> = backend
        .recorded()
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert_eq!(
        paths,
        vec![
            format!("/v1/episodes/{CURRENT_EPISODE}"),
            format!("/v1/episodes/{NEXT_EPISODE}"),
        ]
    );
}

#[tokio::test]
async fn there_is_no_up_next_after_the_last_episode() {
    let last: &'static str =
        Box::leak(episode_detail(CURRENT_EPISODE, SHOW_ID, "null").into_boxed_str());
    let (client, backend) = client_answering_in_turn(vec![(200, "application/json", last)]).await;

    let episode = client
        .up_next_in_show(SHOW_ID.to_owned(), CURRENT_EPISODE.to_owned())
        .await
        .expect("the episode decodes");

    assert_eq!(episode, None);
    assert_eq!(backend.recorded().len(), 1, "nothing to look up next");
}

#[tokio::test]
async fn up_next_never_crosses_into_another_show() {
    let elsewhere: &'static str = Box::leak(
        episode_detail(
            CURRENT_EPISODE,
            "0f0f0f0f-0000-4000-8000-00000000000f",
            &format!("\"{NEXT_EPISODE}\""),
        )
        .into_boxed_str(),
    );
    let (client, backend) =
        client_answering_in_turn(vec![(200, "application/json", elsewhere)]).await;

    let episode = client
        .up_next_in_show(SHOW_ID.to_owned(), CURRENT_EPISODE.to_owned())
        .await
        .expect("the episode decodes");

    assert_eq!(episode, None);
    assert_eq!(backend.recorded().len(), 1);
}

/// A library file, as `GET /v1/libraries/{id}/files` lists one.
macro_rules! library_file {
    ($id:literal, $path:literal) => {
        concat!(
            r#"{"id":""#,
            $id,
            r#"","library_id":"00000000-0000-0000-0000-00000000000a","path":""#,
            $path,
            r#"","size_bytes":1,"hash":"1","status":"known","content_type":"movie","scanned_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#
        )
    };
}

/// The server pages a library's files; the core walks every page, each
/// request after the first carrying the previous page's `end_cursor`, and
/// returns the whole list in order.
#[tokio::test]
async fn a_library_s_files_are_walked_page_by_page() {
    let (client, id, _) = signed_in_client().await;
    let first_page: &'static str = concat!(
        r#"{"items":["#,
        library_file!("00000000-0000-0000-0000-000000000001", "a.mkv"),
        r#"],"page_info":{"has_next_page":true,"has_previous_page":false,"start_cursor":"c1","end_cursor":"c1"}}"#
    );
    let last_page: &'static str = concat!(
        r#"{"items":["#,
        library_file!("00000000-0000-0000-0000-000000000002", "b.mkv"),
        r#"],"page_info":{"has_next_page":false,"has_previous_page":true,"start_cursor":"c2","end_cursor":"c2"}}"#
    );
    let backend = Arc::new(CannedBackend::answering_in_turn(vec![
        (200, "application/json", first_page),
        (200, "application/json", last_page),
    ]));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let files = client
        .library_files("00000000-0000-0000-0000-00000000000a".to_owned())
        .await
        .expect("both pages decode");

    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, vec!["a.mkv", "b.mkv"]);
    let queries: Vec<String> = backend
        .recorded()
        .iter()
        .map(|request| request.url.query().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(queries.len(), 2, "one request per page: {queries:?}");
    assert!(!queries[0].contains("after="), "{}", queries[0]);
    assert!(queries[1].contains("after=c1"), "{}", queries[1]);
    assert!(
        queries.iter().all(|query| query.contains("first=500")),
        "{queries:?}"
    );
}

#[tokio::test]
async fn genres_are_the_servers_list() {
    let body: &'static str =
        Box::leak(format!(r#"{{"items":["Crime","Drama"],{NO_PAGES}}}"#).into_boxed_str());
    let (client, backend) = one_answer(200, body).await;

    let genres = client.genres().await.expect("answered");

    assert_eq!(genres, vec!["Crime".to_owned(), "Drama".to_owned()]);
    let request = the_request(&backend);
    assert_eq!(request.method, reqwest::Method::GET);
    assert_eq!(request.url.path(), "/v1/genres");
}

#[tokio::test]
async fn libraries_are_every_library_on_the_server() {
    let body: &'static str =
        Box::leak(format!(r#"{{"items":[{LIBRARY}],{NO_PAGES}}}"#).into_boxed_str());
    let (client, backend) = one_answer(200, body).await;

    let libraries = client.libraries().await.expect("answered");

    assert_eq!(libraries.len(), 1);
    assert_eq!(libraries[0].name, "Films");
    assert_eq!(libraries[0].size, 42);
    assert_eq!(libraries[0].last_scan_file_count, Some(40));
    assert_eq!(libraries[0].last_scan_finished_at_unix, Some(1_767_226_200));
    let request = the_request(&backend);
    assert_eq!(request.method, reqwest::Method::GET);
    assert_eq!(request.url.path(), "/v1/libraries");
}

#[tokio::test]
async fn one_library_is_asked_for_by_its_id() {
    let (client, backend) = one_answer(200, LIBRARY).await;

    let library = client
        .library(LIBRARY_ID.to_owned())
        .await
        .expect("answered");

    assert_eq!(library.id, LIBRARY_ID);
    assert_eq!(library.description.as_deref(), Some("Feature films"));
    assert_eq!(
        the_request(&backend).url.path(),
        format!("/v1/libraries/{LIBRARY_ID}")
    );
    assert!(matches!(
        client.library("films".to_owned()).await,
        Err(BeamError::BadRequest { .. })
    ));
    assert_eq!(backend.recorded().len(), 1, "a malformed id is never sent");
}

#[tokio::test]
async fn continue_watching_asks_for_the_limit_and_resolves_its_rows() {
    let (client, backend) = one_answer(
        200,
        r#"{"items":[{"reason":"resume","media_id":"00000000-0000-0000-0000-000000000007",
            "media_type":"movie","file_id":"f1f1f1f1-0000-4000-8000-000000000001",
            "position_secs":600.0,"duration_secs":6000.0,
            "last_played_at":"2026-01-01T00:00:00Z","title":"Le Samourai",
            "poster_url":"/artwork/m/poster.jpg"}],
            "page_info":{"has_next_page":false,"has_previous_page":false}}"#,
    )
    .await;

    let rows = client.continue_watching(Some(5)).await.expect("answered");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].file_id, "f1f1f1f1-0000-4000-8000-000000000001");
    assert_eq!(rows[0].progress_fraction, Some(0.1));
    assert_eq!(rows[0].updated_at_unix, 1_767_225_600);
    let poster = rows[0]
        .media
        .as_ref()
        .and_then(|media| media.poster_url.clone());
    assert_eq!(
        poster.as_deref(),
        Some("https://beam.test/artwork/m/poster.jpg")
    );
    let request = the_request(&backend);
    assert_eq!(request.url.path(), "/v1/continue-watching");
    assert_eq!(
        query_of(&request).get("first").map(String::as_str),
        Some("5")
    );
}

#[tokio::test]
async fn a_history_page_carries_its_cursor_total_and_rows() {
    let (client, backend) = one_answer(
        200,
        r#"{"items":[{"media_id":"00000000-0000-0000-0000-000000000007",
            "media_type":"movie","file_id":"f1f1f1f1-0000-4000-8000-000000000001",
            "position_secs":0.0,"played":true,"play_count":2,
            "last_played_at":"2026-01-01T00:00:00Z","title":"Le Samourai"}],
            "page_info":{"has_next_page":true,"has_previous_page":true,
                         "start_cursor":"c2","end_cursor":"c3"},
            "total":12}"#,
    )
    .await;

    let page = client
        .history(Some(10), Some("c1".to_owned()))
        .await
        .expect("answered");

    assert_eq!(page.total, 12);
    assert_eq!(page.end_cursor.as_deref(), Some("c3"));
    assert!(page.has_next_page);
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].completed);
    let request = the_request(&backend);
    assert_eq!(request.url.path(), "/v1/history");
    let query = query_of(&request);
    assert_eq!(query.get("first").map(String::as_str), Some("10"));
    assert_eq!(query.get("after").map(String::as_str), Some("c1"));
}
