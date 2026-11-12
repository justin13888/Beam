//! Forward cursor pagination over the catalog.
//!
//! `GET /v1/media` is Relay-style for a reason the API docs state plainly:
//! indexing and enrichment continuously mutate the result set, so offset
//! pagination would skip or duplicate items. That has two consequences the
//! client must respect, and both are easy to get wrong.
//!
//! First, there is no meaningful "resume from the middle" on refresh -- a
//! cursor describes a position in a list that no longer exists. Refresh
//! restarts from the head.
//!
//! Second, the same item can legitimately arrive twice across pages while the
//! set shifts underneath, so identifiers are de-duplicated rather than trusted.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// How a page of results came back.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PageAdvance {
    /// Identifiers new to this pager, in payload order.
    pub new_ids: Vec<String>,
    /// Identifiers already seen, dropped as duplicates.
    pub duplicate_ids: Vec<String>,
    /// Whether another page is available.
    pub has_next_page: bool,
}

/// Forward-only cursor state for one query.
///
/// Exported as an object rather than re-implemented per platform: Android's
/// hand-rolled loops had already diverged from it, overwriting the cursor with
/// nothing when a page carried none and listing a row twice when it arrived on
/// two pages. One per list on screen; the interior lock is what lets the
/// foreign side hold it as a shared handle.
#[derive(Debug, uniffi::Object)]
pub struct CursorPager {
    state: Mutex<PagerState>,
}

/// What a [`CursorPager`] knows, behind its lock.
#[derive(Debug)]
struct PagerState {
    cursor: Option<String>,
    has_next_page: bool,
    started: bool,
    in_flight: bool,
    seen: HashSet<String>,
}

impl PagerState {
    fn new() -> Self {
        Self {
            cursor: None,
            has_next_page: true,
            started: false,
            in_flight: false,
            seen: HashSet::new(),
        }
    }
}

impl Default for CursorPager {
    fn default() -> Self {
        Self {
            state: Mutex::new(PagerState::new()),
        }
    }
}

impl CursorPager {
    /// The state, whether or not a panic elsewhere poisoned the lock: nothing
    /// that holds it can panic part-way through an update.
    fn state(&self) -> MutexGuard<'_, PagerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many distinct items have been seen.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state().seen.len()
    }
}

#[uniffi::export]
impl CursorPager {
    /// A pager positioned before the first page.
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The cursor to send as `after`, if any.
    #[must_use]
    pub fn cursor(&self) -> Option<String> {
        self.state().cursor.clone()
    }

    /// Whether another page is believed to exist.
    #[must_use]
    pub fn has_next_page(&self) -> bool {
        self.state().has_next_page
    }

    /// Whether nothing has been loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state().seen.is_empty()
    }

    /// Claim the right to request the next page.
    ///
    /// Returns `false` when the list is exhausted or a request is already out.
    /// Both matter on a fast scroll: Compose can fire an append several times
    /// before the first completes, and re-requesting the same cursor would
    /// duplicate a page and waste a rate-limited request.
    pub fn begin_request(&self) -> bool {
        let mut state = self.state();
        if state.in_flight || (state.started && !state.has_next_page) {
            return false;
        }
        state.in_flight = true;
        true
    }

    /// Record a page, advancing the cursor.
    pub fn complete_request(
        &self,
        ids: Vec<String>,
        end_cursor: Option<String>,
        has_next_page: bool,
    ) -> PageAdvance {
        let mut state = self.state();
        state.in_flight = false;
        state.started = true;
        state.has_next_page = has_next_page;
        // Only advance on success, and only when the server actually supplied
        // a cursor: advancing past a page we failed to record would silently
        // lose items.
        if end_cursor.is_some() {
            state.cursor = end_cursor;
        }

        let mut new_ids = Vec::new();
        let mut duplicate_ids = Vec::new();
        for id in ids {
            if state.seen.insert(id.clone()) {
                new_ids.push(id);
            } else {
                duplicate_ids.push(id);
            }
        }

        PageAdvance {
            new_ids,
            duplicate_ids,
            has_next_page,
        }
    }

    /// Record a failed request, leaving the cursor untouched so a retry asks
    /// for the same page rather than skipping it.
    pub fn fail_request(&self) {
        self.state().in_flight = false;
    }

    /// Restart from the head, forgetting everything seen.
    ///
    /// This is what refresh does. There is no partial refresh: the cursor
    /// describes a position in a list the server has since changed.
    pub fn reset(&self) {
        *self.state() = PagerState::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn the_first_request_is_allowed_and_sends_no_cursor() {
        let pager = CursorPager::new();
        assert!(pager.begin_request());
        assert_eq!(pager.cursor(), None);
    }

    #[test]
    fn a_completed_page_advances_the_cursor() {
        let pager = CursorPager::new();
        pager.begin_request();
        pager.complete_request(ids(&["a", "b"]), Some("cursor-1".to_owned()), true);
        assert_eq!(pager.cursor().as_deref(), Some("cursor-1"));
        assert_eq!(pager.len(), 2);
        assert!(!pager.is_empty());
    }

    #[test]
    fn a_concurrent_request_is_refused_while_one_is_in_flight() {
        // A fast scroll fires append repeatedly; re-requesting the same cursor
        // would duplicate a page and burn a rate-limited request.
        let pager = CursorPager::new();
        assert!(pager.begin_request());
        assert!(!pager.begin_request());

        pager.complete_request(ids(&["a"]), Some("c1".to_owned()), true);
        assert!(pager.begin_request());
    }

    #[test]
    fn requests_stop_once_the_server_says_there_is_no_next_page() {
        let pager = CursorPager::new();
        pager.begin_request();
        pager.complete_request(ids(&["a"]), Some("c1".to_owned()), false);
        assert!(!pager.has_next_page());
        assert!(!pager.begin_request(), "the list is exhausted");
    }

    #[test]
    fn a_failed_request_leaves_the_cursor_where_it_was() {
        // Advancing past a page that was never recorded would silently drop
        // every item on it.
        let pager = CursorPager::new();
        pager.begin_request();
        pager.complete_request(ids(&["a"]), Some("c1".to_owned()), true);

        pager.begin_request();
        pager.fail_request();
        assert_eq!(pager.cursor().as_deref(), Some("c1"));
        assert!(pager.begin_request(), "a retry is allowed");
    }

    #[test]
    fn an_item_repeated_across_pages_is_reported_as_a_duplicate() {
        // Legitimate while indexing mutates the set underneath the cursor.
        let pager = CursorPager::new();
        pager.begin_request();
        pager.complete_request(ids(&["a", "b"]), Some("c1".to_owned()), true);

        pager.begin_request();
        let advance = pager.complete_request(ids(&["b", "c"]), Some("c2".to_owned()), true);
        assert_eq!(advance.new_ids, ids(&["c"]));
        assert_eq!(advance.duplicate_ids, ids(&["b"]));
        assert_eq!(pager.len(), 3);
    }

    #[test]
    fn a_page_without_a_cursor_does_not_move_the_position() {
        let pager = CursorPager::new();
        pager.begin_request();
        pager.complete_request(ids(&["a"]), Some("c1".to_owned()), true);

        pager.begin_request();
        pager.complete_request(ids(&["b"]), None, true);
        assert_eq!(pager.cursor().as_deref(), Some("c1"));
    }

    #[test]
    fn resetting_restarts_from_the_head() {
        let pager = CursorPager::new();
        pager.begin_request();
        pager.complete_request(ids(&["a", "b"]), Some("c1".to_owned()), true);

        pager.reset();
        assert_eq!(pager.cursor(), None);
        assert!(pager.is_empty());
        assert!(pager.has_next_page());
        assert!(pager.begin_request());
    }

    #[test]
    fn an_empty_first_page_is_a_legitimate_empty_result() {
        let pager = CursorPager::new();
        pager.begin_request();
        let advance = pager.complete_request(Vec::new(), None, false);
        assert!(advance.new_ids.is_empty());
        assert!(pager.is_empty());
        assert!(!pager.begin_request());
    }
}
