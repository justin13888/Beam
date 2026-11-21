//! The shared [`super::ShowRepository`] contract over the in-memory double.

use std::sync::Arc;

// `Uuid`, `ShowRepository` and `ShowRepositoryFixture` come into scope with
// the contract macro below.
use super::in_memory::InMemoryShowRepository;
use crate::models::show::Show;
use crate::repositories::file::in_memory::InMemoryFileRepository;
use crate::repositories::watch_state::in_memory::InMemoryWatchStateRepository;
use crate::repositories::{FileRepository, WatchStateRepository};

/// The hermetic instantiation of the shared contract: a show double linked
/// to the file and watch-state doubles the contract writes through.
#[derive(Debug)]
struct InMemoryFixture {
    repo: InMemoryShowRepository,
    files: Arc<InMemoryFileRepository>,
    watch_state: Arc<InMemoryWatchStateRepository>,
}

impl Default for InMemoryFixture {
    fn default() -> Self {
        let files = Arc::new(InMemoryFileRepository::default());
        let watch_state = Arc::new(InMemoryWatchStateRepository::default());
        Self {
            repo: InMemoryShowRepository::with_files(files.clone())
                .linking_watch_state(watch_state.clone()),
            files,
            watch_state,
        }
    }
}

#[async_trait::async_trait]
impl ShowRepositoryFixture for InMemoryFixture {
    fn repo(&self) -> &dyn ShowRepository {
        &self.repo
    }

    fn files(&self) -> &dyn FileRepository {
        self.files.as_ref()
    }

    fn watch_state(&self) -> &dyn WatchStateRepository {
        self.watch_state.as_ref()
    }

    async fn new_library(&self) -> Uuid {
        Uuid::new_v4()
    }

    async fn new_user(&self) -> Uuid {
        Uuid::new_v4()
    }

    async fn new_unkeyed_show(
        &self,
        title: &str,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Uuid {
        let now = created_at;
        let show = Show {
            id: Uuid::new_v4(),
            title: title.to_string(),
            identity_key: None,
            pinned_ref: None,
            pin_source: None,
            title_localized: None,
            description: None,
            year: None,
            poster_url: None,
            backdrop_url: None,
            tmdb_id: None,
            imdb_id: None,
            tvdb_id: None,
            anilist_id: None,
            rating_tmdb: None,
            content_rating: None,
            created_at: now,
            updated_at: now,
        };
        let id = show.id;
        self.repo.shows.lock().unwrap().insert(id, show);
        id
    }
}

async fn setup() -> InMemoryFixture {
    InMemoryFixture::default()
}

crate::show_repository_contract!(setup);
