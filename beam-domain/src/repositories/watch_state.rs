use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::watch_state::{
    ContinueCandidate, HistoryPosition, RecordProgress, TitleRef, WatchState, WatchTarget,
};

/// Per-user watched state, one row per (user, movie) or (user, episode)
/// (issue #188). Every write stamps `last_played_at` from the injected
/// [`crate::services::Clock`].
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait WatchStateRepository: Send + Sync + std::fmt::Debug {
    /// Record a report against its title, creating the row on the first.
    ///
    /// A report that reaches the end ([`RecordProgress::reaches_end`]) marks
    /// the title played at position 0 and counts a play -- unless the row
    /// already sat played at 0, so repeated reports past the end count once.
    /// Any other report moves the position and leaves played as it was: a
    /// rewind never unplays. Atomic: concurrent reports for one title leave
    /// one row.
    async fn record_progress(&self, report: RecordProgress) -> Result<WatchState, DbErr>;

    /// Mark every one of `targets` played, as a report reaching the end
    /// would, all stamped with one `last_played_at`. A target already played
    /// and at its start is left exactly as it is, so marking is idempotent.
    /// An empty slice writes nothing.
    async fn mark_played(&self, user_id: Uuid, targets: &[WatchTarget]) -> Result<(), DbErr>;

    /// Forget `targets` entirely: played, play count and position.
    async fn mark_unplayed(&self, user_id: Uuid, targets: &[WatchTarget]) -> Result<(), DbErr>;

    /// Drop the resume position of `target`. A title never played is
    /// forgotten; a played one stays played, back at its start.
    async fn clear_progress(&self, user_id: Uuid, target: WatchTarget) -> Result<(), DbErr>;

    /// Hide `title` -- a movie, or every episode of a show -- from
    /// continue-watching until it is next played.
    async fn dismiss(&self, user_id: Uuid, title: TitleRef) -> Result<(), DbErr>;

    /// Move every user's row for `from` onto `to`: the indexer merged two
    /// movies, or two shows' episodes, into one, and the title retired would
    /// otherwise take its viewers' state with it. A user with a row for both
    /// keeps one: the more recently played row's position, duration and file,
    /// played if either was, the plays of both, and the later dismissal.
    /// Atomic. `from` and `to` must be the same kind; the same target changes
    /// nothing.
    async fn carry(&self, from: WatchTarget, to: WatchTarget) -> Result<(), DbErr>;

    async fn find(&self, user_id: Uuid, target: WatchTarget) -> Result<Option<WatchState>, DbErr>;

    /// The rows for those of `movie_ids` the user has any. One statement,
    /// none for an empty slice.
    async fn find_for_movies(
        &self,
        user_id: Uuid,
        movie_ids: &[Uuid],
    ) -> Result<Vec<WatchState>, DbErr>;

    /// The rows for every episode of `show_id` the user has any.
    async fn find_for_show(&self, user_id: Uuid, show_id: Uuid) -> Result<Vec<WatchState>, DbErr>;

    /// The rows for every episode of each of `show_ids` the user has any.
    /// One statement, none for an empty slice.
    async fn find_for_shows(
        &self,
        user_id: Uuid,
        show_ids: &[Uuid],
    ) -> Result<Vec<WatchState>, DbErr>;

    /// At most `limit` of the titles continue-watching considers, one per
    /// movie or show, newest `(last_played_at, title id)` first, starting
    /// after `after` -- the last candidate of the page before.
    ///
    /// A title is considered when it was played after it was last
    /// dismissed, and -- for a movie -- it has a position to resume from. A
    /// show is considered even when every episode is played, because what it
    /// offers is the *next* episode; whether there is one is not the store's
    /// to know.
    async fn find_continue_candidates(
        &self,
        user_id: Uuid,
        after: Option<ContinueCandidate>,
        limit: u64,
    ) -> Result<Vec<ContinueCandidate>, DbErr>;

    /// One page of the user's rows, newest `(last_played_at, id)` first,
    /// starting after `after`.
    async fn find_history_page(
        &self,
        user_id: Uuid,
        after: Option<HistoryPosition>,
        limit: u64,
    ) -> Result<Vec<WatchState>, DbErr>;

    /// How many rows the user has: every title of their history.
    async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DbErr>;

    /// When each of `file_ids` was last played, by anyone: the latest
    /// `last_played_at` among the rows whose last report named it. A file no
    /// row names is absent. The indexer breaks a tie between identical copies
    /// of a moved file with it (issue #180), so the copy someone is watching
    /// keeps the file.
    async fn last_played_at(
        &self,
        file_ids: Vec<Uuid>,
    ) -> Result<HashMap<Uuid, DateTime<Utc>>, DbErr>;
}

/// Test doubles. Gated behind `test-utils` so downstream crates can depend on
/// them without them reaching a release build. See
/// [`crate::services::clock::in_memory`] for why the `#[mutants::skip]` is
/// required.
#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use crate::models::watch_state::Played;
    use crate::services::Clock;
    use std::sync::{Arc, Mutex};

    /// In-memory stand-in for the SQL repository, stamping from the same
    /// injected [`Clock`], so the shared contract in [`super::contract`]
    /// orders rows by advancing a [`crate::services::TestClock`].
    #[derive(Debug)]
    pub struct InMemoryWatchStateRepository {
        rows: Mutex<Vec<WatchState>>,
        clock: Arc<dyn Clock>,
    }

    impl InMemoryWatchStateRepository {
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self {
                rows: Mutex::new(Vec::new()),
                clock,
            }
        }
    }

    impl Default for InMemoryWatchStateRepository {
        fn default() -> Self {
            Self::new(Arc::new(crate::services::TestClock::new()))
        }
    }

    /// The unique key a row is found by: the movie or the episode.
    fn key(target: WatchTarget) -> Uuid {
        match target {
            WatchTarget::Movie { movie_id } => movie_id,
            WatchTarget::Episode { episode_id, .. } => episode_id,
        }
    }

    fn played(row: &WatchState) -> Played {
        Played {
            position_secs: row.position_secs,
            completed: row.completed,
            play_count: row.play_count,
        }
    }

    fn fresh(user_id: Uuid, target: WatchTarget, now: DateTime<Utc>) -> WatchState {
        WatchState {
            id: Uuid::new_v4(),
            user_id,
            target,
            last_file_id: None,
            position_secs: 0.0,
            duration_secs: None,
            completed: false,
            play_count: 0,
            last_played_at: now,
            dismissed_at: None,
        }
    }

    fn title_of(row: &WatchState) -> TitleRef {
        row.target.title()
    }

    #[async_trait]
    impl WatchStateRepository for InMemoryWatchStateRepository {
        async fn record_progress(&self, report: RecordProgress) -> Result<WatchState, DbErr> {
            let now = self.clock.now();
            let mut rows = self.rows.lock().unwrap();
            let index = match rows
                .iter()
                .position(|r| r.user_id == report.user_id && key(r.target) == key(report.target))
            {
                Some(index) => index,
                None => {
                    rows.push(fresh(report.user_id, report.target, now));
                    rows.len() - 1
                }
            };
            let row = &mut rows[index];
            let before = played(row);
            let after = if report.reaches_end() {
                before.finished()
            } else {
                before.at(report.position_secs)
            };
            row.position_secs = after.position_secs;
            row.completed = after.completed;
            row.play_count = after.play_count;
            row.last_file_id = Some(report.file_id);
            row.duration_secs = report.duration_secs.or(row.duration_secs);
            row.last_played_at = now;
            Ok(row.clone())
        }

        async fn mark_played(&self, user_id: Uuid, targets: &[WatchTarget]) -> Result<(), DbErr> {
            let now = self.clock.now();
            let mut rows = self.rows.lock().unwrap();
            for target in targets {
                match rows
                    .iter_mut()
                    .find(|r| r.user_id == user_id && key(r.target) == key(*target))
                {
                    Some(row) => {
                        if row.is_finished() {
                            continue;
                        }
                        let after = played(row).finished();
                        row.position_secs = after.position_secs;
                        row.completed = after.completed;
                        row.play_count = after.play_count;
                        row.last_played_at = now;
                    }
                    None => {
                        let mut row = fresh(user_id, *target, now);
                        let after = Played::NEVER.finished();
                        row.completed = after.completed;
                        row.play_count = after.play_count;
                        rows.push(row);
                    }
                }
            }
            Ok(())
        }

        async fn mark_unplayed(&self, user_id: Uuid, targets: &[WatchTarget]) -> Result<(), DbErr> {
            let keys: Vec<Uuid> = targets.iter().map(|t| key(*t)).collect();
            self.rows
                .lock()
                .unwrap()
                .retain(|r| !(r.user_id == user_id && keys.contains(&key(r.target))));
            Ok(())
        }

        async fn clear_progress(&self, user_id: Uuid, target: WatchTarget) -> Result<(), DbErr> {
            let mut rows = self.rows.lock().unwrap();
            rows.retain(|r| {
                !(r.user_id == user_id && key(r.target) == key(target) && !r.completed)
            });
            for row in rows
                .iter_mut()
                .filter(|r| r.user_id == user_id && key(r.target) == key(target))
            {
                row.position_secs = 0.0;
            }
            Ok(())
        }

        async fn dismiss(&self, user_id: Uuid, title: TitleRef) -> Result<(), DbErr> {
            let now = self.clock.now();
            for row in self
                .rows
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|r| r.user_id == user_id && title_of(r) == title)
            {
                row.dismissed_at = Some(now);
            }
            Ok(())
        }

        async fn carry(&self, from: WatchTarget, to: WatchTarget) -> Result<(), DbErr> {
            if std::mem::discriminant(&from) != std::mem::discriminant(&to) {
                return Err(DbErr::Custom(
                    "watch state is carried between two movies or two episodes".to_string(),
                ));
            }
            if key(from) == key(to) {
                return Ok(());
            }
            let mut rows = self.rows.lock().unwrap();
            let moving: Vec<WatchState> = rows
                .iter()
                .filter(|r| key(r.target) == key(from))
                .cloned()
                .collect();
            rows.retain(|r| key(r.target) != key(from));
            for mut row in moving {
                match rows
                    .iter_mut()
                    .find(|r| r.user_id == row.user_id && key(r.target) == key(to))
                {
                    Some(kept) => {
                        if row.last_played_at > kept.last_played_at {
                            kept.position_secs = row.position_secs;
                            kept.duration_secs = row.duration_secs.or(kept.duration_secs);
                            kept.last_file_id = row.last_file_id.or(kept.last_file_id);
                            kept.last_played_at = row.last_played_at;
                        }
                        kept.completed |= row.completed;
                        kept.play_count += row.play_count;
                        kept.dismissed_at = kept.dismissed_at.max(row.dismissed_at);
                    }
                    None => {
                        row.target = to;
                        rows.push(row);
                    }
                }
            }
            Ok(())
        }

        async fn find(
            &self,
            user_id: Uuid,
            target: WatchTarget,
        ) -> Result<Option<WatchState>, DbErr> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.user_id == user_id && key(r.target) == key(target))
                .cloned())
        }

        async fn find_for_movies(
            &self,
            user_id: Uuid,
            movie_ids: &[Uuid],
        ) -> Result<Vec<WatchState>, DbErr> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|r| {
                    r.user_id == user_id
                        && matches!(r.target, WatchTarget::Movie { movie_id } if movie_ids.contains(&movie_id))
                })
                .cloned()
                .collect())
        }

        async fn find_for_show(
            &self,
            user_id: Uuid,
            show_id: Uuid,
        ) -> Result<Vec<WatchState>, DbErr> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.user_id == user_id && title_of(r) == TitleRef::Show(show_id))
                .cloned()
                .collect())
        }

        async fn find_for_shows(
            &self,
            user_id: Uuid,
            show_ids: &[Uuid],
        ) -> Result<Vec<WatchState>, DbErr> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|r| {
                    r.user_id == user_id
                        && matches!(r.target, WatchTarget::Episode { show_id, .. } if show_ids.contains(&show_id))
                })
                .cloned()
                .collect())
        }

        async fn find_continue_candidates(
            &self,
            user_id: Uuid,
            after: Option<ContinueCandidate>,
            limit: u64,
        ) -> Result<Vec<ContinueCandidate>, DbErr> {
            let rows = self.rows.lock().unwrap();
            let mut titles: Vec<(TitleRef, DateTime<Utc>)> = Vec::new();
            let mut seen: Vec<TitleRef> = Vec::new();
            for row in rows.iter().filter(|r| r.user_id == user_id) {
                let title = title_of(row);
                if seen.contains(&title) {
                    continue;
                }
                seen.push(title);
                let group: Vec<&WatchState> = rows
                    .iter()
                    .filter(|r| r.user_id == user_id && title_of(r) == title)
                    .collect();
                let latest = group.iter().map(|r| r.last_played_at).max().unwrap();
                let dismissed = group.iter().filter_map(|r| r.dismissed_at).max();
                let resumable = match title {
                    TitleRef::Show(_) => true,
                    TitleRef::Movie(_) => group.iter().any(|r| r.position_secs > 0.0),
                };
                if resumable && dismissed.is_none_or(|at| latest > at) {
                    titles.push((title, latest));
                }
            }
            let key = |(title, latest): &(TitleRef, DateTime<Utc>)| (*latest, title.id());
            titles.sort_by_key(|title| std::cmp::Reverse(key(title)));
            Ok(titles
                .into_iter()
                .filter(|candidate| {
                    after.is_none_or(|after| {
                        key(candidate) < (after.last_played_at, after.title.id())
                    })
                })
                .take(limit as usize)
                .map(|(title, last_played_at)| ContinueCandidate {
                    title,
                    last_played_at,
                })
                .collect())
        }

        async fn find_history_page(
            &self,
            user_id: Uuid,
            after: Option<HistoryPosition>,
            limit: u64,
        ) -> Result<Vec<WatchState>, DbErr> {
            let mut rows: Vec<WatchState> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.user_id == user_id)
                .filter(|r| {
                    after.is_none_or(|after| {
                        (r.last_played_at, r.id) < (after.last_played_at, after.id)
                    })
                })
                .cloned()
                .collect();
            rows.sort_by(|a, b| (b.last_played_at, b.id).cmp(&(a.last_played_at, a.id)));
            rows.truncate(limit as usize);
            Ok(rows)
        }

        async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DbErr> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.user_id == user_id)
                .count() as u64)
        }

        async fn last_played_at(
            &self,
            file_ids: Vec<Uuid>,
        ) -> Result<HashMap<Uuid, DateTime<Utc>>, DbErr> {
            let mut last: HashMap<Uuid, DateTime<Utc>> = HashMap::new();
            for row in self.rows.lock().unwrap().iter() {
                if let Some(file_id) = row.last_file_id
                    && file_ids.contains(&file_id)
                {
                    let latest = last.entry(file_id).or_insert(row.last_played_at);
                    *latest = (*latest).max(row.last_played_at);
                }
            }
            Ok(last)
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory::InMemoryWatchStateRepository;

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_fixture {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::in_memory::InMemoryWatchStateRepository;
    use crate::models::watch_state::WatchTarget;
    use crate::repositories::WatchStateRepository;
    use crate::repositories::contract::fixture::WatchStateFixture;
    use crate::services::TestClock;

    /// The hermetic instantiation of the shared contract. The in-memory
    /// store enforces no referential integrity, so fresh v4 UUIDs are a valid
    /// user, movie, episode, show and file; the Postgres fixture in
    /// `beam-index` inserts real rows for the same calls.
    pub struct InMemoryFixture {
        repo: InMemoryWatchStateRepository,
        clock: Arc<TestClock>,
    }

    impl Default for InMemoryFixture {
        fn default() -> Self {
            Self::new()
        }
    }

    impl InMemoryFixture {
        pub fn new() -> Self {
            let clock = Arc::new(TestClock::new());
            Self {
                repo: InMemoryWatchStateRepository::new(clock.clone()),
                clock,
            }
        }
    }

    #[async_trait::async_trait]
    impl WatchStateFixture for InMemoryFixture {
        fn repo(&self) -> &dyn WatchStateRepository {
            &self.repo
        }

        fn clock(&self) -> &TestClock {
            &self.clock
        }

        async fn new_user(&self) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_movie(&self) -> WatchTarget {
            WatchTarget::Movie {
                movie_id: Uuid::new_v4(),
            }
        }

        async fn new_show(&self) -> Uuid {
            Uuid::new_v4()
        }

        async fn new_episode(&self, show_id: Uuid) -> WatchTarget {
            WatchTarget::Episode {
                episode_id: Uuid::new_v4(),
                show_id,
            }
        }

        async fn new_file(&self) -> Uuid {
            Uuid::new_v4()
        }
    }
}

#[cfg(test)]
mod contract_over_in_memory {
    async fn setup() -> super::in_memory_fixture::InMemoryFixture {
        super::in_memory_fixture::InMemoryFixture::new()
    }

    crate::watch_state_repository_contract!(setup);
}
