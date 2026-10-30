//! What every test in the tier does the same way: start the stack, sign in,
//! add a library, scan it, and read back what the server and its database
//! now hold.

use std::path::Path;
use std::time::Duration;

use beam_entity::{files, library as library_entity, watch_state};
use beam_test_support::e2e::client::{SESSION_COOKIE, api, refused};
use beam_test_support::e2e::idp::ADMIN;
use beam_test_support::e2e::reqwest;
use beam_test_support::e2e::wait;
use beam_test_support::e2e::{Stack, StackOptions};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

pub use beam_test_support::e2e::client::status_of;
pub use beam_test_support::e2e::fixtures::{Container, Video, probe_fixture};
pub use beam_test_support::e2e::idp::{ADMIN as ADMIN_ACCOUNT, VIEWER};

/// How long one scan of a test library may take.
const SCAN_BOUND: Duration = Duration::from_secs(120);

/// The `beam-server` binary this test was built with.
pub fn server_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_beam-server"))
}

/// The workspace this package is in, for the committed probe fixtures.
pub fn workspace_root() -> std::path::PathBuf {
    beam_test_support::e2e::stack::workspace_root(env!("CARGO_MANIFEST_DIR"))
}

/// A stack with the suite's defaults.
pub async fn start() -> Stack {
    start_with(StackOptions::new()).await
}

/// A stack with `options` over the suite's defaults.
pub async fn start_with(options: StackOptions) -> Stack {
    Stack::start(server_binary(), options).await
}

/// A stack and an administrator signed in to it through the device grant.
pub struct Admin {
    pub stack: Stack,
    pub api: api::Client,
    pub session: String,
}

impl Admin {
    pub async fn start() -> Self {
        Self::start_with(StackOptions::new()).await
    }

    pub async fn start_with(options: StackOptions) -> Self {
        let stack = start_with(options).await;
        let session = stack.sign_in(ADMIN).await;
        let api = stack.client(&session);
        Self {
            stack,
            api,
            session,
        }
    }

    /// Add the library rooted at `root`, relative to the video directory.
    pub async fn add_library(&self, name: &str, root: &str) -> Uuid {
        let body = api::types::CreateLibraryRequest {
            name: name.to_owned(),
            root_path: root.to_owned(),
        };
        self.api
            .create_library(&body)
            .await
            .unwrap_or_else(|e| panic!("create the library {name}: {e:?}"))
            .into_inner()
            .id
    }

    /// Start a scan of `library` and wait for it to finish; return the job
    /// as it finished.
    pub async fn scan(&self, library: Uuid) -> api::types::ScanJob {
        let started = self
            .api
            .scan_library(library)
            .await
            .unwrap_or_else(|e| panic!("start a scan: {e:?}"));
        assert_eq!(
            started.status().as_u16(),
            202,
            "a scan is accepted, not run"
        );
        let job = started.into_inner();
        assert!(
            matches!(
                job.state,
                api::types::ScanState::Queued | api::types::ScanState::Running
            ),
            "a just-started scan is queued or running, not {:?}",
            job.state
        );
        self.finished_scan(library, job.id).await
    }

    /// Wait for the scan `job` of `library` to finish, and return it.
    pub async fn finished_scan(&self, library: Uuid, job: Uuid) -> api::types::ScanJob {
        wait::until("the scan to finish", SCAN_BOUND, || {
            let request = self.api.get_library_scan(library);
            async move {
                let current = request.await.map_err(|e| format!("{e:?}"))?.into_inner();
                assert_eq!(current.id, job, "another scan replaced the one awaited");
                match current.state {
                    api::types::ScanState::Succeeded | api::types::ScanState::Failed => Ok(current),
                    other => Err(format!("{other:?}")),
                }
            }
        })
        .await
    }

    /// Scan `library`, and assert the scan succeeded.
    pub async fn scan_ok(&self, library: Uuid) -> api::types::ScanProgress {
        let job = self.scan(library).await;
        assert_eq!(
            job.state,
            api::types::ScanState::Succeeded,
            "the scan succeeds: {:?}",
            job.failure
        );
        job.progress
    }

    /// Every present file of `library`, by its path relative to the root.
    pub async fn files(&self, library: Uuid) -> Vec<api::types::LibraryFile> {
        let mut files = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .api
                .get_library_files(
                    library,
                    api::GetLibraryFilesParams {
                        first: Some(500),
                        after: after.clone(),
                    },
                )
                .await
                .expect("list the library's files")
                .into_inner();
            files.extend(page.items);
            if !page.page_info.has_next_page {
                break;
            }
            after = page.page_info.end_cursor;
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }

    /// The present file at `path` in `library`.
    pub async fn file(&self, library: Uuid, path: &str) -> api::types::LibraryFile {
        self.files(library)
            .await
            .into_iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("no present file at {path}"))
    }

    /// Every title browse lists, by its original title.
    pub async fn titles(&self) -> Vec<Title> {
        browse(&self.api, None).await
    }

    /// The title browse lists as `name`.
    pub async fn title(&self, name: &str) -> Title {
        self.titles()
            .await
            .into_iter()
            .find(|title| title.name == name)
            .unwrap_or_else(|| panic!("browse lists no title {name:?}"))
    }

    /// End the test; see [`Stack::finish`].
    pub async fn finish(self) {
        self.stack.finish().await;
    }
}

/// What a browse result says of one title.
#[derive(Debug, Clone, PartialEq)]
pub struct Title {
    pub id: Uuid,
    pub name: String,
    pub year: Option<i64>,
    pub is_show: bool,
}

/// Every title browse lists for `query`, one page at a time.
pub async fn browse(client: &api::Client, query: Option<&str>) -> Vec<Title> {
    let mut titles = Vec::new();
    let mut after = None;
    loop {
        let params = api::BrowseMediaParams {
            first: Some(100),
            after: after.clone(),
            last: None,
            before: None,
            sort_by: None,
            sort_order: None,
            media_type: None,
            genre: None,
            year: None,
            year_from: None,
            year_to: None,
            query: query.map(str::to_owned),
            min_rating: None,
        };
        let page = client
            .browse_media(params)
            .await
            .expect("browse the catalogue")
            .into_inner();
        titles.extend(page.items.iter().map(title_of));
        if !page.page_info.has_next_page {
            break;
        }
        after = page.page_info.end_cursor;
    }
    titles.sort_by(|a, b| a.name.cmp(&b.name));
    titles
}

/// The summary of one browse or detail item.
pub fn title_of(item: &api::types::MediaMetadata) -> Title {
    match item {
        api::types::MediaMetadata::MediaMetadataVariant1(movie) => Title {
            id: movie.movie.id,
            name: movie.movie.title.original.clone(),
            year: movie.movie.year,
            is_show: false,
        },
        api::types::MediaMetadata::MediaMetadataVariant0(show) => Title {
            id: show.show.id,
            name: show.show.title.original.clone(),
            year: show.show.year,
            is_show: true,
        },
    }
}

/// The movie a detail response describes.
pub fn movie_of(item: api::types::MediaMetadata) -> api::types::MovieMetadata {
    match item {
        api::types::MediaMetadata::MediaMetadataVariant1(movie) => movie.movie,
        other => panic!("expected a movie, got {other:?}"),
    }
}

/// The show a detail response describes.
pub fn show_of(item: api::types::MediaMetadata) -> api::types::ShowMetadata {
    match item {
        api::types::MediaMetadata::MediaMetadataVariant0(show) => show.show,
        other => panic!("expected a show, got {other:?}"),
    }
}

/// Every `files` row of `library` the database holds, missing or not, by its
/// absolute path.
pub async fn file_rows(stack: &Stack, library: Uuid) -> Vec<files::Model> {
    let mut rows = files::Entity::find()
        .filter(files::Column::LibraryId.eq(library))
        .all(stack.db())
        .await
        .expect("read the files table");
    rows.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    rows
}

/// The `files` row with `id`, missing or not.
pub async fn file_row(stack: &Stack, id: Uuid) -> Option<files::Model> {
    files::Entity::find_by_id(id)
        .one(stack.db())
        .await
        .expect("read the files table")
}

/// The stored root of `library`.
pub async fn library_root(stack: &Stack, library: Uuid) -> String {
    library_entity::Entity::find_by_id(library)
        .one(stack.db())
        .await
        .expect("read the library table")
        .expect("the library is stored")
        .root_path
}

/// Every `watch_state` row the database holds.
pub async fn watch_rows(stack: &Stack) -> Vec<watch_state::Model> {
    watch_state::Entity::find()
        .all(stack.db())
        .await
        .expect("read the watch_state table")
}

/// Report `position_secs` into `file`, asserting the report is accepted.
pub async fn report(
    client: &api::Client,
    file: Uuid,
    position_secs: f64,
    duration_secs: Option<f64>,
) -> api::types::UserTitleState {
    let body = api::types::ReportProgressRequest {
        position_secs,
        duration_secs,
    };
    client
        .report_playback_progress(file, &body)
        .await
        .unwrap_or_else(|e| panic!("report {position_secs}s into {file}: {e:?}"))
        .into_inner()
}

/// The viewer's continue-watching shelf.
pub async fn continue_watching(client: &api::Client) -> Vec<api::types::ContinueWatchingItem> {
    client
        .get_continue_watching(api::GetContinueWatchingParams { first: Some(50) })
        .await
        .expect("read continue-watching")
        .into_inner()
        .items
}

/// `GET {base}{path}` outside the generated client -- for the byte-range
/// media endpoints it deliberately omits -- presenting `session` as the
/// cookie and `range` as the `Range` header.
pub async fn raw_get(
    base: &str,
    path: &str,
    session: Option<&str>,
    range: Option<&str>,
) -> reqwest::Response {
    let range: Vec<(&str, &str)> = range.map(|range| ("range", range)).into_iter().collect();
    raw_get_with(base, path, session, &range).await
}

/// `GET {base}{path}` outside the generated client, presenting `session` as
/// the cookie and each of `headers` as given: for the response headers and
/// the request headers -- `If-None-Match`, `X-Forwarded-For` -- the client
/// does not model.
pub async fn raw_get_with(
    base: &str,
    path: &str,
    session: Option<&str>,
    headers: &[(&str, &str)],
) -> reqwest::Response {
    let mut request = reqwest::Client::new().get(format!("{base}{path}"));
    if let Some(session) = session {
        request = request.header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE}={session}"),
        );
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request
        .send()
        .await
        .unwrap_or_else(|e| panic!("GET {path}: {e}"))
}

/// The value of `response`'s header `name`, if it has one in text.
pub fn header<'a>(response: &'a reqwest::Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|v| v.to_str().ok())
}

/// The first source of the title `media`.
pub async fn first_source(client: &api::Client, media: Uuid) -> api::types::MediaSource {
    client
        .get_media_sources(media)
        .await
        .unwrap_or_else(|e| panic!("read the sources of {media}: {e:?}"))
        .into_inner()
        .items
        .remove(0)
}

/// Assert `result` was refused with `status`.
pub fn assert_refused<T: std::fmt::Debug, E: std::fmt::Debug>(
    what: &str,
    status: u16,
    result: Result<T, api::Error<E>>,
) {
    assert_eq!(refused(what, result), status, "{what}");
}
