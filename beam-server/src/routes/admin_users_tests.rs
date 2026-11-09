//! Subcutaneous tests for the `/v1/admin/users*` and `/v1/admin/status`
//! routes -- listing and disabling users, per-user access, and system status
//! -- over the fixture in [`super::admin_tests`].

use std::path::PathBuf;
use std::sync::Arc;

use beam_auth::utils::{
    models::{CreateUser, User},
    repository::in_memory::InMemoryUserRepository,
    session_store::{SessionData, SessionStore},
};
use beam_domain::models::enrichment::EnrichmentTargetId;
use beam_domain::repositories::{EnrichmentStateRepository, FileRepository};
use beam_domain::test_utils::create_media_file;
use kynos::http::StatusCode;
use serde_json::Value;

use super::admin_tests::{
    TestFixture, build_client, make_test_state, make_test_state_with_users, seed_user_session,
};
use crate::models::{
    AdminStatus, AdminUserConnection, ContentRatingBand, CreateLibraryRequest, Library,
    LibraryConnection, UpdateAdminUserRequest, UserAccess,
};

// ─── Admin users & system status (issue #85) ─────────────────────────────────

/// Seeds an additional non-admin user (no session) and returns it.
async fn seed_plain_user(fixture: &TestFixture, subject: &str, name: &str) -> User {
    fixture
        .user_repo
        .create(CreateUser {
            oidc_issuer: "https://test.example".to_string(),
            oidc_subject: subject.to_string(),
            email: None,
            display_name: name.to_string(),
            avatar_url: None,
            is_admin: false,
        })
        .await
        .expect("seed user should succeed")
}

/// Creates a live session for `user_id` directly in the fixture's store.
async fn seed_session_for(fixture: &TestFixture, user_id: &str) {
    fixture
        .session_store
        .create(
            &SessionData {
                user_id: user_id.to_string(),
                device_hash: "target-device".to_string(),
                ip: "127.0.0.1".to_string(),
                created_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
            },
            86400,
            86400,
        )
        .await
        .expect("seed session should succeed");
}

#[tokio::test]
async fn the_admin_user_and_status_endpoints_are_closed_to_a_regular_user_and_leave_the_target_enabled_with_its_sessions()
 {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, false).await;
    let target = seed_plain_user(&fixture, "target-subj", "Target").await;
    seed_session_for(&fixture, &target.id.to_string()).await;

    assert_eq!(
        client
            .get("/v1/admin/users")
            .cookie("beam_session", &token)
            .send()
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    assert_eq!(
        client
            .patch(&format!("/v1/admin/users/{}", target.id))
            .cookie("beam_session", &token)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send()
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    assert_eq!(
        client
            .get("/v1/admin/status")
            .cookie("beam_session", &token)
            .send()
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    let stored = fixture
        .user_repo
        .find_by_id(target.id)
        .await
        .unwrap()
        .expect("the target still exists");
    assert!(
        !stored.disabled,
        "the refused patch left the target enabled"
    );
    assert_eq!(
        fixture
            .session_store
            .list_for_user(&target.id.to_string())
            .await
            .unwrap()
            .len(),
        1,
        "and revoked none of its sessions"
    );
}

/// The admin scope is checked against the user row, so a session whose user
/// row has gone is no longer anyone's: it is 401, never the 403 a known
/// non-admin gets and never an admin answer.
#[tokio::test]
async fn an_admin_whose_row_is_gone_is_401_on_admin_routes() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let orphan = uuid::Uuid::from_u128(0xdead);
    let token = fixture
        .session_store
        .create(
            &SessionData {
                user_id: orphan.to_string(),
                device_hash: "orphan-device".to_string(),
                ip: "127.0.0.1".to_string(),
                created_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
            },
            86400,
            86400,
        )
        .await
        .expect("seed session should succeed");

    for path in ["/v1/admin/status", "/v1/admin/users", "/v1/admin/logs"] {
        client
            .get(path)
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}

/// NFR-205: a user store that cannot list is a 500, never an empty page that
/// tells an administrator there are no accounts.
#[tokio::test]
async fn a_user_store_failure_is_a_500_never_an_empty_user_list() {
    use beam_auth::utils::repository::in_memory::{FailingUserRepository, UserRepositoryOperation};

    let fixture = make_test_state_with_users(Arc::new(FailingUserRepository::new(
        InMemoryUserRepository::default(),
        UserRepositoryOperation::ListPage,
    )));
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    let response = client
        .get("/v1/admin/users")
        .cookie("beam_session", &token)
        .send()
        .await;
    response
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#internal");
    assert!(
        !response.text().contains("injected failure"),
        "the store's message never reaches the wire"
    );
}

#[tokio::test]
async fn listing_admin_users_without_a_session_is_401() {
    let fixture = make_test_state();
    let client = build_client(&fixture);

    assert_eq!(
        client.get("/v1/admin/users").send().await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_admin_user_list_paginates_and_reports_the_total() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await; // user 1 (admin)
    seed_plain_user(&fixture, "s2", "Alice").await;
    seed_plain_user(&fixture, "s3", "Bob").await;
    seed_plain_user(&fixture, "s4", "Carol").await;

    // The default page holds everyone, all enabled, exactly one admin.
    let response = client
        .get("/v1/admin/users")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: AdminUserConnection = response.json();
    assert_eq!(body.total, 4);
    assert_eq!(body.items.len(), 4);
    assert!(!body.page_info.has_next_page);
    assert!(body.items.iter().all(|u| !u.disabled));
    assert_eq!(body.items.iter().filter(|u| u.is_admin).count(), 1);

    // Two pages of two, the second after the first's `end_cursor`, cover all
    // four users exactly once, and `total` stays the full count on every
    // page.
    let page1: AdminUserConnection = client
        .get("/v1/admin/users?first=2")
        .cookie("beam_session", &token)
        .send()
        .await
        .json();
    assert!(page1.page_info.has_next_page);
    assert!(!page1.page_info.has_previous_page);
    let after = page1
        .page_info
        .end_cursor
        .clone()
        .expect("a full page ends somewhere");
    let page2: AdminUserConnection = client
        .get(&format!("/v1/admin/users?first=2&after={after}"))
        .cookie("beam_session", &token)
        .send()
        .await
        .json();
    assert!(!page2.page_info.has_next_page);
    assert!(page2.page_info.has_previous_page);
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page1.total, 4);
    assert_eq!(page2.total, 4);
    let mut ids: Vec<uuid::Uuid> = page1
        .items
        .iter()
        .chain(page2.items.iter())
        .map(|u| u.id)
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "pages must cover every user exactly once");

    // A page size below its published minimum of 1 is refused by Kynos's
    // `QueryParams` check rather than served.
    client
        .get("/v1/admin/users?first=0")
        .cookie("beam_session", &token)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .assert_problem_type("about:blank");

    // The log's cursor is not this list's.
    let logs = crate::services::cursor::offset::encode(
        crate::services::cursor::offset::OffsetList::AdminLogs,
        0,
    );
    // Neither is a forged position whose next page starts past the `bigint`
    // Postgres reads `OFFSET` as: it is the caller's 400, not a database 500.
    let past_offset = crate::services::cursor::offset::encode(
        crate::services::cursor::offset::OffsetList::AdminUsers,
        crate::services::cursor::offset::MAX_AT + 1,
    );
    for after in [logs, past_offset] {
        client
            .get(&format!("/v1/admin/users?after={after}"))
            .cookie("beam_session", &token)
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#invalid-cursor");
    }
}

#[tokio::test]
async fn disabling_a_user_revokes_their_sessions_and_re_enabling_flips_the_flag() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let admin_token = seed_user_session(&fixture, true).await;

    let target = seed_plain_user(&fixture, "target-subj", "Target").await;
    seed_session_for(&fixture, &target.id.to_string()).await;
    seed_session_for(&fixture, &target.id.to_string()).await;
    assert_eq!(
        fixture
            .session_store
            .list_for_user(&target.id.to_string())
            .await
            .unwrap()
            .len(),
        2
    );

    let path = format!("/v1/admin/users/{}", target.id);

    assert_eq!(
        client
            .patch(&path)
            .cookie("beam_session", &admin_token)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send()
            .await
            .status(),
        StatusCode::NO_CONTENT
    );

    let stored = fixture
        .user_repo
        .find_by_id(target.id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.disabled);
    assert_eq!(
        fixture
            .session_store
            .list_for_user(&target.id.to_string())
            .await
            .unwrap()
            .len(),
        0,
        "disabling must revoke every session of the target"
    );

    // Re-enable: the flag flips back and no session reappears.
    assert_eq!(
        client
            .patch(&path)
            .cookie("beam_session", &admin_token)
            .json(&UpdateAdminUserRequest { disabled: false })
            .send()
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let stored = fixture
        .user_repo
        .find_by_id(target.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!stored.disabled);
    assert_eq!(
        fixture
            .session_store
            .list_for_user(&target.id.to_string())
            .await
            .unwrap()
            .len(),
        0,
        "re-enabling must not mint sessions"
    );
}

#[tokio::test]
async fn an_admin_cannot_disable_their_own_account() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let admin = fixture
        .user_repo
        .find_by_oidc_identity("https://test.example", "admin-subj")
        .await
        .unwrap()
        .expect("admin was seeded");

    let response = client
        .patch(&format!("/v1/admin/users/{}", admin.id))
        .cookie("beam_session", &token)
        .json(&UpdateAdminUserRequest { disabled: true })
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let stored = fixture
        .user_repo
        .find_by_id(admin.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!stored.disabled, "self-disable must not change the account");
}

#[tokio::test]
async fn patching_an_unknown_user_is_404_and_an_unparseable_id_is_400() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;

    assert_eq!(
        client
            .patch(&format!("/v1/admin/users/{}", uuid::Uuid::new_v4()))
            .cookie("beam_session", &token)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send()
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        client
            .patch("/v1/admin/users/not-a-uuid")
            .cookie("beam_session", &token)
            .json(&UpdateAdminUserRequest { disabled: true })
            .send()
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn the_status_endpoint_reports_counts_queue_state_and_recent_scans() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await; // user 1 (admin)
    seed_plain_user(&fixture, "s2", "Alice").await; // user 2

    // One library created through the API, one file indexed into it.
    let create_response = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let library: Library = create_response.json();

    fixture
        .file_repo
        .create(create_media_file(library.id, "/videos/movies/a.mkv"))
        .await
        .unwrap();

    // Enrichment queue: two rows, one of which then fails terminally.
    fixture
        .enrichment_repo
        .ensure_pending(EnrichmentTargetId::Movie(uuid::Uuid::new_v4()))
        .await
        .unwrap();
    fixture
        .enrichment_repo
        .ensure_pending(EnrichmentTargetId::Movie(uuid::Uuid::new_v4()))
        .await
        .unwrap();
    let due = fixture
        .enrichment_repo
        .fetch_due(chrono::Utc::now(), 10)
        .await
        .unwrap();
    fixture
        .enrichment_repo
        .mark_failed(due[0].id, "provider exploded", chrono::Utc::now())
        .await
        .unwrap();

    // Two scan log entries plus one unrelated system entry that must be
    // filtered out of `recent_scans`.
    for message in ["scan one", "scan two"] {
        fixture
            .state
            .services
            .admin_log
            .log(
                beam_domain::models::AdminLogLevel::Info,
                beam_domain::models::AdminLogCategory::LibraryScan,
                message.to_string(),
                None,
            )
            .await
            .unwrap();
    }
    fixture
        .state
        .services
        .admin_log
        .log(
            beam_domain::models::AdminLogLevel::Info,
            beam_domain::models::AdminLogCategory::System,
            "server started".to_string(),
            None,
        )
        .await
        .unwrap();

    let response = client
        .get("/v1/admin/status")
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: AdminStatus = response.json();

    // uptime_secs deserialized as u64 (its presence is shape-verified);
    // version is the crate's own.
    assert_eq!(body.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(body.counts.users, 2);
    assert_eq!(body.counts.libraries, 1);
    assert_eq!(body.counts.files, 1);
    assert_eq!(body.enrichment.pending, 1);
    assert_eq!(body.enrichment.failed, 1);
    assert_eq!(body.enrichment.enriched, 0);
    assert_eq!(body.enrichment.unmatched, 0);
    // FR-307: no token and AniList off -- neither provider is configured,
    // and the status says what to set.
    assert_eq!(
        body.enrichment_providers
            .iter()
            .map(|status| (status.provider, status.state))
            .collect::<Vec<_>>(),
        vec![
            (
                crate::models::MetadataProvider::Tmdb,
                crate::models::EnrichmentProviderState::NotConfigured
            ),
            (
                crate::models::MetadataProvider::Anilist,
                crate::models::EnrichmentProviderState::NotConfigured
            ),
        ]
    );
    assert!(
        body.enrichment_providers[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("BEAM_TMDB_API_TOKEN"))
    );
    // Issue #267: every provider, TheTVDB included, in the list that is no
    // frozen enumeration.
    assert_eq!(
        body.metadata_providers
            .iter()
            .map(|status| (status.provider.as_str(), status.state))
            .collect::<Vec<_>>(),
        vec![
            (
                "tmdb",
                crate::models::EnrichmentProviderState::NotConfigured
            ),
            (
                "anilist",
                crate::models::EnrichmentProviderState::NotConfigured
            ),
            (
                "tvdb",
                crate::models::EnrichmentProviderState::NotConfigured
            ),
        ]
    );
    assert!(
        body.metadata_providers[2]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("BEAM_TVDB_API_KEY"))
    );

    let messages: Vec<&str> = body
        .recent_scans
        .iter()
        .map(|scan| scan.message.as_str())
        .collect();
    assert_eq!(body.recent_scans.len(), 2);
    assert!(messages.contains(&"scan one"));
    assert!(messages.contains(&"scan two"));
    assert!(
        !messages.contains(&"server started"),
        "non-scan categories must be filtered out"
    );
}

// ── Watcher status (issue #186) ─────────────────────────────────────────────

#[tokio::test]
async fn the_status_endpoint_reports_how_each_library_is_watched() {
    use crate::models::{LibraryPollReason, LibraryWatchMode};
    use beam_index::services::watch_status::{PollReason, WatchMode};

    let fixture = make_test_state();
    let client = build_client(&fixture);
    let token = seed_user_session(&fixture, true).await;
    let library: Library = client
        .post("/v1/admin/libraries")
        .cookie("beam_session", &token)
        .json(&CreateLibraryRequest {
            name: "Movies".to_string(),
            root_path: "movies".to_string(),
        })
        .send()
        .await
        .json();
    let library_id = library.id;

    let get_watcher = || async {
        client
            .get("/v1/admin/status")
            .cookie("beam_session", &token)
            .send()
            .await
            .json::<AdminStatus>()
            .watcher
    };

    // Nothing has registered a watch yet: the library is unwatched.
    let watcher = get_watcher().await;
    assert!(!watcher.enabled);
    assert_eq!(watcher.libraries.len(), 1);
    assert_eq!(watcher.libraries[0].library_id, library_id);
    assert_eq!(watcher.libraries[0].mode, LibraryWatchMode::Unwatched);
    assert_eq!(watcher.libraries[0].poll_reason, None);

    // What the runtime writes when the library hits the watch limit.
    let status = &fixture.state.services.watch_status;
    status.set_enabled(true);
    status.set_max_user_watches(Some(8192));
    status.mark_limit_reached();
    status.set_mode(
        library_id,
        WatchMode::Polling(PollReason::WatchLimitReached),
    );

    let watcher = get_watcher().await;
    assert!(watcher.enabled);
    assert!(watcher.watch_limit_reached);
    assert_eq!(watcher.watch_limit_count, Some(8192));
    assert_eq!(watcher.libraries[0].mode, LibraryWatchMode::Polling);
    assert_eq!(
        watcher.libraries[0].poll_reason,
        Some(LibraryPollReason::WatchLimitReached)
    );
}

// ─── Per-user access (issue #263) ───────────────────────────────────────────

/// A user of its own, and a session for them.
async fn seed_named_user(
    fixture: &TestFixture,
    subject: &str,
    is_admin: bool,
) -> (uuid::Uuid, String) {
    let user = fixture
        .user_repo
        .create(CreateUser {
            oidc_issuer: "https://test.example".to_string(),
            oidc_subject: subject.to_string(),
            email: None,
            display_name: subject.to_string(),
            avatar_url: None,
            is_admin,
        })
        .await
        .expect("seed a user");
    let token = fixture
        .session_store
        .create(
            &SessionData {
                user_id: user.id.to_string(),
                device_hash: "test-device".to_string(),
                ip: "127.0.0.1".to_string(),
                created_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
            },
            86400,
            86400,
        )
        .await
        .expect("seed a session");
    (user.id, token)
}

/// A library row, as registration leaves one.
async fn seed_library(fixture: &TestFixture, name: &str) -> uuid::Uuid {
    fixture
        .state
        .services
        .library_repo
        .create(beam_domain::models::library::CreateLibrary {
            name: name.to_string(),
            root_path: PathBuf::from(format!("/videos/{name}")),
            description: None,
        })
        .await
        .expect("seed a library")
        .id
}

fn sorted(mut ids: Vec<uuid::Uuid>) -> Vec<uuid::Uuid> {
    ids.sort();
    ids
}

#[tokio::test]
async fn a_users_access_reads_back_as_it_was_set_and_is_replaced_whole() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let (_, admin) = seed_named_user(&fixture, "admin", true).await;
    let (child, _) = seed_named_user(&fixture, "child", false).await;
    let (kids, family) = (
        seed_library(&fixture, "kids").await,
        seed_library(&fixture, "family").await,
    );
    let path = format!("/v1/admin/users/{child}/access");

    let unrestricted: UserAccess = client
        .get(&path)
        .cookie("beam_session", &admin)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(
        unrestricted,
        UserAccess {
            all_libraries: true,
            library_ids: vec![],
            max_content_rating: None,
        }
    );

    for access in [
        UserAccess {
            all_libraries: false,
            library_ids: vec![kids, family],
            max_content_rating: Some(ContentRatingBand::ParentalGuidance),
        },
        UserAccess {
            all_libraries: false,
            library_ids: vec![family],
            max_content_rating: None,
        },
        UserAccess {
            all_libraries: true,
            library_ids: vec![],
            max_content_rating: Some(ContentRatingBand::Teen),
        },
    ] {
        let mut stored: UserAccess = client
            .put(&path)
            .cookie("beam_session", &admin)
            .json(&access)
            .send()
            .await
            .assert_status(StatusCode::OK)
            .json();
        let mut read: UserAccess = client
            .get(&path)
            .cookie("beam_session", &admin)
            .send()
            .await
            .assert_status(StatusCode::OK)
            .json();
        stored.library_ids = sorted(stored.library_ids);
        read.library_ids = sorted(read.library_ids);
        let expected = UserAccess {
            library_ids: sorted(access.library_ids.clone()),
            ..access
        };
        assert_eq!(stored, expected, "what PUT answers");
        assert_eq!(read, expected, "what GET reads back");
    }
}

#[tokio::test]
async fn access_naming_a_missing_library_or_both_kinds_of_grant_is_422_and_changes_nothing() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let (_, admin) = seed_named_user(&fixture, "admin", true).await;
    let (child, _) = seed_named_user(&fixture, "child", false).await;
    let kids = seed_library(&fixture, "kids").await;
    let path = format!("/v1/admin/users/{child}/access");

    for (case, body, pointers) in [
        (
            "an unknown library",
            UserAccess {
                all_libraries: false,
                library_ids: vec![kids, uuid::Uuid::new_v4()],
                max_content_rating: None,
            },
            vec!["/library_ids/1"],
        ),
        (
            "every library and some",
            UserAccess {
                all_libraries: true,
                library_ids: vec![kids],
                max_content_rating: None,
            },
            vec!["/library_ids"],
        ),
    ] {
        let problem: Value = client
            .put(&path)
            .cookie("beam_session", &admin)
            .json(&body)
            .send()
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
            .assert_problem_type("https://beam.justinchung.net/reference/errors/#validation-failed")
            .json();
        let named: Vec<&str> = problem["errors"]
            .as_array()
            .unwrap_or_else(|| panic!("{case}: an errors list: {problem}"))
            .iter()
            .map(|error| error["pointer"].as_str().unwrap())
            .collect();
        assert_eq!(named, pointers, "{case}");
    }
    assert_eq!(
        fixture
            .state
            .services
            .access
            .policy(child)
            .await
            .expect("read the policy"),
        beam_domain::models::access::AccessPolicy::unrestricted(),
        "nothing was stored"
    );
}

#[tokio::test]
async fn access_is_an_administrators_to_read_and_set_and_names_a_user_that_exists() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let (_, admin) = seed_named_user(&fixture, "admin", true).await;
    let (child, child_token) = seed_named_user(&fixture, "child", false).await;
    let body = UserAccess {
        all_libraries: true,
        library_ids: vec![],
        max_content_rating: None,
    };

    let own = format!("/v1/admin/users/{child}/access");
    client
        .get(&own)
        .cookie("beam_session", &child_token)
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .put(&own)
        .cookie("beam_session", &child_token)
        .json(&body)
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    let nobody = format!("/v1/admin/users/{}/access", uuid::Uuid::new_v4());
    client
        .get(&nobody)
        .cookie("beam_session", &admin)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#user-not-found");
    client
        .put(&nobody)
        .cookie("beam_session", &admin)
        .json(&body)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .assert_problem_type("https://beam.justinchung.net/reference/errors/#user-not-found");
}

/// A restricted viewer's library reads hold only the libraries they were
/// granted, and a ceiling closes every file listing: a file's path names
/// its title whatever that title's rating.
#[tokio::test]
async fn a_restricted_viewer_reads_only_the_libraries_granted_to_them() {
    let fixture = make_test_state();
    let client = build_client(&fixture);
    let (_, admin) = seed_named_user(&fixture, "admin", true).await;
    let (child, child_token) = seed_named_user(&fixture, "child", false).await;
    let (kids, grown) = (
        seed_library(&fixture, "kids").await,
        seed_library(&fixture, "grown").await,
    );
    let listed = || async {
        let response = client
            .get("/v1/libraries")
            .cookie("beam_session", &child_token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        sorted(
            response
                .json::<LibraryConnection>()
                .items
                .into_iter()
                .map(|library| library.id)
                .collect(),
        )
    };
    let read = |path: String| {
        let client = &client;
        let token = child_token.clone();
        async move {
            client
                .get(&path)
                .cookie("beam_session", &token)
                .send()
                .await
                .status()
        }
    };
    let set = |access: UserAccess| {
        let client = &client;
        let admin = admin.clone();
        async move {
            let path = format!("/v1/admin/users/{child}/access");
            let response = client
                .put(&path)
                .cookie("beam_session", &admin)
                .json(&access)
                .send()
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }
    };

    assert_eq!(listed().await, sorted(vec![kids, grown]));

    set(UserAccess {
        all_libraries: false,
        library_ids: vec![kids],
        max_content_rating: None,
    })
    .await;
    assert_eq!(listed().await, vec![kids]);
    assert_eq!(read(format!("/v1/libraries/{kids}")).await, StatusCode::OK);
    assert_eq!(
        read(format!("/v1/libraries/{grown}")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        read(format!("/v1/libraries/{kids}/files")).await,
        StatusCode::OK
    );
    assert_eq!(
        read(format!("/v1/libraries/{grown}/files")).await,
        StatusCode::NOT_FOUND
    );

    set(UserAccess {
        all_libraries: false,
        library_ids: vec![kids],
        max_content_rating: Some(ContentRatingBand::Teen),
    })
    .await;
    assert_eq!(read(format!("/v1/libraries/{kids}")).await, StatusCode::OK);
    assert_eq!(
        read(format!("/v1/libraries/{kids}/files")).await,
        StatusCode::NOT_FOUND
    );
}
