use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::user_list::{UserList, UserListEntry, UserListPosition};
use crate::models::watch_state::TitleRef;

/// A user's watchlist and favourites (issue #262): movies and shows, one
/// entry per (user, list, title). Every addition stamps `added_at` from the
/// injected [`crate::services::Clock`].
#[async_trait]
pub trait UserListRepository: Send + Sync + std::fmt::Debug {
    /// Put `title` on the user's `list`, and return its entry. A title
    /// already there keeps its entry -- its place and when it was first
    /// added -- so adding is idempotent. Atomic: concurrent additions of one
    /// title leave one entry.
    async fn add(
        &self,
        user_id: Uuid,
        list: UserList,
        title: TitleRef,
    ) -> Result<UserListEntry, DbErr>;

    /// Take `title` off the user's `list`; whether it was there. Taking off
    /// a title not on it changes nothing.
    async fn remove(&self, user_id: Uuid, list: UserList, title: TitleRef) -> Result<bool, DbErr>;

    /// Which of the user's lists `title` is on, in [`UserList`]'s order.
    async fn lists_of(&self, user_id: Uuid, title: TitleRef) -> Result<Vec<UserList>, DbErr>;

    /// At most `limit` of the user's `list`, newest `(added_at, id)` first,
    /// starting after `after` -- the last entry of the page before.
    async fn find_page(
        &self,
        user_id: Uuid,
        list: UserList,
        after: Option<UserListPosition>,
        limit: u64,
    ) -> Result<Vec<UserListEntry>, DbErr>;

    /// How many titles are on the user's `list`.
    async fn count(&self, user_id: Uuid, list: UserList) -> Result<u64, DbErr>;

    /// Move every user's entries for `from` onto `to`: the indexer merged
    /// two movies, or two shows, into one, and the title retired would
    /// otherwise take the entries with it. A user with both on one list
    /// keeps one entry, added when the earlier of the two was. Atomic.
    /// `from` and `to` must be the same kind; the same title changes nothing.
    async fn carry(&self, from: TitleRef, to: TitleRef) -> Result<(), DbErr>;
}

/// Why [`UserListRepository::carry`] refuses a pair: a movie's entries
/// cannot become a show's.
pub fn carry_kind_mismatch() -> DbErr {
    DbErr::Custom("list entries are carried between two movies or two shows".to_string())
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build. See
/// [`crate::services::clock::in_memory`] for why the `#[mutants::skip]` is
/// required.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::services::Clock;
    use std::sync::{Arc, Mutex};

    /// In-memory stand-in for the SQL repository, stamping from the same
    /// injected [`Clock`], so the shared contract orders entries by
    /// advancing a [`crate::services::TestClock`].
    #[derive(Debug)]
    pub struct InMemoryUserListRepository {
        entries: Mutex<Vec<UserListEntry>>,
        clock: Arc<dyn Clock>,
    }

    impl InMemoryUserListRepository {
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self {
                entries: Mutex::new(Vec::new()),
                clock,
            }
        }
    }

    impl Default for InMemoryUserListRepository {
        fn default() -> Self {
            Self::new(Arc::new(crate::services::TestClock::new()))
        }
    }

    #[async_trait]
    impl UserListRepository for InMemoryUserListRepository {
        async fn add(
            &self,
            user_id: Uuid,
            list: UserList,
            title: TitleRef,
        ) -> Result<UserListEntry, DbErr> {
            let mut entries = self.entries.lock().unwrap();
            if let Some(entry) = entries
                .iter()
                .find(|e| e.user_id == user_id && e.list == list && e.title == title)
            {
                return Ok(*entry);
            }
            let entry = UserListEntry {
                id: Uuid::new_v4(),
                user_id,
                list,
                title,
                added_at: self.clock.now(),
            };
            entries.push(entry);
            Ok(entry)
        }

        async fn remove(
            &self,
            user_id: Uuid,
            list: UserList,
            title: TitleRef,
        ) -> Result<bool, DbErr> {
            let mut entries = self.entries.lock().unwrap();
            let before = entries.len();
            entries.retain(|e| !(e.user_id == user_id && e.list == list && e.title == title));
            Ok(entries.len() != before)
        }

        async fn lists_of(&self, user_id: Uuid, title: TitleRef) -> Result<Vec<UserList>, DbErr> {
            let mut lists: Vec<UserList> = self
                .entries
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.user_id == user_id && e.title == title)
                .map(|e| e.list)
                .collect();
            lists.sort();
            Ok(lists)
        }

        async fn find_page(
            &self,
            user_id: Uuid,
            list: UserList,
            after: Option<UserListPosition>,
            limit: u64,
        ) -> Result<Vec<UserListEntry>, DbErr> {
            let mut page: Vec<UserListEntry> = self
                .entries
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.user_id == user_id && e.list == list)
                .filter(|e| {
                    after.is_none_or(|after| (e.added_at, e.id) < (after.added_at, after.id))
                })
                .copied()
                .collect();
            page.sort_by(|a, b| (b.added_at, b.id).cmp(&(a.added_at, a.id)));
            page.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            Ok(page)
        }

        async fn count(&self, user_id: Uuid, list: UserList) -> Result<u64, DbErr> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.user_id == user_id && e.list == list)
                .count() as u64)
        }

        async fn carry(&self, from: TitleRef, to: TitleRef) -> Result<(), DbErr> {
            if std::mem::discriminant(&from) != std::mem::discriminant(&to) {
                return Err(carry_kind_mismatch());
            }
            if from == to {
                return Ok(());
            }
            let mut entries = self.entries.lock().unwrap();
            let moving: Vec<UserListEntry> = entries
                .iter()
                .filter(|e| e.title == from)
                .copied()
                .collect();
            entries.retain(|e| e.title != from);
            for mut entry in moving {
                match entries
                    .iter_mut()
                    .find(|e| e.user_id == entry.user_id && e.list == entry.list && e.title == to)
                {
                    Some(kept) => kept.added_at = kept.added_at.min(entry.added_at),
                    None => {
                        entry.title = to;
                        entries.push(entry);
                    }
                }
            }
            Ok(())
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::InMemoryUserListRepository;

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::UserListRepository;
    use super::in_memory::InMemoryUserListRepository;
    use crate::repositories::contract::fixture::UserListFixture;
    use crate::services::TestClock;

    /// The hermetic instantiation of the shared contract. The in-memory
    /// store enforces no referential integrity, so fresh v4 UUIDs are a valid
    /// user, movie and show; the Postgres fixture in `beam-index` inserts
    /// real rows for the same calls.
    pub struct InMemoryFixture {
        repo: InMemoryUserListRepository,
        clock: Arc<TestClock>,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            let clock = Arc::new(TestClock::new());
            Self {
                repo: InMemoryUserListRepository::new(clock.clone()),
                clock,
            }
        }
    }

    #[async_trait::async_trait]
    impl UserListFixture for InMemoryFixture {
        fn repo(&self) -> &dyn UserListRepository {
            &self.repo
        }

        fn clock(&self) -> &TestClock {
            &self.clock
        }

        async fn new_user(&self) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_movie(&self) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_show(&self) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::default()
    }

    crate::user_list_repository_contract!(setup);
}
