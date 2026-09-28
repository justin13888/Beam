//! Subcutaneous tests for `/v1/files/{file_id}/subtitles/{subtitle_id}` and
//! its `/webvtt` rendition (issue #189).
//!
//! Real router, real handlers, the real `DbSubtitleService`, and real subtitle
//! files in a `TempDir`; only the index below the repository traits is a
//! double. What these assert is what a player receives: the bytes, their
//! type, and which problem a failure names.

use std::path::PathBuf;
use std::sync::Arc;

use beam_auth::utils::session_store::SessionData;
use beam_domain::models::sidecar::{SidecarInfo, SubtitleFormat, UpsertSidecarSubtitle};
use beam_domain::models::{FileStatus, MediaFile, MediaFileContent};
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::sidecar_subtitle::in_memory::InMemorySidecarSubtitleRepository;
use beam_domain::repositories::{FileRepository, SidecarSubtitleRepository};
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use tempfile::TempDir;
use uuid::Uuid;

use crate::routes::subtitle::{get_subtitle, get_subtitle_webvtt};
use crate::routes::test_support::make_app_state;
use crate::services::sources::SUBTITLE_CONVERT_MAX_BYTES;
use crate::services::subtitle::DbSubtitleService;
use crate::state::{AppServices, AppState};

const SRT: &[u8] = b"1\r\n00:00:01,000 --> 00:00:02,500\r\n<i>Hello</i> & welcome\r\n\r\n2\r\n00:01:00,000 --> 00:01:01,000\r\nBye\r\n";
const SRT_AS_WEBVTT: &str = "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\n<i>Hello</i> &amp; welcome\n\n2\n00:01:00.000 --> 00:01:01.000\nBye\n";

/// The state, the index doubles behind its subtitle service, and the
/// directory the subtitle files live in.
struct Fixture {
    state: AppState,
    files: Arc<InMemoryFileRepository>,
    sidecars: Arc<InMemorySidecarSubtitleRepository>,
    dir: TempDir,
}

fn fixture() -> Fixture {
    let base = make_app_state();
    let files = Arc::new(InMemoryFileRepository::default());
    let sidecars = Arc::new(InMemorySidecarSubtitleRepository::default());

    let services = AppServices {
        hash: base.services.hash.clone(),
        library: base.services.library.clone(),
        metadata: base.services.metadata.clone(),
        subtitles: Arc::new(DbSubtitleService::new(files.clone(), sidecars.clone())),
        notification: base.services.notification.clone(),
        admin_log: base.services.admin_log.clone(),
        user_repo: base.services.user_repo.clone(),
        playback: base.services.playback.clone(),
        genre_repo: base.services.genre_repo.clone(),
        library_repo: base.services.library_repo.clone(),
        file_repo: files.clone(),
        enrichment_repo: base.services.enrichment_repo.clone(),
        movie_repo: base.services.movie_repo.clone(),
        show_repo: base.services.show_repo.clone(),
        artwork: base.services.artwork.clone(),
        session_store: base.services.session_store.clone(),
        oidc_client: base.services.oidc_client.clone(),
        pending_auth_store: base.services.pending_auth_store.clone(),
        device_auth_store: base.services.device_auth_store.clone(),
        oidc_config: base.services.oidc_config.clone(),
        watch_status: Arc::new(beam_index::services::watch_status::WatchStatus::new()),
        telemetry: crate::routes::test_support::idle_library_report(),
        playback_telemetry: crate::routes::test_support::idle_playback_telemetry(),
    };

    Fixture {
        state: AppState::new(base.config.clone(), services, base.probe.clone(), None),
        files,
        sidecars,
        dir: TempDir::new().expect("a temp dir"),
    }
}

impl Fixture {
    fn client(&self) -> TestClient<AppState> {
        let service = Router::new()
            .nest(
                "/v1",
                Router::new().mount(kynos::routes![get_subtitle, get_subtitle_webvtt]),
            )
            .build(self.state.clone())
            .expect("the subtitle router describes itself");
        TestClient::new(service)
    }

    async fn session(&self) -> String {
        self.state
            .services
            .session_store
            .create(
                &SessionData {
                    user_id: Uuid::new_v4().to_string(),
                    device_hash: "test-device".to_owned(),
                    ip: "127.0.0.1".to_owned(),
                    created_at: chrono::Utc::now().timestamp(),
                    last_active: chrono::Utc::now().timestamp(),
                },
                86_400,
                86_400,
            )
            .await
            .expect("the in-memory session store issues a session")
    }

    /// A present video file row.
    fn video(&self) -> Uuid {
        let file = MediaFile {
            id: Uuid::new_v4(),
            library_id: Uuid::new_v4(),
            path: self.dir.path().join("Movie.mkv"),
            hash: 0,
            size_bytes: 1,
            mtime: None,
            mime_type: None,
            duration: None,
            container_format: None,
            content: Some(MediaFileContent::Movie {
                movie_entry_id: Uuid::new_v4(),
            }),
            status: FileStatus::Known,
            scanned_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            missing_since: None,
            classifier_version: 0,
            container_tags: None,
            identity: None,
        };
        let id = file.id;
        self.files.files.lock().unwrap().insert(file.id, file);
        id
    }

    /// A subtitle file `name` holding `bytes`, indexed beside `file_id`.
    async fn sidecar(&self, file_id: Uuid, name: &str, bytes: &[u8]) -> Uuid {
        let path = self.dir.path().join(name);
        std::fs::write(&path, bytes).expect("write the subtitle");
        let format = SubtitleFormat::from_extension(
            path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default(),
        )
        .expect("a subtitle extension");
        self.sidecars
            .upsert_by_path(UpsertSidecarSubtitle {
                file_id,
                library_id: Uuid::new_v4(),
                path,
                info: SidecarInfo {
                    format,
                    language: Some("eng".to_owned()),
                    title: None,
                    is_forced: false,
                    is_sdh: false,
                    is_default: false,
                },
                size_bytes: bytes.len() as u64,
                mtime: None,
            })
            .await
            .expect("index the subtitle")
            .id
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

fn problem_type(body: &[u8]) -> String {
    let problem: serde_json::Value = serde_json::from_slice(body).expect("a problem document");
    problem["type"].as_str().unwrap_or_default().to_owned()
}

fn url(file_id: Uuid, subtitle_id: Uuid) -> String {
    format!("/v1/files/{file_id}/subtitles/{subtitle_id}")
}

#[tokio::test]
async fn a_subrip_file_is_served_as_stored_with_its_own_type() {
    let f = fixture();
    let video = f.video();
    let srt = f.sidecar(video, "Movie.en.srt", SRT).await;
    let token = f.session().await;

    let response = f
        .client()
        .get(&url(video, srt))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.header("content-type"),
        Some("application/x-subrip")
    );
    assert_eq!(response.bytes().as_ref(), SRT);
}

#[tokio::test]
async fn each_format_is_served_as_stored_with_its_type() {
    let f = fixture();
    let video = f.video();
    let token = f.session().await;
    for (name, content_type) in [
        ("Movie.en.vtt", "text/vtt"),
        ("Movie.en.ass", "text/x-ass"),
        ("Movie.en.ssa", "text/x-ssa"),
    ] {
        let id = f.sidecar(video, name, b"[Script Info]\n").await;
        let response = f
            .client()
            .get(&url(video, id))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        assert_eq!(
            response.header("content-type"),
            Some(content_type),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_range_of_a_subtitle_is_served_as_a_part() {
    let f = fixture();
    let video = f.video();
    let srt = f.sidecar(video, "Movie.en.srt", SRT).await;
    let token = f.session().await;

    let response = f
        .client()
        .get(&url(video, srt))
        .cookie("beam_session", &token)
        .header("range", "bytes=3-12")
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.header("content-range"),
        Some(format!("bytes 3-12/{}", SRT.len()).as_str())
    );
    assert_eq!(response.bytes().as_ref(), &SRT[3..=12]);
}

#[tokio::test]
async fn a_subrip_file_is_served_as_webvtt_and_revalidates() {
    let f = fixture();
    let video = f.video();
    let srt = f.sidecar(video, "Movie.en.srt", SRT).await;
    let token = f.session().await;
    let client = f.client();

    let response = client
        .get(&format!("{}/webvtt", url(video, srt)))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.header("content-type"),
        Some("text/vtt; charset=utf-8")
    );
    assert_eq!(response.text(), SRT_AS_WEBVTT);
    let etag = response.header("etag").expect("a validator").to_owned();

    let stored = client
        .get(&url(video, srt))
        .cookie("beam_session", &token)
        .send()
        .await;
    assert_ne!(
        stored.header("etag"),
        Some(etag.as_str()),
        "the rendition and the file are different representations"
    );

    let again = client
        .get(&format!("{}/webvtt", url(video, srt)))
        .cookie("beam_session", &token)
        .header("if-none-match", &etag)
        .send()
        .await;
    assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn a_webvtt_file_is_normalised_to_utf8_with_lf_line_ends() {
    let f = fixture();
    let video = f.video();
    let vtt = f
        .sidecar(
            video,
            "Movie.en.vtt",
            b"\xEF\xBB\xBFWEBVTT\r\n\r\n00:01.000 --> 00:02.000\r\nHi",
        )
        .await;
    let token = f.session().await;

    let response = f
        .client()
        .get(&format!("{}/webvtt", url(video, vtt)))
        .cookie("beam_session", &token)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text(), "WEBVTT\n\n00:01.000 --> 00:02.000\nHi\n");
}

#[tokio::test]
async fn ass_and_an_oversized_file_have_no_webvtt_rendition() {
    let f = fixture();
    let video = f.video();
    let ass = f.sidecar(video, "Movie.en.ass", b"[Script Info]\n").await;
    // Indexed small, then grown past the ceiling: the file as it is now
    // decides.
    let grown = f.sidecar(video, "Movie.fr.srt", SRT).await;
    std::fs::write(
        f.path("Movie.fr.srt"),
        vec![b'a'; SUBTITLE_CONVERT_MAX_BYTES as usize + 1],
    )
    .expect("grow the subtitle");
    let token = f.session().await;

    for id in [ass, grown] {
        let response = f
            .client()
            .get(&format!("{}/webvtt", url(video, id)))
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            problem_type(response.bytes()),
            "https://beam.justinchung.net/reference/errors/#subtitle-rendition-unavailable"
        );
    }
}

#[tokio::test]
async fn a_subtitle_is_found_only_beside_its_own_present_video() {
    let f = fixture();
    let video = f.video();
    let other = f.video();
    let gone = f.video();
    let srt = f.sidecar(video, "Movie.en.srt", SRT).await;
    let others = f.sidecar(other, "Other.en.srt", SRT).await;
    let of_gone = f.sidecar(gone, "Gone.en.srt", SRT).await;
    f.files
        .mark_missing(vec![gone], chrono::Utc::now())
        .await
        .unwrap();
    let token = f.session().await;

    let cases = [
        (
            url(video, others),
            "subtitle-not-found",
            "another file's subtitle",
        ),
        (
            url(video, Uuid::new_v4()),
            "subtitle-not-found",
            "an unknown subtitle",
        ),
        (
            url(Uuid::new_v4(), srt),
            "file-not-found",
            "an unknown file",
        ),
        (
            url(gone, of_gone),
            "file-not-found",
            "a video gone from disk",
        ),
        (
            format!("{}/webvtt", url(gone, of_gone)),
            "file-not-found",
            "a video gone from disk, as WebVTT",
        ),
    ];
    for (path, code, why) in cases {
        let response = f
            .client()
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{why}");
        assert_eq!(
            problem_type(response.bytes()),
            format!("https://beam.justinchung.net/reference/errors/#{code}"),
            "{why}"
        );
    }
}

#[tokio::test]
async fn a_subtitle_deleted_from_disk_is_source_file_missing() {
    let f = fixture();
    let video = f.video();
    let srt = f.sidecar(video, "Movie.en.srt", SRT).await;
    std::fs::remove_file(f.path("Movie.en.srt")).expect("delete the subtitle");
    let token = f.session().await;

    for path in [url(video, srt), format!("{}/webvtt", url(video, srt))] {
        let response = f
            .client()
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            problem_type(response.bytes()),
            "https://beam.justinchung.net/reference/errors/#source-file-missing",
            "{path}"
        );
    }
}

/// A subtitle replaced since it was indexed -- by a symbolic link out of the
/// library, or by a FIFO -- is not read (FR-212): neither route serves the
/// link's target, and neither waits on the FIFO for a writer. Both are
/// `#source-file-missing`, as a deleted file is.
#[cfg(unix)]
#[tokio::test]
async fn a_subtitle_replaced_by_a_link_or_a_fifo_is_source_file_missing() {
    const SECRET: &[u8] = b"1\n00:00:01,000 --> 00:00:02,000\nDATABASE_URL=postgres://secret\n";
    let f = fixture();
    let video = f.video();
    let outside = f.path("outside.srt");
    std::fs::write(&outside, SECRET).expect("write the file outside");

    let mut replaced = Vec::new();
    for name in ["Movie.en.srt", "Movie.fr.vtt"] {
        let id = f.sidecar(video, name, SRT).await;
        std::fs::remove_file(f.path(name)).expect("remove the subtitle");
        std::os::unix::fs::symlink(&outside, f.path(name)).expect("link it out");
        replaced.push((name, id));
    }
    #[cfg(target_os = "linux")]
    {
        let id = f.sidecar(video, "Movie.de.srt", SRT).await;
        std::fs::remove_file(f.path("Movie.de.srt")).expect("remove the subtitle");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            f.path("Movie.de.srt"),
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .expect("make a FIFO");
        replaced.push(("Movie.de.srt", id));
    }
    let token = f.session().await;

    for (name, id) in replaced {
        for path in [url(video, id), format!("{}/webvtt", url(video, id))] {
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                f.client().get(&path).cookie("beam_session", &token).send(),
            )
            .await
            .unwrap_or_else(|_| panic!("{name}: {path} waited on the file"));
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{name}: {path}");
            assert_eq!(
                problem_type(response.bytes()),
                "https://beam.justinchung.net/reference/errors/#source-file-missing",
                "{name}: {path}"
            );
        }
    }
}

#[tokio::test]
async fn a_malformed_id_is_refused_before_any_lookup() {
    let f = fixture();
    let video = f.video();
    let token = f.session().await;

    for path in [
        format!("/v1/files/not-a-uuid/subtitles/{}", Uuid::new_v4()),
        format!("/v1/files/{video}/subtitles/not-a-uuid/webvtt"),
    ] {
        let response = f
            .client()
            .get(&path)
            .cookie("beam_session", &token)
            .send()
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
}

#[tokio::test]
async fn subtitles_require_a_session() {
    let f = fixture();
    let video = f.video();
    let srt = f.sidecar(video, "Movie.en.srt", SRT).await;

    for path in [url(video, srt), format!("{}/webvtt", url(video, srt))] {
        let response = f.client().get(&path).send().await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
}
