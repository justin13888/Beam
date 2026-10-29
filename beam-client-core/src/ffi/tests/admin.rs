use super::*;

/// `GET /v1/admin/libraries/{id}/scan` for a scan part-way through.
const RUNNING_SCAN: &str = r#"{"id":"5c000000-0000-4000-8000-000000000001",
    "library_id":"00000000-0000-0000-0000-00000000000a","trigger":"manual",
    "state":"running","queued_at":"2026-01-01T00:00:00Z",
    "started_at":"2026-01-01T00:00:01Z","finished_at":null,
    "progress":{"total_count":120,"processed_count":30,"added_count":12,
                "changed_count":3,"unchanged_count":15,"deferred_count":0,
                "failed_count":1,"marked_missing_count":2,"restored_count":0,
                "relinked_count":0,"purged_count":0},
    "failure":null}"#;

#[tokio::test]
async fn a_running_scan_reports_its_progress() {
    let (client, backend) =
        client_answering_in_turn(vec![(200, "application/json", RUNNING_SCAN)]).await;

    let job = client
        .library_scan(LIBRARY_ID.to_owned())
        .await
        .expect("the scan decodes")
        .expect("the library has a scan");

    assert_eq!(job.state, crate::catalog::ScanJobState::Running);
    assert_eq!(job.library_id, LIBRARY_ID);
    assert_eq!((job.processed_count, job.total_count), (30, Some(120)));
    assert_eq!(
        (
            job.added_count,
            job.changed_count,
            job.failed_count,
            job.missing_count
        ),
        (12, 3, 1, 2)
    );
    assert_eq!(job.finished_at_unix, None);
    assert_eq!(
        backend.recorded()[0].url.path(),
        format!("/v1/admin/libraries/{LIBRARY_ID}/scan")
    );
}

/// The server forgets scans when it restarts: that is "not scanned
/// since", an answer, while an unknown library is still a failure.
#[tokio::test]
async fn no_scan_since_the_server_started_is_none_and_an_unknown_library_is_an_error() {
    let (client, _) = client_answering_in_turn(vec![
        (
            404,
            "application/problem+json",
            r#"{"type":"https://beam.justinchung.net/reference/errors/#scan-not-found","status":404,"title":"No scan of this library since the server started"}"#,
        ),
        (
            404,
            "application/problem+json",
            r#"{"type":"https://beam.justinchung.net/reference/errors/#library-not-found","status":404,"title":"Library not found"}"#,
        ),
    ])
    .await;

    assert_eq!(
        client
            .library_scan(LIBRARY_ID.to_owned())
            .await
            .expect("not scanned is an answer"),
        None
    );
    assert!(matches!(
        client.library_scan(LIBRARY_ID.to_owned()).await,
        Err(BeamError::NotFound { .. })
    ));
}

/// A page of the log carries the server's cursor and total through, and
/// the next request sends the cursor it was handed.
#[tokio::test]
async fn a_page_of_the_log_carries_its_cursor_and_total() {
    let (client, id, _) = signed_in_client().await;
    let page: &'static str = r#"{"items":[{"id":"00000000-0000-0000-0000-000000000003","level":"info","category":"system","message":"started","created_at":"2026-01-01T00:00:00Z"}],"page_info":{"has_next_page":true,"has_previous_page":false,"start_cursor":"l0","end_cursor":"l0"},"total":7}"#;
    let backend = Arc::new(CannedBackend::answering(200, "application/json", page));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let logs = client
        .admin_logs(Some(1), Some("l-1".to_owned()))
        .await
        .expect("the page decodes");

    assert_eq!(logs.items.len(), 1);
    assert_eq!(logs.total, 7);
    assert_eq!(logs.end_cursor.as_deref(), Some("l0"));
    assert!(logs.has_next_page);
    let query = backend.recorded()[0]
        .url
        .query()
        .unwrap_or_default()
        .to_owned();
    assert!(
        query.contains("first=1") && query.contains("after=l-1"),
        "{query}"
    );
}

/// A scan is accepted, not finished: the 202 and its job are all the
/// call waits for.
#[tokio::test]
async fn starting_a_scan_returns_once_the_server_accepts_it() {
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering(
        202,
        "application/json",
        r#"{"id":"00000000-0000-0000-0000-000000000001","library_id":"00000000-0000-0000-0000-000000000002","trigger":"manual","state":"queued","queued_at":"2026-09-27T00:00:00Z","started_at":null,"finished_at":null,"progress":{"total_count":null,"processed_count":0,"added_count":0,"changed_count":0,"unchanged_count":0,"deferred_count":0,"failed_count":0,"marked_missing_count":0,"restored_count":0,"relinked_count":0,"purged_count":0},"failure":null}"#,
    ));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    client
        .scan_library("00000000-0000-0000-0000-000000000002".to_owned())
        .await
        .expect("an accepted scan is a success");

    let recorded = backend.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].method, reqwest::Method::POST);
    assert!(
        recorded[0]
            .url
            .path()
            .ends_with("/v1/admin/libraries/00000000-0000-0000-0000-000000000002/scan"),
        "{}",
        recorded[0].url
    );
}

/// A library id that is not a UUID is refused before anything is sent:
/// the route captures a `Uuid`, so the request could only be a 400.
#[tokio::test]
async fn a_scan_of_a_malformed_library_id_is_refused_without_a_request() {
    let (client, id, _) = signed_in_client().await;
    let backend = Arc::new(CannedBackend::answering(202, "application/json", "{}"));
    client
        .use_transport(
            &id,
            Arc::clone(&backend) as Arc<dyn crate::api::HttpBackend>,
        )
        .expect("the server is registered");

    let error = client
        .scan_library("not-a-uuid".to_owned())
        .await
        .expect_err("a malformed id fails the call");

    assert!(
        matches!(&error, BeamError::BadRequest { code, .. } if code == ABOUT_BLANK),
        "{error:?}"
    );
    assert!(backend.recorded().is_empty(), "nothing was sent");
}

/// A scan already running is the server's 409, surfaced with its problem
/// type so a screen can say so rather than report a failure.
#[tokio::test]
async fn a_scan_already_running_is_a_409() {
    let (client, id, _) = signed_in_client().await;
    client
        .use_transport(
            &id,
            Arc::new(CannedBackend::answering(
                409,
                "application/problem+json",
                r#"{"type":"https://beam.justinchung.net/reference/errors/#library-scan-in-progress","status":409,"detail":"A scan of this library is already queued or running"}"#,
            )),
        )
        .expect("the server is registered");

    let error = client
        .scan_library("00000000-0000-0000-0000-000000000002".to_owned())
        .await
        .expect_err("the canned 409 fails the call");

    assert!(
        matches!(
            &error,
            BeamError::Server { status: 409, code, .. }
                if code.ends_with("#library-scan-in-progress")
        ),
        "{error:?}"
    );
}

const ADMIN_STATUS: &str = r#"{"uptime_secs":3600,"version":"1.2.3",
    "counts":{"users":3,"libraries":2,"files":120},
    "enrichment":{"pending":4,"enriched":100,"unmatched":5,"failed":1},
    "enrichment_providers":[{"provider":"tmdb","state":"configured"}],
    "metadata_providers":[{"provider":"tvdb","state":"not_configured","detail":"set a key"}],
    "recent_scans":[{"level":"warning","message":"skipped 2 files",
                     "created_at":"2026-01-01T00:00:00Z"}],
    "watcher":{"enabled":true,"watch_limit_reached":false,"libraries":[]}}"#;

#[tokio::test]
async fn the_admin_status_is_read_and_narrowed() {
    let (client, backend) = one_answer(200, ADMIN_STATUS).await;

    let status = client.admin_status().await.expect("answered");

    assert_eq!(status.version, "1.2.3");
    assert_eq!(status.uptime_secs, 3600);
    assert_eq!(
        (
            status.counts.users,
            status.counts.libraries,
            status.counts.files
        ),
        (3, 2, 120)
    );
    assert_eq!(status.recent_scans.len(), 1);
    assert_eq!(the_request(&backend).url.path(), "/v1/admin/status");
}

#[tokio::test]
async fn a_page_of_admin_users_resolves_avatars_against_the_server() {
    let (client, backend) = one_answer(
        200,
        r#"{"items":[{"id":"00000000-0000-0000-0000-0000000000b1","display_name":"Bea",
            "email":"bea@example.test","avatar_url":"/avatars/b1.png","is_admin":false,
            "disabled":true,"created_at":"2026-01-01T00:00:00Z"}],
            "page_info":{"has_next_page":true,"has_previous_page":false,
                         "start_cursor":"u1","end_cursor":"u1"},
            "total":7}"#,
    )
    .await;

    let page = client.admin_users(Some(1), None).await.expect("answered");

    assert_eq!((page.total, page.has_next_page), (7, true));
    assert_eq!(page.end_cursor.as_deref(), Some("u1"));
    assert_eq!(
        page.items[0].avatar_url.as_deref(),
        Some("https://beam.test/avatars/b1.png")
    );
    assert!(page.items[0].disabled);
    let request = the_request(&backend);
    assert_eq!(request.url.path(), "/v1/admin/users");
    let query = query_of(&request);
    assert_eq!(query.get("first").map(String::as_str), Some("1"));
    assert!(!query.contains_key("after"));
}

#[tokio::test]
async fn disabling_a_user_patches_only_that_flag() {
    const USER_ID: &str = "00000000-0000-0000-0000-0000000000b1";
    let (client, backend) = one_answer(204, "").await;

    client
        .set_user_disabled(USER_ID.to_owned(), true)
        .await
        .expect("disabled");

    let request = the_request(&backend);
    assert_eq!(request.method, reqwest::Method::PATCH);
    assert_eq!(request.url.path(), format!("/v1/admin/users/{USER_ID}"));
    assert_eq!(json_body(&request), serde_json::json!({"disabled": true}));
}

#[tokio::test]
async fn admin_events_ask_for_the_limit_and_fold_their_categories() {
    let (client, backend) = one_answer(
        200,
        r#"{"items":[
            {"id":"00000000-0000-0000-0000-0000000000e1","occurred_at":"2026-01-01T00:00:00Z",
             "level":"info","category":"scan_progress","message":"40 of 42",
             "library_id":"00000000-0000-0000-0000-00000000000a","library_name":"Films"},
            {"id":"00000000-0000-0000-0000-0000000000e2","occurred_at":"2026-01-01T00:00:01Z",
             "level":"error","category":"enrichment","message":"no match"}],
            "page_info":{"has_next_page":false,"has_previous_page":false}}"#,
    )
    .await;

    let events = client.admin_events(Some(3)).await.expect("answered");

    use crate::catalog::EventCategory;
    let categories: Vec<EventCategory> = events.iter().map(|event| event.category).collect();
    assert_eq!(
        categories,
        vec![EventCategory::LibraryScan, EventCategory::System]
    );
    assert_eq!(events[0].library_name.as_deref(), Some("Films"));
    let request = the_request(&backend);
    assert_eq!(request.url.path(), "/v1/admin/events");
    assert_eq!(
        query_of(&request).get("first").map(String::as_str),
        Some("3")
    );
}

#[tokio::test]
async fn creating_a_library_posts_its_name_and_root() {
    let (client, backend) = one_answer(200, LIBRARY).await;

    let library = client
        .create_library("Films".to_owned(), "/media/films".to_owned())
        .await
        .expect("created");

    assert_eq!(library.name, "Films");
    let request = the_request(&backend);
    assert_eq!(request.method, reqwest::Method::POST);
    assert_eq!(request.url.path(), "/v1/admin/libraries");
    assert_eq!(
        json_body(&request),
        serde_json::json!({"name": "Films", "root_path": "/media/films"})
    );
}

#[tokio::test]
async fn deleting_a_library_deletes_it_by_id() {
    let (client, backend) = one_answer(204, "").await;

    client
        .delete_library(LIBRARY_ID.to_owned())
        .await
        .expect("deleted");

    let request = the_request(&backend);
    assert_eq!(request.method, reqwest::Method::DELETE);
    assert_eq!(
        request.url.path(),
        format!("/v1/admin/libraries/{LIBRARY_ID}")
    );
}

#[tokio::test]
async fn refreshing_metadata_posts_to_the_titles_refresh() {
    let (client, backend) = one_answer(204, "").await;

    client
        .refresh_media_metadata(MEDIA_ID.to_owned())
        .await
        .expect("accepted");

    let request = the_request(&backend);
    assert_eq!(request.method, reqwest::Method::POST);
    assert_eq!(
        request.url.path(),
        format!("/v1/admin/media/{MEDIA_ID}/refresh")
    );
    assert!(matches!(
        client.refresh_media_metadata("x".to_owned()).await,
        Err(BeamError::BadRequest { .. })
    ));
    assert_eq!(backend.recorded().len(), 1, "a malformed id is never sent");
}
