//! The stream-grant cache (issue #462): its expiry by the injected clock, its
//! bound, and its invalidation, then the same properties end to end through
//! `/v1/files/{file_id}/stream` with counting fakes below the trait line --
//! a run of Range requests reads the policy and the file once, and a revoked
//! session is refused whatever is cached.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use beam_auth::utils::session_store::{SessionData, SessionStore, in_memory::InMemorySessionStore};
use beam_domain::models::access::AccessPolicy;
use beam_domain::repositories::AccessPolicyRepository;
use beam_domain::repositories::access_policy::in_memory::InMemoryAccessPolicyRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::services::TestClock;
use kynos::http::StatusCode;
use kynos::prelude::*;
use kynos::test::TestClient;
use sea_orm::DbErr;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::routes::stream::stream_file;
use crate::services::access::{AccessRepositories, AccessService};
use crate::services::library::{LibraryError, LibraryFilePage, LibraryService};
use crate::state::{AppServices, AppState};

// ─── The cache itself ────────────────────────────────────────────────────

fn located(name: &str) -> LocatedFile {
    LocatedFile {
        id: Uuid::from_u128(0xF11E),
        path: PathBuf::from(format!("/media/{name}")),
        library_root: PathBuf::from("/media"),
        mime_type: Some("video/mp4".to_owned()),
    }
}

fn cache(clock: &Arc<TestClock>, capacity: usize) -> BoundedStreamGrantCache {
    BoundedStreamGrantCache::with_limits(
        clock.clone(),
        STREAM_GRANT_TTL,
        NonZeroUsize::new(capacity).expect("a non-zero capacity"),
    )
}

/// Remember `file` for `(user, file_id)` on a fresh miss's ticket.
fn grant(cache: &BoundedStreamGrantCache, user: Uuid, file_id: Uuid, file: LocatedFile) {
    let ticket = cache
        .lookup(user, file_id)
        .expect_err("nothing is held for this key yet");
    cache.remember(ticket, user, file_id, file);
}

const ALICE: Uuid = Uuid::from_u128(0xA11CE);
const BOB: Uuid = Uuid::from_u128(0xB0B);
const FILM: Uuid = Uuid::from_u128(1);
const SHOW: Uuid = Uuid::from_u128(2);

#[test]
fn a_grant_is_served_until_its_ttl_and_not_at_it() {
    let clock = Arc::new(TestClock::new());
    let cache = cache(&clock, 8);
    grant(&cache, ALICE, FILM, located("film.mkv"));

    clock.advance(STREAM_GRANT_TTL - Duration::from_millis(1));
    assert_eq!(cache.lookup(ALICE, FILM), Ok(located("film.mkv")));

    clock.advance(Duration::from_millis(1));
    assert!(
        cache.lookup(ALICE, FILM).is_err(),
        "a grant is stale once its TTL has passed"
    );
}

#[test]
fn a_grant_is_held_per_user_and_file() {
    let clock = Arc::new(TestClock::new());
    let cache = cache(&clock, 8);
    grant(&cache, ALICE, FILM, located("film.mkv"));

    assert!(cache.lookup(BOB, FILM).is_err(), "another user's grant");
    assert!(cache.lookup(ALICE, SHOW).is_err(), "another file's grant");
}

#[test]
fn forgetting_a_user_drops_only_their_grants() {
    let clock = Arc::new(TestClock::new());
    let cache = cache(&clock, 8);
    grant(&cache, ALICE, FILM, located("film.mkv"));
    grant(&cache, ALICE, SHOW, located("show.mkv"));
    grant(&cache, BOB, FILM, located("film.mkv"));

    cache.forget_user(ALICE);

    assert!(cache.lookup(ALICE, FILM).is_err());
    assert!(cache.lookup(ALICE, SHOW).is_err());
    assert_eq!(cache.lookup(BOB, FILM), Ok(located("film.mkv")));
}

/// A request that missed, then read a policy an administrator replaced
/// before it finished, must not store the decision it made from the old one.
#[test]
fn a_grant_resolved_before_a_forget_is_not_remembered() {
    let clock = Arc::new(TestClock::new());
    let cache = cache(&clock, 8);
    let ticket = cache.lookup(ALICE, FILM).expect_err("a miss");

    cache.forget_user(ALICE);
    cache.remember(ticket, ALICE, FILM, located("film.mkv"));

    assert!(cache.lookup(ALICE, FILM).is_err());
}

#[test]
fn past_capacity_the_grant_nearest_expiry_is_dropped() {
    let clock = Arc::new(TestClock::new());
    let cache = cache(&clock, 2);
    let third = Uuid::from_u128(3);
    grant(&cache, ALICE, FILM, located("film.mkv"));
    clock.advance(Duration::from_millis(10));
    grant(&cache, ALICE, SHOW, located("show.mkv"));
    clock.advance(Duration::from_millis(10));
    grant(&cache, ALICE, third, located("third.mkv"));

    assert!(cache.lookup(ALICE, FILM).is_err(), "the oldest grant went");
    assert_eq!(cache.lookup(ALICE, SHOW), Ok(located("show.mkv")));
    assert_eq!(cache.lookup(ALICE, third), Ok(located("third.mkv")));
}

/// Re-remembering a key already held takes no room, so it evicts nothing.
#[test]
fn renewing_a_held_grant_at_capacity_drops_nothing() {
    let clock = Arc::new(TestClock::new());
    let cache = cache(&clock, 2);
    grant(&cache, ALICE, FILM, located("film.mkv"));
    clock.advance(Duration::from_millis(10));
    grant(&cache, ALICE, SHOW, located("show.mkv"));

    // A ticket from a hit-free lookup on another key, then a renewal of the
    // older grant through it.
    let ticket = cache.lookup(BOB, FILM).expect_err("a miss");
    cache.remember(ticket, ALICE, FILM, located("film-renamed.mkv"));

    assert_eq!(cache.lookup(ALICE, FILM), Ok(located("film-renamed.mkv")));
    assert_eq!(cache.lookup(ALICE, SHOW), Ok(located("show.mkv")));
}

// ─── Through /stream ─────────────────────────────────────────────────────

/// The in-memory policy store, counting reads.
#[derive(Debug, Default)]
struct CountingPolicies {
    inner: InMemoryAccessPolicyRepository,
    finds: AtomicUsize,
}

#[async_trait::async_trait]
impl AccessPolicyRepository for CountingPolicies {
    async fn find(&self, user_id: Uuid) -> Result<AccessPolicy, DbErr> {
        self.finds.fetch_add(1, Ordering::SeqCst);
        self.inner.find(user_id).await
    }

    async fn replace(&self, user_id: Uuid, policy: &AccessPolicy) -> Result<(), DbErr> {
        self.inner.replace(user_id, policy).await
    }
}

/// One located file, counting lookups. Delivery calls nothing else.
#[derive(Debug)]
struct CountingLibrary {
    file: LocatedFile,
    lookups: AtomicUsize,
}

#[async_trait::async_trait]
impl LibraryService for CountingLibrary {
    async fn get_libraries(&self) -> Result<Vec<crate::models::Library>, LibraryError> {
        unimplemented!("not called by delivery")
    }
    async fn get_library_by_id(
        &self,
        _library_id: Uuid,
    ) -> Result<Option<crate::models::Library>, LibraryError> {
        unimplemented!("not called by delivery")
    }
    async fn get_library_files(
        &self,
        _library_id: Uuid,
        _after: Option<(String, Uuid)>,
        _first: std::num::NonZeroU32,
    ) -> Result<LibraryFilePage, LibraryError> {
        unimplemented!("not called by delivery")
    }
    async fn create_library(
        &self,
        _name: String,
        _root_path: String,
    ) -> Result<crate::models::Library, LibraryError> {
        unimplemented!("not called by delivery")
    }
    async fn declare_library(
        &self,
        _name: String,
        _root_path: String,
    ) -> Result<crate::services::library::LibraryDeclared, LibraryError> {
        unimplemented!("not called by delivery")
    }
    async fn start_scan(&self, _library_id: Uuid) -> Result<crate::models::ScanJob, LibraryError> {
        unimplemented!("not called by delivery")
    }
    async fn get_scan(
        &self,
        _library_id: Uuid,
    ) -> Result<Option<crate::models::ScanJob>, LibraryError> {
        unimplemented!("not called by delivery")
    }
    async fn delete_library(&self, _library_id: Uuid) -> Result<bool, LibraryError> {
        unimplemented!("not called by delivery")
    }
    async fn get_file_by_id(&self, file_id: Uuid) -> Result<Option<LocatedFile>, LibraryError> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        Ok((file_id == self.file.id).then(|| self.file.clone()))
    }
}

const CONTENTS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

struct Delivery {
    _dir: TempDir,
    clock: Arc<TestClock>,
    policies: Arc<CountingPolicies>,
    library: Arc<CountingLibrary>,
    sessions: Arc<InMemorySessionStore>,
    state: AppState,
    client: TestClient<AppState>,
    user_id: Uuid,
    token: String,
}

impl Delivery {
    async fn new() -> Self {
        let dir = TempDir::new().expect("a temp dir");
        let path = dir.path().join("film.mkv");
        std::fs::write(&path, CONTENTS).expect("write the film");

        let clock = Arc::new(TestClock::new());
        let policies = Arc::new(CountingPolicies::default());
        let library = Arc::new(CountingLibrary {
            file: LocatedFile {
                id: FILM,
                path,
                library_root: dir.path().to_owned(),
                mime_type: Some("video/x-matroska".to_owned()),
            },
            lookups: AtomicUsize::new(0),
        });
        let sessions = Arc::new(InMemorySessionStore::default());
        let access = AccessService::new(AccessRepositories {
            policies: policies.clone(),
            movies: Arc::new(InMemoryMovieRepository::default()),
            shows: Arc::new(InMemoryShowRepository::default()),
            files: Arc::new(InMemoryFileRepository::default()),
        })
        .with_stream_grants(Arc::new(BoundedStreamGrantCache::new(clock.clone())));

        let services = AppServices {
            access: Arc::new(access),
            library: library.clone(),
            session_store: sessions.clone(),
            ..crate::routes::test_support::idle_services()
        };
        let state = AppState::with_clock(
            crate::config::ServerConfig {
                cookie_secure: Some(false),
                ..Default::default()
            },
            services,
            Arc::new(crate::services::health::InMemoryDependencyProbe::healthy()),
            clock.clone(),
            None,
        );
        let router = Router::new()
            .nest("/v1", Router::new().mount(kynos::routes![stream_file]))
            .build(state.clone())
            .expect("the stream router describes itself");

        let user_id = Uuid::from_u128(0xA11CE);
        let token = sessions
            .create(
                &SessionData {
                    user_id: user_id.to_string(),
                    device_hash: "test-device".to_owned(),
                    ip: "127.0.0.1".to_owned(),
                    created_at: chrono::Utc::now().timestamp(),
                    last_active: chrono::Utc::now().timestamp(),
                },
                86_400,
                86_400,
            )
            .await
            .expect("the in-memory session store issues a session");

        Self {
            _dir: dir,
            clock,
            policies,
            library,
            sessions,
            state,
            client: TestClient::new(router),
            user_id,
            token,
        }
    }

    /// `Range: bytes={first}-{last}` of `file_id`'s stream.
    async fn range(&self, file_id: Uuid, first: usize, last: usize) -> kynos::test::TestResponse {
        self.client
            .get(&format!("/v1/files/{file_id}/stream"))
            .cookie("beam_session", &self.token)
            .header("Range", &format!("bytes={first}-{last}"))
            .send()
            .await
    }

    /// Asserts a ranged request for the film encloses exactly that span.
    async fn assert_part(&self, first: usize, last: usize) {
        let response = self.range(FILM, first, last).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.bytes().as_ref(), &CONTENTS[first..=last]);
    }

    /// (policy reads, file lookups) so far.
    fn reads(&self) -> (usize, usize) {
        (
            self.policies.finds.load(Ordering::SeqCst),
            self.library.lookups.load(Ordering::SeqCst),
        )
    }
}

#[tokio::test]
async fn ranged_requests_within_the_ttl_read_the_policy_and_the_file_once() {
    let delivery = Delivery::new().await;

    for first in [0, 4, 8, 12, 16] {
        delivery.assert_part(first, first + 3).await;
    }

    assert_eq!(delivery.reads(), (1, 1));
}

#[tokio::test]
async fn past_the_ttl_the_next_request_resolves_again() {
    let delivery = Delivery::new().await;
    delivery.assert_part(0, 3).await;

    delivery.clock.advance(STREAM_GRANT_TTL);
    delivery.assert_part(4, 7).await;

    assert_eq!(delivery.reads(), (2, 2));
}

#[tokio::test]
async fn a_policy_change_makes_the_next_request_resolve_again() {
    let delivery = Delivery::new().await;
    delivery.assert_part(0, 3).await;

    delivery
        .state
        .services
        .access
        .replace(delivery.user_id, &AccessPolicy::unrestricted())
        .await
        .expect("store the policy");
    delivery.assert_part(4, 7).await;

    assert_eq!(delivery.reads(), (2, 2));
}

/// The session is resolved on every request, cache or no cache.
#[tokio::test]
async fn a_revoked_session_is_refused_while_its_grant_is_cached() {
    let delivery = Delivery::new().await;
    delivery.assert_part(0, 3).await;

    delivery
        .sessions
        .delete(&delivery.token)
        .await
        .expect("revoke the session");
    let refused = delivery.range(FILM, 4, 7).await;

    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
}

/// Only a grant is held: a file the library does not have is looked up
/// again, so one the next scan finds is served on the next request.
#[tokio::test]
async fn a_refusal_is_resolved_afresh_each_time() {
    let delivery = Delivery::new().await;
    let unknown = Uuid::from_u128(0xDEAD);

    for _ in 0..2 {
        let response = delivery.range(unknown, 0, 3).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    assert_eq!(delivery.reads(), (2, 2));
}
