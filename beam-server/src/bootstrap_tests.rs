use super::*;

fn https_deployment() -> ServerConfig {
    ServerConfig {
        server_url: "https://beam.example.com".to_string(),
        web_url: "https://beam.example.com".to_string(),
        ..Default::default()
    }
}

mod cookie_security_gate {
    use super::*;

    #[test]
    fn a_plain_http_dev_setup_starts_without_comment() {
        let config = ServerConfig {
            server_url: "http://localhost:8000".to_string(),
            web_url: "http://localhost:5173".to_string(),
            ..Default::default()
        };
        assert_eq!(
            check_cookie_security(&config).unwrap(),
            StartupGate::Proceed
        );
    }

    #[test]
    fn a_fully_https_deployment_starts_without_comment() {
        assert_eq!(
            check_cookie_security(&https_deployment()).unwrap(),
            StartupGate::Proceed
        );
    }

    #[test]
    fn a_tls_terminating_proxy_without_an_override_refuses_to_start() {
        // The footgun this gate exists for: the server sees plain HTTP, the
        // browser sees HTTPS, and the session cookie would ship without
        // Secure on a production site.
        let config = ServerConfig {
            server_url: "http://localhost:8000".to_string(),
            web_url: "https://beam.example.com".to_string(),
            ..Default::default()
        };

        let error = check_cookie_security(&config)
            .expect_err("an implicit insecure cookie on an HTTPS site must stop startup");
        assert!(
            error.contains("BEAM_SERVER_URL") && error.contains("BEAM_COOKIE_SECURE"),
            "the message must name both ways out: {error}"
        );
    }

    #[test]
    fn an_explicit_opt_out_starts_but_says_so() {
        let config = ServerConfig {
            server_url: "http://localhost:8000".to_string(),
            web_url: "https://beam.example.com".to_string(),
            cookie_secure: Some(false),
            ..Default::default()
        };

        let StartupGate::ProceedWithWarning(warning) = check_cookie_security(&config).unwrap()
        else {
            panic!("an explicit opt-out must warn, not pass silently");
        };
        assert!(warning.contains("BEAM_COOKIE_SECURE=false"));
    }

    #[test]
    fn an_explicit_opt_in_resolves_the_misconfiguration() {
        let config = ServerConfig {
            server_url: "http://localhost:8000".to_string(),
            web_url: "https://beam.example.com".to_string(),
            cookie_secure: Some(true),
            ..Default::default()
        };
        assert_eq!(
            check_cookie_security(&config).unwrap(),
            StartupGate::Proceed
        );
    }
}

mod retired_env_vars {
    use super::*;

    #[test]
    fn a_retired_variable_that_is_still_set_is_named_in_a_warning() {
        let warnings = retired_env_warnings(|name| name == "BEAM_HASH_UNKNOWN_FILES");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("BEAM_HASH_UNKNOWN_FILES "));
    }

    #[test]
    fn nothing_is_said_when_no_retired_variable_is_set() {
        assert!(retired_env_warnings(|_| false).is_empty());
    }
}

mod cors {
    use std::collections::BTreeSet;

    use kynos::http::StatusCode;
    use kynos::prelude::*;
    use kynos::response::status::NoContent;
    use kynos::test::TestClient;

    use super::*;

    // Three operations on one path, doing nothing: the policy is the subject,
    // and the *set* of methods is now part of what it answers with.
    #[kynos::get("/thing", operation_id = "corsProbe")]
    async fn probe() -> NoContent {
        NoContent
    }

    #[kynos::patch("/thing", operation_id = "corsProbePatch")]
    async fn probe_patch() -> NoContent {
        NoContent
    }

    #[kynos::delete("/thing", operation_id = "corsProbeDelete")]
    async fn probe_delete() -> NoContent {
        NoContent
    }

    /// `Cors` is an `Interceptor<C>` for any context, so the policy needs no
    /// `AppState` to be exercised. The allowed origin is the web one
    /// [`https_deployment`] configures.
    fn client() -> TestClient<()> {
        let service = Router::new()
            .mount(kynos::routes![probe, probe_patch, probe_delete])
            .intercept(cors_policy(&AllowedOrigins::from_config(
                &https_deployment(),
            )))
            .build(())
            .expect("the probe router describes itself");

        TestClient::new(service)
    }

    /// A preflight proposing `method`, from the configured web origin.
    async fn preflight(method: &str) -> kynos::test::TestResponse {
        client()
            .options("/thing")
            .header("origin", "https://beam.example.com")
            .header("access-control-request-method", method)
            .header("access-control-request-headers", "range")
            .send()
            .await
    }

    #[tokio::test]
    async fn a_preflight_allows_the_headers_the_player_and_client_send() {
        let response = preflight("GET").await;

        // Kynos answers a preflight with 204: it is a protocol answer rather
        // than an operation, so there is no body and no declared status.
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let allowed = response
            .header("access-control-allow-headers")
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            allowed.contains("range"),
            "without `range` the player cannot seek: {allowed}"
        );
        assert!(allowed.contains("content-type"), "{allowed}");
    }

    /// The advertised methods are derived from the operations declared on the
    /// matched path, so preflight and the description cannot disagree. The
    /// hand-written list this replaces named GET, POST, PUT, DELETE and
    /// OPTIONS -- but not PATCH, which `/v1/admin/users/{id}` has used since
    /// issue #85, so a browser preflighting that request was told no.
    ///
    /// HEAD is in the set because the path's GET answers it (RFC 9110
    /// section 9.3.2), which Kynos advertises from 0.4.0 on.
    #[tokio::test]
    async fn the_advertised_methods_are_the_ones_the_path_actually_declares() {
        let response = preflight("PATCH").await;

        let advertised: BTreeSet<&str> = response
            .header("access-control-allow-methods")
            .expect("a permitted preflight advertises the methods")
            .split(',')
            .map(str::trim)
            .collect();

        assert_eq!(
            advertised,
            BTreeSet::from(["GET", "HEAD", "PATCH", "DELETE"]),
            "the advertised set must be exactly what this path declares"
        );
    }

    #[tokio::test]
    async fn credentials_are_allowed_so_the_session_cookie_is_sent() {
        let response = client()
            .get("/thing")
            .header("origin", "https://beam.example.com")
            .send()
            .await;

        assert_eq!(
            response.header("access-control-allow-credentials"),
            Some("true"),
            "cookie-only auth needs credentialed CORS"
        );
    }

    #[tokio::test]
    async fn the_range_response_headers_are_exposed_to_the_browser() {
        // A browser cannot read `content-range` off a cross-origin response
        // unless it is explicitly exposed, and the player needs it to know how
        // much of the file it just received.
        let response = client()
            .get("/thing")
            .header("origin", "https://beam.example.com")
            .send()
            .await;

        let exposed = response
            .header("access-control-expose-headers")
            .unwrap_or_default()
            .to_ascii_lowercase();
        for header in ["accept-ranges", "content-length", "content-range"] {
            assert!(exposed.contains(header), "{header} not exposed: {exposed}");
        }
    }

    /// The origin is allow-listed, not mirrored: a credentialed response is
    /// readable only by an origin the configuration names (issue #348).
    #[tokio::test]
    async fn an_origin_the_configuration_does_not_name_gets_no_cors_headers() {
        let response = client()
            .get("/thing")
            .header("origin", "https://somewhere.else.example")
            .send()
            .await;

        assert_eq!(response.header("access-control-allow-origin"), None);
        assert_eq!(response.header("access-control-allow-credentials"), None);
    }
}

mod allowed_origins {
    use super::*;

    #[test]
    fn the_web_server_and_extra_origins_are_all_named_without_trailing_slashes() {
        let origins = AllowedOrigins::from_config(&ServerConfig {
            web_url: "https://app.example.com/".to_string(),
            server_url: "https://api.example.com".to_string(),
            extra_allowed_origins: Some(
                " https://one.example.com/ ,,https://two.example.com".to_string(),
            ),
            ..Default::default()
        });

        for origin in [
            "https://app.example.com",
            "https://api.example.com",
            "https://one.example.com",
            "https://two.example.com",
        ] {
            assert!(origins.contains(origin), "{origin} is configured");
        }
        assert!(
            !origins.contains(""),
            "an empty list entry is not an origin"
        );
        assert!(!origins.contains("https://app.example.com/"));
    }

    #[test]
    fn an_origin_matches_whatever_the_case_of_its_scheme_and_host() {
        let origins = AllowedOrigins::from_config(&ServerConfig {
            web_url: "https://App.Example.com".to_string(),
            server_url: "https://api.example.com".to_string(),
            ..Default::default()
        });

        assert!(origins.contains("https://app.example.com"));
        assert!(origins.contains("HTTPS://API.EXAMPLE.COM"));
        assert!(!origins.contains("https://app.example.org"));
    }
}

/// The policy as `/v1` serves it: through `create_router`, over a real
/// `AppState`, with a session the stores' own traits minted. These are the
/// tests the mirrored policy would have failed -- the router is the subject,
/// so dropping or reordering the CORS interceptor is caught here.
mod served_router {
    use beam_auth::utils::models::CreateUser;
    use beam_auth::utils::session_store::SessionData;
    use kynos::http::StatusCode;
    use kynos::test::TestClient;

    use super::*;
    use crate::routes::create_router;
    use crate::routes::test_support::make_app_state_with;
    use crate::state::AppState;

    const WEB_ORIGIN: &str = "https://beam.example.com";
    const EXTRA_ORIGIN: &str = "https://admin.example.com";
    /// A same-site sibling: its requests carry a `SameSite=Lax` cookie, so
    /// only CORS stands between it and the response.
    const SIBLING_ORIGIN: &str = "https://evil.example.com";

    /// The served router over a state configured with [`WEB_ORIGIN`] and
    /// [`EXTRA_ORIGIN`], built with the origins `main` would pass.
    fn served() -> (TestClient<AppState>, AppState) {
        let state = make_app_state_with(|config| {
            config.web_url = WEB_ORIGIN.to_string();
            config.server_url = "https://api.example.com".to_string();
            config.extra_allowed_origins = Some(EXTRA_ORIGIN.to_string());
        });
        let service = create_router(&AllowedOrigins::from_config(&state.config))
            .build(state.clone())
            .expect("the served router describes itself");
        (TestClient::new(service), state)
    }

    /// Seeds a user and a session through the stores' own traits and returns
    /// the `beam_session` cookie value.
    async fn session(state: &AppState) -> String {
        let user = state
            .services
            .user_repo
            .create(CreateUser {
                oidc_issuer: "https://idp.example".to_string(),
                oidc_subject: "subject".to_string(),
                email: Some("someone@example.com".to_string()),
                display_name: "Someone".to_string(),
                avatar_url: None,
                is_admin: false,
            })
            .await
            .unwrap();
        let now = chrono::Utc::now().timestamp();
        state
            .services
            .session_store
            .create(
                &SessionData {
                    user_id: user.id.to_string(),
                    device_hash: "test-device".to_string(),
                    ip: "127.0.0.1".to_string(),
                    created_at: now,
                    last_active: now,
                },
                86400,
                86400,
            )
            .await
            .unwrap()
    }

    async fn me_from(origin: &str) -> kynos::test::TestResponse {
        let (client, state) = served();
        let token = session(&state).await;
        client
            .get("/v1/me")
            .header("origin", origin)
            .cookie("beam_session", &token)
            .send()
            .await
    }

    #[tokio::test]
    async fn a_cross_origin_reader_cannot_read_an_authenticated_response() {
        let response = me_from(SIBLING_ORIGIN).await;

        // The server still answers -- CORS is enforced by the browser, which
        // withholds a response that carries no grant for the asking origin.
        response.assert_status(StatusCode::OK);
        assert_eq!(
            response.header("access-control-allow-origin"),
            None,
            "an origin nobody configured must not be granted a credentialed read"
        );
        assert_eq!(response.header("access-control-allow-credentials"), None);
    }

    #[tokio::test]
    async fn the_configured_web_origin_may_read_with_credentials() {
        for origin in [WEB_ORIGIN, EXTRA_ORIGIN] {
            let response = me_from(origin).await;

            response.assert_status(StatusCode::OK);
            assert_eq!(
                response.header("access-control-allow-origin"),
                Some(origin),
                "{origin} is configured"
            );
            assert_eq!(
                response.header("access-control-allow-credentials"),
                Some("true"),
                "{origin} is configured"
            );
        }
    }

    #[tokio::test]
    async fn a_mixed_case_configured_origin_may_both_read_and_write() {
        // The CORS grant compares case-insensitively; the same-origin write
        // check must agree, or the origin reads but every write is a 403.
        let state = make_app_state_with(|config| {
            config.web_url = "https://Beam.Example.com".to_string();
            config.server_url = "https://api.example.com".to_string();
        });
        let client = TestClient::new(
            create_router(&AllowedOrigins::from_config(&state.config))
                .build(state.clone())
                .expect("the served router describes itself"),
        );
        let token = session(&state).await;

        let read = client
            .get("/v1/me")
            .header("origin", WEB_ORIGIN)
            .cookie("beam_session", &token)
            .send()
            .await;
        read.assert_status(StatusCode::OK);
        assert_eq!(read.header("access-control-allow-origin"), Some(WEB_ORIGIN));

        let write = client
            .post("/v1/logout")
            .header("origin", WEB_ORIGIN)
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_ne!(
            write.status(),
            StatusCode::FORBIDDEN,
            "an origin trusted to read must be trusted to write"
        );
    }

    #[tokio::test]
    async fn a_preflight_from_an_unconfigured_origin_is_not_granted() {
        let (client, _) = served();
        let response = client
            .options("/v1/me")
            .header("origin", SIBLING_ORIGIN)
            .header("access-control-request-method", "GET")
            .send()
            .await;

        assert_eq!(response.header("access-control-allow-origin"), None);
    }

    #[tokio::test]
    async fn a_request_without_an_origin_is_answered_without_cors_headers() {
        // A native client sends no `Origin`; this is not a CORS request, and
        // the allow-list must not get in its way.
        let (client, state) = served();
        let token = session(&state).await;
        let response = client
            .get("/v1/me")
            .cookie("beam_session", &token)
            .send()
            .await;

        response.assert_status(StatusCode::OK);
        assert_eq!(response.header("access-control-allow-origin"), None);
    }
}

mod library_declarations {
    use super::*;
    use std::sync::Arc;

    fn declared(name: &str, path: &str) -> LibraryDeclaration {
        LibraryDeclaration {
            name: name.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn a_blank_value_declares_nothing() {
        for raw in ["", "  \n", "[]"] {
            assert_eq!(parse_library_declarations(raw).unwrap(), vec![], "{raw:?}");
        }
    }

    #[test]
    fn names_are_trimmed_and_order_is_kept() {
        let parsed = parse_library_declarations(
            r#"[{"name":" Movies ","path":"/videos/movies"},{"name":"shows","path":"shows"}]"#,
        )
        .unwrap();
        assert_eq!(
            parsed,
            vec![
                declared("Movies", "/videos/movies"),
                declared("shows", "shows")
            ]
        );
    }

    #[test]
    fn a_malformed_or_ambiguous_declaration_is_refused() {
        /// Whether a refusal is the one a case expects.
        type Expected = fn(&LibraryDeclarationError) -> bool;
        let cases: [(&str, Expected); 7] = [
            ("{", |e| matches!(e, LibraryDeclarationError::Json(_))),
            (r#"{"name":"a","path":"/a"}"#, |e| {
                matches!(e, LibraryDeclarationError::Json(_))
            }),
            (r#"[{"name":"a"}]"#, |e| {
                matches!(e, LibraryDeclarationError::Json(_))
            }),
            (r#"[{"name":"a","path":"/a","mount":"/b"}]"#, |e| {
                matches!(e, LibraryDeclarationError::Json(_))
            }),
            (
                r#"[{"name":"a","path":"/a"},{"name":" ","path":"/b"}]"#,
                |e| matches!(e, LibraryDeclarationError::EmptyName(1)),
            ),
            (
                r#"[{"name":"a","path":""}]"#,
                |e| matches!(e, LibraryDeclarationError::EmptyPath(name) if name == "a"),
            ),
            (
                r#"[{"name":"a","path":"/a"},{"name":"a ","path":"/b"}]"#,
                |e| matches!(e, LibraryDeclarationError::DuplicateName(name) if name == "a"),
            ),
        ];
        for (raw, expected) in cases {
            let err = parse_library_declarations(raw).expect_err(raw);
            assert!(expected(&err), "{raw}: {err:?}");
        }
        let err =
            parse_library_declarations(r#"[{"name":"a","path":"/m"},{"name":"b","path":"/m"}]"#)
                .unwrap_err();
        assert!(
            matches!(&err, LibraryDeclarationError::DuplicatePath(path) if path == "/m"),
            "{err:?}"
        );
    }

    /// A video directory holding `movies`, `shows` and `anime`, and a data
    /// directory beside it, all on a real filesystem: the root a declaration
    /// names is resolved exactly as in production.
    struct Host {
        _tmp: tempfile::TempDir,
        videos: std::path::PathBuf,
        data: std::path::PathBuf,
    }

    fn host() -> Host {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let base = tmp.path().canonicalize().expect("canonical temp dir");
        let videos = base.join("videos");
        for name in ["movies", "shows", "anime"] {
            std::fs::create_dir_all(videos.join(name)).unwrap();
        }
        let data = base.join("data");
        std::fs::create_dir_all(&data).unwrap();
        Host {
            _tmp: tmp,
            videos,
            data,
        }
    }

    /// An empty library store, which outlives each boot that uses it.
    fn no_libraries()
    -> Arc<beam_domain::repositories::library::in_memory::InMemoryLibraryRepository> {
        Arc::default()
    }

    /// One boot's library service over `libraries`, the store that outlives
    /// it. The index service expects no call: declaring never scans.
    fn boot(
        host: &Host,
        libraries: Arc<beam_domain::repositories::library::in_memory::InMemoryLibraryRepository>,
    ) -> crate::services::library::LocalLibraryService {
        use crate::services::library::{
            LibraryServiceDependencies, LocalLibraryService, OsPathValidator,
        };
        LocalLibraryService::new(LibraryServiceDependencies {
            library_repo: libraries,
            file_repo: Arc::new(
                beam_domain::repositories::file::in_memory::InMemoryFileRepository::default(),
            ),
            video_dir: host.videos.clone(),
            data_dir: host.data.clone(),
            notification_service: Arc::new(
                beam_index::services::notification::InMemoryNotificationService::new(),
            ),
            index_service: Arc::new(beam_index::services::index::MockIndexService::new()),
            path_validator: Arc::new(OsPathValidator),
            watch_hook: Arc::new(beam_index::runtime::LibraryWatches::new(None)),
        })
    }

    /// Every stored library as `(name, root)`, sorted.
    async fn stored(
        libraries: &beam_domain::repositories::library::in_memory::InMemoryLibraryRepository,
    ) -> Vec<(String, std::path::PathBuf)> {
        use beam_domain::repositories::LibraryRepository;
        let mut stored: Vec<_> = libraries
            .find_all()
            .await
            .unwrap()
            .into_iter()
            .map(|library| (library.name, library.root_path))
            .collect();
        stored.sort();
        stored
    }

    async fn store(
        libraries: &beam_domain::repositories::library::in_memory::InMemoryLibraryRepository,
        name: &str,
        root: std::path::PathBuf,
    ) {
        use beam_domain::repositories::LibraryRepository;
        libraries
            .create(beam_domain::models::CreateLibrary {
                name: name.to_string(),
                root_path: root,
                description: None,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn declared_libraries_are_created_once_across_two_boots() {
        let host = host();
        let libraries = no_libraries();
        // One absolute, one relative to the video directory, as either may be
        // declared.
        let declarations = vec![
            declared("Movies", &host.videos.join("movies").display().to_string()),
            declared("Shows", "shows"),
        ];

        let first = declare_libraries(&boot(&host, libraries.clone()), &declarations).await;
        assert!(
            first
                .iter()
                .all(|report| matches!(report.outcome, Ok(LibraryDeclared::Created(_)))),
            "{first:?}"
        );
        let after_first = stored(&libraries).await;
        assert_eq!(
            after_first,
            vec![
                ("Movies".to_string(), host.videos.join("movies")),
                ("Shows".to_string(), host.videos.join("shows")),
            ]
        );

        let second = declare_libraries(&boot(&host, libraries.clone()), &declarations).await;
        let outcomes: Vec<_> = second
            .iter()
            .map(|report| match &report.outcome {
                Ok(LibraryDeclared::AlreadyStored { stored_name }) => {
                    (report.name.clone(), stored_name.clone())
                }
                other => panic!("{}: {other:?}", report.name),
            })
            .collect();
        assert_eq!(
            outcomes,
            vec![
                ("Movies".to_string(), "Movies".to_string()),
                ("Shows".to_string(), "Shows".to_string()),
            ]
        );
        assert_eq!(stored(&libraries).await, after_first);
    }

    #[tokio::test]
    async fn a_library_stored_at_the_declared_root_keeps_its_own_name() {
        let host = host();
        let libraries = no_libraries();
        store(&libraries, "Films", host.videos.join("movies")).await;

        let reports = declare_libraries(
            &boot(&host, libraries.clone()),
            &[declared("Movies", "movies")],
        )
        .await;

        assert!(
            matches!(
                &reports[0].outcome,
                Ok(LibraryDeclared::AlreadyStored { stored_name }) if stored_name == "Films"
            ),
            "{reports:?}"
        );
        assert_eq!(
            stored(&libraries).await,
            vec![("Films".to_string(), host.videos.join("movies"))]
        );
    }

    #[tokio::test]
    async fn a_name_held_at_another_root_is_neither_moved_nor_duplicated() {
        let host = host();
        let libraries = no_libraries();
        store(&libraries, "Movies", host.videos.join("anime")).await;

        let reports = declare_libraries(
            &boot(&host, libraries.clone()),
            &[declared("Movies", "movies")],
        )
        .await;

        assert!(
            matches!(reports[0].outcome, Ok(LibraryDeclared::NameTaken)),
            "{reports:?}"
        );
        assert_eq!(
            stored(&libraries).await,
            vec![("Movies".to_string(), host.videos.join("anime"))]
        );
    }

    #[tokio::test]
    async fn a_declaration_the_root_rules_refuse_is_skipped_and_the_rest_still_apply() {
        let host = host();
        let libraries = no_libraries();
        std::fs::create_dir_all(host.videos.join("shows/kids")).unwrap();
        store(&libraries, "Shows", host.videos.join("shows")).await;

        let reports = declare_libraries(
            &boot(&host, libraries.clone()),
            &[
                // Inside a stored library (#186).
                declared("Kids", "shows/kids"),
                // Not mounted.
                declared("Music", "music"),
                // Outside the video directory.
                declared(
                    "Elsewhere",
                    &host.data.parent().unwrap().display().to_string(),
                ),
                declared("Anime", "anime"),
            ],
        )
        .await;

        let outcome = |name: &str| {
            &reports
                .iter()
                .find(|report| report.name == name)
                .unwrap()
                .outcome
        };
        assert!(matches!(
            outcome("Kids"),
            Err(LibraryError::PathOverlapsLibrary)
        ));
        assert!(matches!(
            outcome("Music"),
            Err(LibraryError::PathNotFound(_))
        ));
        assert!(matches!(
            outcome("Elsewhere"),
            Err(LibraryError::PathOutsideRoot(_))
        ));
        assert!(matches!(outcome("Anime"), Ok(LibraryDeclared::Created(_))));
        assert_eq!(
            stored(&libraries).await,
            vec![
                ("Anime".to_string(), host.videos.join("anime")),
                ("Shows".to_string(), host.videos.join("shows")),
            ]
        );
    }

    #[tokio::test]
    async fn a_data_directory_under_the_video_directory_is_never_declared_a_library() {
        let host = host();
        let data = host.videos.join("beam-data");
        std::fs::create_dir_all(data.join("artwork")).unwrap();
        let host = Host { data, ..host };
        let libraries = no_libraries();

        let reports = declare_libraries(
            &boot(&host, libraries.clone()),
            &[declared("Everything", ".")],
        )
        .await;

        assert!(
            matches!(reports[0].outcome, Err(LibraryError::PathOverlapsDataDir)),
            "{reports:?}"
        );
        assert_eq!(stored(&libraries).await, vec![]);
    }
}
