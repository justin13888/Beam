//! What a viewer is watching: continue-watching rows and watch history.

use super::media::{EpisodeSummary, MediaKind, MediaSummary};
use super::{absolute, narrow_u32};
use crate::api::types as wire;
use crate::servers::ServerRecord;

/// A title to resume, or the next episode of a show to start.
///
/// The server sends what a row displays (issue #188), so `media` and `episode`
/// are built from the row itself rather than looked up: they carry the title,
/// artwork and episode numbering a tile draws, and nothing a row does not
/// send -- no genres, no synopsis.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ContinueWatchingEntry {
    /// The title this progress belongs to.
    pub media_id: String,
    /// The episode, when the title is a series.
    pub episode_id: Option<String>,
    /// The file to resume.
    pub file_id: String,
    /// Whether the title is a film or a series.
    pub kind: MediaKind,
    /// Where the user stopped.
    pub position_secs: f64,
    /// Total duration, where one is known.
    pub duration_secs: Option<f64>,
    /// How far through, from 0.0 to 1.0, or `None` without a duration.
    pub progress_fraction: Option<f64>,
    /// When progress was last reported, as a Unix timestamp.
    pub updated_at_unix: i64,
    /// The title's tile-level fields, as the row carries them. Always present
    /// for a row the server sent; optional so a record a platform builds by
    /// hand can leave it out.
    pub media: Option<MediaSummary>,
    /// The episode to play, as the row carries it, when this is a series.
    pub episode: Option<EpisodeSummary>,
    /// Whether the row offers the next episode of a show the viewer finished
    /// one of, rather than a title to resume; it starts at 0.
    #[uniffi(default)]
    pub is_next_up: bool,
}

/// One watch-history row.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct HistoryEntry {
    /// The title this row belongs to.
    pub media_id: String,
    /// The episode, when the title is a series.
    pub episode_id: Option<String>,
    /// The file that was played.
    pub file_id: String,
    /// Whether the title is a film or a series.
    pub kind: MediaKind,
    /// Where the user stopped.
    pub position_secs: f64,
    /// Total duration, where one is known.
    pub duration_secs: Option<f64>,
    /// How far through, from 0.0 to 1.0, or `None` without a duration.
    pub progress_fraction: Option<f64>,
    /// Whether the title was watched to the end.
    pub completed: bool,
    /// When it was last played, as a Unix timestamp.
    pub updated_at_unix: i64,
    /// The title's tile-level fields, as the row carries them (issue #188).
    pub media: Option<MediaSummary>,
    /// The episode, as the row carries it, when this is a series.
    pub episode: Option<EpisodeSummary>,
}

/// One page of watch history.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct HistoryPage {
    /// The rows on this page.
    pub items: Vec<HistoryEntry>,
    /// Total rows across every page, for a count in the UI.
    pub total: u64,
    /// Pass as `after` for the next page; `None` on an empty page.
    pub end_cursor: Option<String>,
    /// Whether a page follows this one.
    pub has_next_page: bool,
}

/// The fraction of a title watched, where a duration is known.
///
/// Clamped because a player that reports a position past the end -- which
/// happens when a file's probed duration is slightly short -- must not produce
/// a progress bar wider than its track.
#[must_use]
pub fn progress_fraction(position_secs: f64, duration_secs: Option<f64>) -> Option<f64> {
    match duration_secs {
        Some(duration) if duration > 0.0 => Some((position_secs / duration).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// Map the server's `media_type` onto the core's own enum.
fn kind_from_wire(media_type: wire::TitleKind) -> MediaKind {
    match media_type {
        wire::TitleKind::Movie => MediaKind::Movie,
        wire::TitleKind::Show => MediaKind::Show,
    }
}

impl ContinueWatchingEntry {
    /// Normalise a generated continue-watching row, from its own display
    /// fields: no request per row.
    #[must_use]
    pub fn from_generated(item: wire::ContinueWatchingItem, record: &ServerRecord) -> Self {
        let wire::ContinueWatchingItem {
            duration_secs,
            episode_id,
            file_id,
            media_id,
            media_type,
            position_secs,
            last_played_at,
            reason,
            title,
            poster_url,
            backdrop_url,
            // A row shows its episode as "E4", within the show; the season is
            // the show detail's to present.
            season_number: _,
            episode_number,
            episode_title,
            thumbnail_url,
        } = item;
        let row = RowDisplay {
            kind: kind_from_wire(media_type),
            media_id: media_id.to_string(),
            episode_id: episode_id.map(|id| id.to_string()),
            file_id: file_id.to_string(),
            title,
            poster_url,
            backdrop_url,
            episode_number,
            episode_title,
            thumbnail_url,
            duration_secs,
            position_secs,
            played: false,
        };
        let (media, episode) = row.summaries(record);

        Self {
            media_id: row.media_id,
            episode_id: row.episode_id,
            file_id: row.file_id,
            kind: row.kind,
            position_secs,
            duration_secs,
            progress_fraction: progress_fraction(position_secs, duration_secs),
            updated_at_unix: last_played_at.0.unix_timestamp(),
            media: Some(media),
            episode,
            is_next_up: matches!(reason, wire::ContinueWatchingReason::NextUp),
        }
    }
}

/// What a continue-watching or history row sends to display itself.
struct RowDisplay {
    kind: MediaKind,
    media_id: String,
    episode_id: Option<String>,
    file_id: String,
    title: String,
    poster_url: Option<String>,
    backdrop_url: Option<String>,
    episode_number: Option<i64>,
    episode_title: Option<String>,
    thumbnail_url: Option<String>,
    duration_secs: Option<f64>,
    position_secs: f64,
    played: bool,
}

impl RowDisplay {
    /// The title's and the episode's tile-level fields, from the row alone.
    ///
    /// Only what the row carries: a row sends no genres, synopsis or rating,
    /// and inventing them would make a partial summary look complete.
    fn summaries(&self, record: &ServerRecord) -> (MediaSummary, Option<EpisodeSummary>) {
        let media = MediaSummary {
            id: self.media_id.clone(),
            kind: self.kind,
            title: self.title.clone(),
            original_title: self.title.clone(),
            year: None,
            description: None,
            poster_url: absolute(record, self.poster_url.clone()),
            backdrop_url: absolute(record, self.backdrop_url.clone()),
            genres: Vec::new(),
            runtime_minutes: None,
            tmdb_rating: None,
            file_id: (self.kind == MediaKind::Movie).then(|| self.file_id.clone()),
            season_count: 0,
            episode_count: 0,
            played: self.kind == MediaKind::Movie && self.played,
            resume_position_secs: if self.kind == MediaKind::Movie {
                self.position_secs
            } else {
                0.0
            },
            watched_episode_count: 0,
        };
        let episode = self.episode_id.as_ref().map(|id| EpisodeSummary {
            id: id.clone(),
            episode_number: narrow_u32(self.episode_number).unwrap_or(0),
            title: self.episode_title.clone().unwrap_or_default(),
            description: None,
            thumbnail_url: absolute(record, self.thumbnail_url.clone()),
            air_date: None,
            duration_secs: self.duration_secs,
            file_id: Some(self.file_id.clone()),
            played: self.played,
            resume_position_secs: self.position_secs,
        });
        (media, episode)
    }
}

impl HistoryEntry {
    /// Normalise a generated history row, from its own display fields.
    #[must_use]
    pub fn from_generated(item: wire::HistoryItem, record: &ServerRecord) -> Self {
        let wire::HistoryItem {
            played,
            duration_secs,
            episode_id,
            file_id,
            media_id,
            media_type,
            position_secs,
            last_played_at,
            play_count: _,
            title,
            poster_url,
            backdrop_url,
            season_number: _,
            episode_number,
            episode_title,
            thumbnail_url,
        } = item;
        let row = RowDisplay {
            kind: kind_from_wire(media_type),
            media_id: media_id.to_string(),
            episode_id: episode_id.map(|id| id.to_string()),
            file_id: file_id.to_string(),
            title,
            poster_url,
            backdrop_url,
            episode_number,
            episode_title,
            thumbnail_url,
            duration_secs,
            position_secs,
            played,
        };
        let (media, episode) = row.summaries(record);
        Self {
            media_id: row.media_id,
            episode_id: row.episode_id,
            file_id: row.file_id,
            kind: row.kind,
            position_secs,
            duration_secs,
            progress_fraction: progress_fraction(position_secs, duration_secs),
            completed: played,
            updated_at_unix: last_played_at.0.unix_timestamp(),
            media: Some(media),
            episode,
        }
    }
}

impl HistoryPage {
    /// Normalise a generated page of watch history, keeping its cursor and
    /// total.
    #[must_use]
    pub fn from_generated(connection: wire::HistoryConnection, record: &ServerRecord) -> Self {
        let wire::HistoryConnection {
            items,
            page_info,
            total,
        } = connection;
        Self {
            items: items
                .into_iter()
                .map(|item| HistoryEntry::from_generated(item, record))
                .collect(),
            total: u64::try_from(total).unwrap_or(0),
            end_cursor: page_info.end_cursor,
            has_next_page: page_info.has_next_page,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_support::record;

    #[test]
    fn progress_is_a_fraction_only_when_a_duration_is_known() {
        assert_eq!(progress_fraction(30.0, Some(120.0)), Some(0.25));
        assert_eq!(progress_fraction(30.0, None), None);
        assert_eq!(
            progress_fraction(30.0, Some(0.0)),
            None,
            "a zero duration must not divide"
        );
    }

    #[test]
    fn a_position_past_the_end_clamps_rather_than_overflowing_the_bar() {
        // Probed durations are sometimes slightly short of the real file, so
        // the player genuinely does report positions past the end.
        assert_eq!(progress_fraction(130.0, Some(120.0)), Some(1.0));
        assert_eq!(progress_fraction(-5.0, Some(120.0)), Some(0.0));
    }

    /// A continue-watching row as the server sends one: a movie to
    /// resume, or an episode of a show.
    fn continue_json(media_type: &str, episode_id: Option<&str>) -> String {
        let episode = episode_id.map_or("null".to_string(), |id| format!(r#""{id}""#));
        format!(
            r#"{{"reason":"resume","file_id":"00000000-0000-0000-0000-0000000000f1",
                 "media_id":"00000000-0000-0000-0000-0000000000a1",
                 "media_type":"{media_type}","episode_id":{episode},
                 "position_secs":30.0,"duration_secs":120.0,
                 "last_played_at":"2026-01-01T00:00:00Z","title":"Heat"}}"#
        )
    }

    #[test]
    fn continue_watching_classifies_by_media_type() {
        let make = |media_type: &str| {
            let item: wire::ContinueWatchingItem =
                serde_json::from_str(&continue_json(media_type, None)).expect("a valid item");
            ContinueWatchingEntry::from_generated(item, &record())
        };
        assert_eq!(make("show").kind, MediaKind::Show);
        assert_eq!(make("movie").kind, MediaKind::Movie);
    }

    #[test]
    fn a_continue_watching_row_carries_its_progress() {
        let json = continue_json("show", Some("00000000-0000-0000-0000-0000000000e1"));
        let item: wire::ContinueWatchingItem = serde_json::from_str(&json).expect("a valid item");
        let entry = ContinueWatchingEntry::from_generated(item, &record());
        assert_eq!(entry.progress_fraction, Some(0.25));
        assert_eq!(entry.updated_at_unix, 1_767_225_600);
        assert_eq!(entry.media_id, "00000000-0000-0000-0000-0000000000a1");
        assert_eq!(
            entry.episode_id.as_deref(),
            Some("00000000-0000-0000-0000-0000000000e1")
        );
        assert!(!entry.is_next_up, "a resume row");
    }

    /// The row draws itself from what the server sent (issue #188): the
    /// title and artwork on `media`, the episode's numbering, title and still
    /// on `episode`, every URL resolved against the server -- with no lookup.
    #[test]
    fn a_next_up_row_is_drawn_from_its_own_display_fields() {
        let json = r#"{"reason":"next_up","file_id":"00000000-0000-0000-0000-0000000000f2",
                       "media_id":"00000000-0000-0000-0000-0000000000a1","media_type":"show",
                       "episode_id":"00000000-0000-0000-0000-0000000000e2",
                       "position_secs":0.0,"duration_secs":3000.0,
                       "last_played_at":"2026-01-01T00:00:00Z","title":"Dark",
                       "poster_url":"/v1/artwork/show/a1/poster",
                       "backdrop_url":"/v1/artwork/show/a1/backdrop",
                       "season_number":2,"episode_number":5,"episode_title":"Truths",
                       "thumbnail_url":"/v1/artwork/episode/e2/still"}"#;
        let item: wire::ContinueWatchingItem = serde_json::from_str(json).expect("a valid item");
        let entry = ContinueWatchingEntry::from_generated(item, &record());

        assert!(entry.is_next_up);
        let media = entry.media.expect("the row's title");
        assert_eq!(media.title, "Dark");
        assert_eq!(media.kind, MediaKind::Show);
        assert_eq!(
            media.poster_url.as_deref(),
            Some("https://beam.local:8000/v1/artwork/show/a1/poster")
        );
        assert_eq!(
            media.backdrop_url.as_deref(),
            Some("https://beam.local:8000/v1/artwork/show/a1/backdrop")
        );
        assert_eq!(media.file_id, None, "a show plays through its episodes");
        let episode = entry.episode.expect("the row's episode");
        assert_eq!(episode.id, "00000000-0000-0000-0000-0000000000e2");
        assert_eq!(episode.episode_number, 5);
        assert_eq!(episode.title, "Truths");
        assert_eq!(
            episode.thumbnail_url.as_deref(),
            Some("https://beam.local:8000/v1/artwork/episode/e2/still")
        );
        assert_eq!(
            episode.file_id.as_deref(),
            Some("00000000-0000-0000-0000-0000000000f2")
        );
    }

    #[test]
    fn a_movie_row_has_no_episode_and_names_its_file() {
        let json = continue_json("movie", None);
        let item: wire::ContinueWatchingItem = serde_json::from_str(&json).expect("a valid item");
        let entry = ContinueWatchingEntry::from_generated(item, &record());

        assert!(entry.episode.is_none());
        let media = entry.media.expect("the row's title");
        assert_eq!(media.title, "Heat");
        assert_eq!(
            media.file_id.as_deref(),
            Some("00000000-0000-0000-0000-0000000000f1")
        );
        assert!((media.resume_position_secs - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_history_row_carries_whether_it_was_played() {
        let json = r#"{"file_id":"00000000-0000-0000-0000-0000000000f1",
                       "media_id":"00000000-0000-0000-0000-0000000000a1","media_type":"show",
                       "episode_id":"00000000-0000-0000-0000-0000000000e1","played":true,
                       "play_count":2,"position_secs":0.0,"duration_secs":120.0,
                       "last_played_at":"2026-01-01T00:00:00Z","title":"Dark",
                       "episode_number":1,"episode_title":"Secrets"}"#;
        let item: wire::HistoryItem = serde_json::from_str(json).expect("a valid item");
        let entry = HistoryEntry::from_generated(item, &record());
        assert!(entry.completed);
        assert_eq!(
            entry.episode_id.as_deref(),
            Some("00000000-0000-0000-0000-0000000000e1")
        );
        assert_eq!(entry.progress_fraction, Some(0.0));
        let media = entry.media.expect("the row's title");
        assert_eq!(media.title, "Dark");
        assert!(
            !media.played,
            "a played episode marks the episode, not the whole show"
        );
        let episode = entry.episode.expect("the row's episode");
        assert_eq!(episode.title, "Secrets");
        assert!(episode.played);
    }

    fn history_connection(total: i64, end_cursor: &str, has_next_page: bool) -> String {
        format!(
            r#"{{"items":[{{"file_id":"00000000-0000-0000-0000-0000000000f1",
                  "media_id":"00000000-0000-0000-0000-0000000000a1","media_type":"movie",
                  "played":false,"play_count":1,"position_secs":30.0,"duration_secs":120.0,
                  "last_played_at":"2026-01-01T00:00:00Z","title":"Arrival"}}],
                "page_info":{{"has_next_page":{has_next_page},"has_previous_page":false,
                              "end_cursor":"{end_cursor}"}},
                "total":{total}}}"#
        )
    }

    /// A page of history keeps where it ends and whether another follows, so
    /// the next request asks for the page after this one, and carries its
    /// rows and the server's total.
    #[test]
    fn a_history_page_carries_its_rows_cursor_and_total() {
        let connection: wire::HistoryConnection =
            serde_json::from_str(&history_connection(7, "c2", true)).expect("a valid page");
        let page = HistoryPage::from_generated(connection, &record());
        assert_eq!(page.total, 7);
        assert_eq!(page.end_cursor.as_deref(), Some("c2"));
        assert!(page.has_next_page);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].progress_fraction, Some(0.25));

        let last: wire::HistoryConnection =
            serde_json::from_str(&history_connection(7, "c3", false)).expect("a valid page");
        assert!(!HistoryPage::from_generated(last, &record()).has_next_page);
    }

    /// The contract's `uint64` reaches the generated client as `i64`; a
    /// negative total is nonsense and reads as none rather than wrapping.
    #[test]
    fn a_negative_history_total_reads_as_zero() {
        let connection: wire::HistoryConnection =
            serde_json::from_str(&history_connection(-1, "c2", true)).expect("a valid page");
        assert_eq!(HistoryPage::from_generated(connection, &record()).total, 0);
    }

    proptest::proptest! {
        /// A progress bar never runs past either end of its track, whatever
        /// position the server holds -- negative after a seek glitch, or past
        /// a probed duration that came out short. Finite only: both numbers
        /// arrive as JSON, which has no infinity and no NaN to send.
        #[test]
        fn a_progress_fraction_stays_within_the_bar(
            position in proptest::num::f64::NORMAL
                | proptest::num::f64::SUBNORMAL
                | proptest::num::f64::ZERO,
            duration in proptest::option::of(
                proptest::num::f64::NORMAL
                    | proptest::num::f64::SUBNORMAL
                    | proptest::num::f64::ZERO
            ),
        ) {
            if let Some(fraction) = progress_fraction(position, duration) {
                proptest::prop_assert!((0.0..=1.0).contains(&fraction), "{fraction}");
            } else {
                proptest::prop_assert!(!duration.is_some_and(|known| known > 0.0));
            }
        }
    }
}
