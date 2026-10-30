//! Titles as the catalog lists them: browse pages, a title's detail, and the
//! query that browses them.

use super::{absolute, narrow_u32, rating_percent};
use crate::api::types as wire;
use crate::error::BeamError;
use crate::servers::ServerRecord;

/// Whether a catalog entry is a film or a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaKind {
    /// A single film.
    Movie,
    /// A series with seasons and episodes.
    Show,
}

/// A catalog entry as a grid tile or list row needs it.
///
/// Deliberately flat: a tile renders from this alone, with no follow-up
/// request and no nested optionals to unwrap on the UI thread.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MediaSummary {
    /// Stable identifier, used for detail and source lookups.
    pub id: String,
    /// Whether this is a film or a series.
    pub kind: MediaKind,
    /// The title to display, preferring the localized form.
    pub title: String,
    /// The original-language title, for a subtitle when it differs.
    pub original_title: String,
    /// Release year, where metadata determined one.
    pub year: Option<u32>,
    /// Synopsis.
    pub description: Option<String>,
    /// Absolute poster URL.
    pub poster_url: Option<String>,
    /// Absolute backdrop URL.
    pub backdrop_url: Option<String>,
    /// Genre names.
    pub genres: Vec<String>,
    /// Runtime in whole minutes, for films.
    pub runtime_minutes: Option<u32>,
    /// TMDB rating as a percentage, where one is known.
    pub tmdb_rating: Option<u32>,
    /// The file to play, for a film that has exactly one.
    pub file_id: Option<String>,
    /// Number of seasons, for a series.
    pub season_count: u32,
    /// Number of episodes across every season, for a series.
    pub episode_count: u32,
    /// Whether the viewer has watched it (issue #188): a film watched to the
    /// end or marked watched; a series every episode of which is. A series in
    /// a browse result reads as unwatched, since browse sends no viewer state
    /// for one; its detail fills it.
    #[uniffi(default)]
    pub played: bool,
    /// Where to resume a film, in seconds; 0 for one not started, one just
    /// finished, and a series.
    #[uniffi(default)]
    pub resume_position_secs: f64,
    /// How many of a series' episodes the viewer has watched; 0 for a film.
    #[uniffi(default)]
    pub watched_episode_count: u32,
}

/// One page of a cursor-paginated browse.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MediaPage {
    /// The entries on this page, in server order.
    pub items: Vec<MediaSummary>,
    /// Cursor to pass as `after` to fetch the next page.
    pub end_cursor: Option<String>,
    /// Whether another page exists.
    pub has_next_page: bool,
}

/// One episode of a series.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct EpisodeSummary {
    /// Stable identifier.
    pub id: String,
    /// Position within its season, as metadata numbered it.
    pub episode_number: u32,
    /// Episode title.
    pub title: String,
    /// Synopsis.
    pub description: Option<String>,
    /// Absolute thumbnail URL.
    pub thumbnail_url: Option<String>,
    /// Air date, as the server recorded it.
    pub air_date: Option<String>,
    /// Duration in seconds, where probing determined one.
    pub duration_secs: Option<f64>,
    /// The file to play, when one is indexed.
    ///
    /// `None` means the episode is known to metadata but has no file, which
    /// is what up-next skips over.
    pub file_id: Option<String>,
    /// Whether the viewer has watched it to the end or marked it watched.
    #[uniffi(default)]
    pub played: bool,
    /// Where to resume it, in seconds; 0 for one not started or just finished.
    #[uniffi(default)]
    pub resume_position_secs: f64,
}

/// One season of a series.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SeasonSummary {
    /// Season number, as metadata numbered it.
    pub season_number: u32,
    /// Absolute poster URL for this season.
    pub poster_url: Option<String>,
    /// Typical episode runtime in whole minutes.
    pub episode_runtime_minutes: Option<u32>,
    /// Genre names recorded against this season.
    pub genres: Vec<String>,
    /// Episodes, in the order the server returned them.
    pub episodes: Vec<EpisodeSummary>,
    /// Stable identifier, which marking the whole season watched names.
    #[uniffi(default)]
    pub id: String,
    /// Whether every episode is watched, and there is at least one.
    #[uniffi(default)]
    pub played: bool,
    /// How many of its episodes the viewer has watched.
    #[uniffi(default)]
    pub watched_episode_count: u32,
}

/// Everything a detail screen shows.
///
/// A sealed alternative rather than one struct with empty collections: a film
/// has no seasons and a series has no single `file_id`, and encoding that in
/// the type stops a screen rendering a "Play" button for a series that has no
/// file behind it.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum MediaDetail {
    /// A film, playable directly.
    Movie {
        /// Tile-level fields.
        summary: MediaSummary,
    },
    /// A series, played through one of its episodes.
    Show {
        /// Tile-level fields.
        summary: MediaSummary,
        /// Seasons in server order.
        seasons: Vec<SeasonSummary>,
    },
}

impl MediaDetail {
    /// The tile-level fields, whichever kind this is.
    #[must_use]
    pub fn summary(&self) -> &MediaSummary {
        match self {
            Self::Movie { summary } | Self::Show { summary, .. } => summary,
        }
    }
}

/// Field to order a browse by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaSortField {
    /// Alphabetical by title.
    Title,
    /// By release year.
    Year,
    /// By rating.
    Rating,
    /// By when the file was indexed.
    DateAdded,
    /// By runtime.
    Runtime,
}

/// Ascending or descending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SortOrder {
    /// Smallest or earliest first.
    Ascending,
    /// Largest or latest first.
    Descending,
}

/// Restrict a browse to one kind of entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaTypeFilter {
    /// Films only.
    Movie,
    /// Series only.
    Show,
}

/// Everything the explore screen can ask for.
///
/// One record rather than a dozen arguments, so adding a filter later is not a
/// breaking change to every caller on three platforms.
#[derive(Debug, Clone, PartialEq, Eq, Default, uniffi::Record)]
pub struct BrowseQuery {
    /// Page size.
    pub first: Option<u32>,
    /// Cursor from a previous page's `end_cursor`.
    pub after: Option<String>,
    /// Field to order by.
    pub sort_by: Option<MediaSortField>,
    /// Direction to order in.
    pub sort_order: Option<SortOrder>,
    /// Restrict to films or series.
    pub media_type: Option<MediaTypeFilter>,
    /// Restrict to one genre.
    pub genre: Option<String>,
    /// Restrict to one exact year.
    pub year: Option<u32>,
    /// Earliest year, inclusive.
    pub year_from: Option<u32>,
    /// Latest year, inclusive.
    pub year_to: Option<u32>,
    /// Free-text search.
    pub query: Option<String>,
    /// Minimum rating percentage.
    pub min_rating: Option<u32>,
}

impl MediaSummary {
    /// Normalise a generated catalog entry.
    fn from_generated(node: wire::MediaMetadata, record: &ServerRecord) -> Self {
        match node {
            wire::MediaMetadata::MediaMetadataVariant1(boxed) => {
                Self::from_movie(&boxed.movie, record)
            }
            wire::MediaMetadata::MediaMetadataVariant0(boxed) => {
                Self::from_show(&boxed.show, record)
            }
        }
    }

    fn from_movie(movie: &wire::MovieMetadata, record: &ServerRecord) -> Self {
        let title = &movie.title;
        Self {
            id: movie.id.to_string(),
            kind: MediaKind::Movie,
            title: title
                .localized
                .clone()
                .unwrap_or_else(|| title.original.clone()),
            original_title: title.original.clone(),
            year: narrow_u32(movie.year),
            description: movie.description.clone(),
            poster_url: absolute(record, movie.poster_url.clone()),
            backdrop_url: absolute(record, movie.backdrop_url.clone()),
            genres: movie.genres.clone(),
            runtime_minutes: narrow_u32(movie.runtime_mins),
            tmdb_rating: rating_percent(movie.ratings.as_ref().and_then(|ratings| ratings.tmdb)),
            file_id: movie.file_id.map(|id| id.to_string()),
            season_count: 0,
            episode_count: 0,
            played: movie.user_state.played,
            resume_position_secs: movie.user_state.position_secs,
            watched_episode_count: 0,
        }
    }

    fn from_show(show: &wire::ShowMetadata, record: &ServerRecord) -> Self {
        let title = &show.title;
        // A series' own poster wins. One without -- specials, season-less
        // shows, a series the provider has no art for -- borrows the first
        // season poster there is, so it still renders as more than a blank
        // placeholder. Genres, rating and the season and episode counts are
        // the show's own; browse sends no seasons, only the counts. Runtime
        // is recorded per season, so a tile takes the first there is.
        let poster = show.poster_url.clone().or_else(|| {
            show.seasons
                .iter()
                .find_map(|season| season.poster_url.clone())
        });
        let rating = show.ratings.as_ref().and_then(|ratings| ratings.tmdb);
        let runtime = show
            .seasons
            .iter()
            .find_map(|season| season.episode_runtime_mins);

        Self {
            id: show.id.to_string(),
            kind: MediaKind::Show,
            title: title
                .localized
                .clone()
                .unwrap_or_else(|| title.original.clone()),
            original_title: title.original.clone(),
            year: narrow_u32(show.year),
            description: show.description.clone(),
            poster_url: absolute(record, poster),
            backdrop_url: absolute(record, show.backdrop_url.clone()),
            genres: show.genres.clone(),
            runtime_minutes: narrow_u32(runtime),
            tmdb_rating: rating_percent(rating),
            file_id: None,
            season_count: narrow_u32(Some(show.season_count)).unwrap_or(0),
            episode_count: narrow_u32(Some(show.episode_count)).unwrap_or(0),
            played: show.user_state.as_ref().is_some_and(|state| state.played),
            resume_position_secs: 0.0,
            watched_episode_count: show
                .user_state
                .as_ref()
                .and_then(|state| narrow_u32(Some(state.watched_episode_count)))
                .unwrap_or(0),
        }
    }
}

impl EpisodeSummary {
    pub(crate) fn from_generated(episode: &wire::EpisodeMetadata, record: &ServerRecord) -> Self {
        Self {
            id: episode.id.to_string(),
            episode_number: narrow_u32(Some(episode.episode_number)).unwrap_or(0),
            title: episode.title.clone(),
            description: episode.description.clone(),
            thumbnail_url: absolute(record, episode.thumbnail_url.clone()),
            air_date: episode.air_date.as_ref().map(|date| date.0.to_string()),
            duration_secs: episode.duration_secs,
            file_id: episode.file_id.map(|id| id.to_string()),
            played: episode.user_state.played,
            resume_position_secs: episode.user_state.position_secs,
        }
    }
}

impl SeasonSummary {
    fn from_generated(season: &wire::SeasonMetadata, record: &ServerRecord) -> Self {
        Self {
            season_number: narrow_u32(Some(season.season_number)).unwrap_or(0),
            poster_url: absolute(record, season.poster_url.clone()),
            episode_runtime_minutes: narrow_u32(season.episode_runtime_mins),
            genres: season.genres.clone(),
            episodes: season
                .episodes
                .iter()
                .map(|episode| EpisodeSummary::from_generated(episode, record))
                .collect(),
            id: season.id.to_string(),
            played: season.user_state.as_ref().is_some_and(|state| state.played),
            watched_episode_count: season
                .user_state
                .as_ref()
                .and_then(|state| narrow_u32(Some(state.watched_episode_count)))
                .unwrap_or(0),
        }
    }
}

impl MediaDetail {
    /// Normalise a generated detail response.
    #[must_use]
    pub fn from_generated(node: wire::MediaMetadata, record: &ServerRecord) -> Self {
        match node {
            wire::MediaMetadata::MediaMetadataVariant1(boxed) => Self::Movie {
                summary: MediaSummary::from_movie(&boxed.movie, record),
            },
            wire::MediaMetadata::MediaMetadataVariant0(boxed) => Self::Show {
                summary: MediaSummary::from_show(&boxed.show, record),
                seasons: boxed
                    .show
                    .seasons
                    .iter()
                    .map(|season| SeasonSummary::from_generated(season, record))
                    .collect(),
            },
        }
    }
}

impl MediaPage {
    /// Normalise a generated connection.
    #[must_use]
    pub fn from_generated(connection: wire::MediaConnection, record: &ServerRecord) -> Self {
        let wire::MediaConnection { items, page_info } = connection;
        Self {
            items: items
                .into_iter()
                .map(|item| MediaSummary::from_generated(item, record))
                .collect(),
            end_cursor: page_info.end_cursor,
            has_next_page: page_info.has_next_page,
        }
    }
}

/// Translate a browse query into the generated parameter type.
///
/// # Errors
///
/// Returns [`BeamError::BadRequest`] when the query cannot be expressed, which
/// today means only a page size the server would reject.
pub fn browse_params(query: &BrowseQuery) -> Result<crate::api::BrowseMediaParams, BeamError> {
    let first = match query.first {
        Some(0) => {
            return Err(BeamError::BadRequest {
                detail: "a page size of zero would return nothing".to_owned(),
                // Refused before the wire, so there is no server type.
                code: crate::transport::ABOUT_BLANK.to_owned(),
            });
        }
        Some(value) => Some(i64::from(value)),
        None => None,
    };

    Ok(crate::api::BrowseMediaParams {
        first,
        after: query.after.clone(),
        last: None,
        before: None,
        // The query-parameter enums are generated separately from the schema
        // enums of the same shape, so these map onto `types::SortBy` and
        // friends rather than onto `MediaSortField`.
        sort_by: query.sort_by.map(|field| match field {
            MediaSortField::Title => wire::SortBy::Title,
            MediaSortField::Year => wire::SortBy::Year,
            MediaSortField::Rating => wire::SortBy::Rating,
            MediaSortField::DateAdded => wire::SortBy::DateAdded,
            MediaSortField::Runtime => wire::SortBy::Runtime,
        }),
        sort_order: query.sort_order.map(|order| match order {
            SortOrder::Ascending => wire::SortOrderFe86ad6c::Asc,
            SortOrder::Descending => wire::SortOrderFe86ad6c::Desc,
        }),
        media_type: query.media_type.map(|kind| match kind {
            MediaTypeFilter::Movie => wire::MediaType::Movie,
            MediaTypeFilter::Show => wire::MediaType::Show,
        }),
        genre: query.genre.clone(),
        year: query.year.map(i64::from),
        year_from: query.year_from.map(i64::from),
        year_to: query.year_to.map(i64::from),
        query: query.query.clone(),
        // The record keeps its percentage (issue #190 left the UniFFI surface
        // alone); the wire takes the rating on its own 0-10 scale.
        min_rating: query.min_rating.map(|percent| f64::from(percent) / 10.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_support::record;

    /// Parsing the wire shape rather than hand-building the generated structs
    /// means these tests also assert the contract the server actually
    /// publishes, not just this module's arithmetic.
    fn node(json: &str) -> wire::MediaMetadata {
        serde_json::from_str(json).expect("the fixture should match the generated type")
    }

    const MOVIE_ID: &str = "11111111-1111-4111-8111-111111111111";
    const MOVIE_FILE_ID: &str = "f1f1f1f1-0000-4000-8000-000000000001";
    const SHOW_ID: &str = "22222222-2222-4222-8222-222222222222";
    const EPISODE_ONE_FILE_ID: &str = "f1f1f1f1-0000-4000-8000-000000000001";
    const RETURN_EPISODE_ID: &str = "e0000003-0000-4000-8000-000000000003";

    const MOVIE: &str = r#"{"Movie":{
        "id":"11111111-1111-4111-8111-111111111111",
        "title":{"original":"Le Samourai","localized":"The Samurai"},
        "genres":["Crime","Drama"],
        "year":1967,
        "runtime_mins":105,
        "duration_secs":6300.0,
        "description":"A contract killer.",
        "poster_url":"/artwork/m1/poster.jpg",
        "backdrop_url":"/artwork/m1/backdrop.jpg",
        "file_id":"f1f1f1f1-0000-4000-8000-000000000001",
        "ratings":{"tmdb":8.1},
        "user_state":{"played":false,"position_secs":0.0,"play_count":0}
    }}"#;

    const SHOW: &str = r#"{"Show":{
        "id":"22222222-2222-4222-8222-222222222222",
        "title":{"original":"Le Bureau"},
        "description":"Undercover.",
        "year":2015,
        "genres":["Drama","Thriller"],
        "ratings":{"tmdb":8.8},
        "season_count":2,
        "episode_count":3,
        "seasons":[
            {"id":"5e500001-0000-4000-8000-000000000001","season_number":1,"dates":{},"genres":[],"episodes":[
                {"id":"e0000001-0000-4000-8000-000000000001","episode_number":1,"title":"Pilot","source_count":1,"user_state":{"played":false,"position_secs":0.0,"play_count":0},"file_id":"f1f1f1f1-0000-4000-8000-000000000001"},
                {"id":"e0000002-0000-4000-8000-000000000002","episode_number":2,"title":"Second","source_count":0,"user_state":{"played":false,"position_secs":0.0,"play_count":0}}
            ]},
            {"id":"5e500002-0000-4000-8000-000000000002","season_number":2,"dates":{},"genres":[],"episode_runtime_mins":52,
             "poster_url":"/artwork/s1/2.jpg","episodes":[
                {"id":"e0000003-0000-4000-8000-000000000003","episode_number":1,"title":"Return","source_count":1,"user_state":{"played":false,"position_secs":0.0,"play_count":0},"file_id":"f3f3f3f3-0000-4000-8000-000000000003",
                 "thumbnail_url":"/artwork/e3.jpg","duration_secs":3120.0,"air_date":"2016-01-01"}
            ]}
        ]
    }}"#;

    /// A show as browse sends it: its counts and its own genres and rating,
    /// and no seasons.
    const BROWSED_SHOW: &str = r#"{"Show":{
        "id":"22222222-2222-4222-8222-222222222222",
        "title":{"original":"Le Bureau"},
        "genres":["Drama"],
        "ratings":{"tmdb":9.1},
        "season_count":5,
        "episode_count":50,
        "seasons":[]
    }}"#;

    #[test]
    fn movie_prefers_the_localized_title_and_keeps_the_original() {
        let summary = MediaSummary::from_generated(node(MOVIE), &record());
        assert_eq!(summary.title, "The Samurai");
        assert_eq!(summary.original_title, "Le Samourai");
        assert_eq!(summary.kind, MediaKind::Movie);
        assert_eq!(summary.year, Some(1967));
        assert_eq!(summary.runtime_minutes, Some(105));
        assert_eq!(summary.tmdb_rating, Some(81));
        assert_eq!(summary.file_id.as_deref(), Some(MOVIE_FILE_ID));
        assert_eq!(summary.id, MOVIE_ID);
    }

    #[test]
    fn a_title_without_a_localized_form_falls_back_to_the_original() {
        let summary = MediaSummary::from_generated(node(SHOW), &record());
        assert_eq!(summary.title, "Le Bureau");
        assert_eq!(summary.original_title, "Le Bureau");
    }

    #[test]
    fn relative_artwork_is_resolved_against_the_serving_server() {
        let summary = MediaSummary::from_generated(node(MOVIE), &record());
        assert_eq!(
            summary.poster_url.as_deref(),
            Some("https://beam.local:8000/artwork/m1/poster.jpg")
        );
        assert_eq!(
            summary.backdrop_url.as_deref(),
            Some("https://beam.local:8000/artwork/m1/backdrop.jpg")
        );
    }

    /// A series carrying its own artwork, as the server publishes it whenever
    /// enrichment found any, alongside a season that also has a poster.
    const SHOW_WITH_ARTWORK: &str = r#"{"Show":{
        "id":"22222222-2222-4222-8222-222222222222",
        "title":{"original":"Le Bureau"},
        "genres":[],
        "season_count":1,
        "episode_count":0,
        "poster_url":"/v1/artwork/show/s1/poster",
        "backdrop_url":"/v1/artwork/show/s1/backdrop",
        "seasons":[
            {"id":"5e500002-0000-4000-8000-000000000002","season_number":2,"dates":{},"genres":[],
             "poster_url":"/artwork/s1/2.jpg","episodes":[]}
        ]
    }}"#;

    #[test]
    fn a_show_prefers_its_own_artwork_over_a_seasons() {
        let summary = MediaSummary::from_generated(node(SHOW_WITH_ARTWORK), &record());
        assert_eq!(
            summary.poster_url.as_deref(),
            Some("https://beam.local:8000/v1/artwork/show/s1/poster")
        );
        assert_eq!(
            summary.backdrop_url.as_deref(),
            Some("https://beam.local:8000/v1/artwork/show/s1/backdrop")
        );
    }

    #[test]
    fn a_show_borrows_artwork_from_the_first_season_when_the_show_has_none() {
        // A series with no poster of its own would otherwise render as an
        // untitled grey placeholder.
        let summary = MediaSummary::from_generated(node(SHOW), &record());
        assert_eq!(summary.backdrop_url, None, "no season carries a backdrop");
        assert_eq!(summary.kind, MediaKind::Show);
        assert_eq!(
            summary.poster_url.as_deref(),
            Some("https://beam.local:8000/artwork/s1/2.jpg")
        );
        assert_eq!(summary.runtime_minutes, Some(52));
    }

    #[test]
    fn a_show_takes_its_genres_rating_and_counts_from_itself() {
        let summary = MediaSummary::from_generated(node(SHOW), &record());
        assert_eq!(
            summary.genres,
            vec!["Drama".to_owned(), "Thriller".to_owned()]
        );
        assert_eq!(summary.tmdb_rating, Some(88));
        assert_eq!(summary.season_count, 2);
        assert_eq!(summary.episode_count, 3);
        assert_eq!(summary.file_id, None, "a series is not itself playable");
    }

    /// Browse sends a show without its seasons (issue #187): the tile's
    /// counts, genres and rating come from the show, not from seasons it
    /// does not have.
    #[test]
    fn a_browsed_show_without_seasons_still_carries_its_counts() {
        let summary = MediaSummary::from_generated(node(BROWSED_SHOW), &record());
        assert_eq!(summary.season_count, 5);
        assert_eq!(summary.episode_count, 50);
        assert_eq!(summary.genres, vec!["Drama".to_owned()]);
        assert_eq!(summary.tmdb_rating, Some(91));
    }

    #[test]
    fn show_detail_carries_seasons_with_their_episodes() {
        let detail = MediaDetail::from_generated(node(SHOW), &record());
        let MediaDetail::Show { seasons, summary } = &detail else {
            panic!("a Show node must lower to a Show detail");
        };
        assert_eq!(summary.id, SHOW_ID);
        assert_eq!(seasons.len(), 2);
        assert_eq!(seasons[0].episodes.len(), 2);
        assert_eq!(seasons[1].episodes[0].id, RETURN_EPISODE_ID);
        assert_eq!(
            seasons[1].episodes[0].thumbnail_url.as_deref(),
            Some("https://beam.local:8000/artwork/e3.jpg")
        );
        assert_eq!(seasons[1].episodes[0].duration_secs, Some(3120.0));
        assert_eq!(
            seasons[1].episodes[0].air_date.as_deref(),
            Some("2016-01-01")
        );
    }

    #[test]
    fn an_episode_with_no_indexed_file_keeps_a_none_file_id() {
        // Up-next relies on this to skip past episodes that exist in metadata
        // but have nothing to play.
        let detail = MediaDetail::from_generated(node(SHOW), &record());
        let MediaDetail::Show { seasons, .. } = &detail else {
            panic!("a Show node must lower to a Show detail");
        };
        assert_eq!(
            seasons[0].episodes[0].file_id.as_deref(),
            Some(EPISODE_ONE_FILE_ID)
        );
        assert_eq!(seasons[0].episodes[1].file_id, None);
    }

    /// The viewer's state rides the detail (issue #188): per episode, per
    /// season, and for the whole show -- and a season carries the id that
    /// marking it watched names.
    #[test]
    fn a_show_detail_carries_the_viewers_watched_state_at_every_level() {
        const WATCHED: &str = r#"{"Show":{
            "id":"22222222-2222-4222-8222-222222222222",
            "title":{"original":"Le Bureau"},
            "genres":[],"season_count":1,"episode_count":2,
            "user_state":{"played":false,"watched_episode_count":1,"episode_count":2},
            "seasons":[
                {"id":"5e500001-0000-4000-8000-000000000001","season_number":1,"dates":{},"genres":[],
                 "user_state":{"played":false,"watched_episode_count":1,"episode_count":2},
                 "episodes":[
                    {"id":"e0000001-0000-4000-8000-000000000001","episode_number":1,"title":"Pilot","source_count":1,
                     "user_state":{"played":true,"position_secs":0.0,"play_count":1}},
                    {"id":"e0000002-0000-4000-8000-000000000002","episode_number":2,"title":"Second","source_count":1,
                     "user_state":{"played":false,"position_secs":600.0,"play_count":0}}
                ]}
            ]
        }}"#;
        let detail = MediaDetail::from_generated(node(WATCHED), &record());
        let MediaDetail::Show { seasons, summary } = &detail else {
            panic!("a Show node must lower to a Show detail");
        };
        assert!(!summary.played);
        assert_eq!(summary.watched_episode_count, 1);
        let season = &seasons[0];
        assert_eq!(season.id, "5e500001-0000-4000-8000-000000000001");
        assert!(!season.played);
        assert_eq!(season.watched_episode_count, 1);
        assert!(season.episodes[0].played);
        assert!(!season.episodes[1].played);
        assert!((season.episodes[1].resume_position_secs - 600.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_watched_movie_reads_as_played_and_a_browsed_show_as_unwatched() {
        let watched = MOVIE.replace(
            r#""user_state":{"played":false,"position_secs":0.0,"play_count":0}"#,
            r#""user_state":{"played":true,"position_secs":1200.0,"play_count":2}"#,
        );
        let movie = MediaDetail::from_generated(node(&watched), &record());
        assert!(movie.summary().played);
        assert!((movie.summary().resume_position_secs - 1200.0).abs() < f64::EPSILON);

        // Browse sends no viewer state for a show; it reads as unwatched
        // rather than failing to decode.
        let browsed = MediaDetail::from_generated(node(BROWSED_SHOW), &record());
        assert!(!browsed.summary().played);
        assert_eq!(browsed.summary().watched_episode_count, 0);
    }

    #[test]
    fn movie_detail_lowers_to_the_movie_branch() {
        let detail = MediaDetail::from_generated(node(MOVIE), &record());
        assert!(matches!(detail, MediaDetail::Movie { .. }));
        assert_eq!(detail.summary().id, MOVIE_ID);
    }

    #[test]
    fn a_connection_becomes_a_page_that_carries_its_cursor() {
        let json = format!(
            r#"{{"items":[{MOVIE}],
                 "page_info":{{"has_next_page":true,"has_previous_page":false,"end_cursor":"c1"}}}}"#
        );
        let connection: wire::MediaConnection =
            serde_json::from_str(&json).expect("a valid connection");
        let page = MediaPage::from_generated(connection, &record());
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.end_cursor.as_deref(), Some("c1"));
        assert!(page.has_next_page);
    }

    /// The UniFFI record keeps its percentage; the wire's 0-10 rating is
    /// scaled and rounded to it, and a value off the scale is dropped.
    #[test]
    fn a_tmdb_rating_becomes_the_records_rounded_percentage() {
        for (wire_rating, percent) in [
            (Some(8.4), Some(84)),
            (Some(7.25), Some(73)),
            (Some(0.0), Some(0)),
            (Some(10.0), Some(100)),
            (Some(10.5), None),
            (Some(-0.1), None),
            (Some(f64::NAN), None),
            (None, None),
        ] {
            assert_eq!(rating_percent(wire_rating), percent, "{wire_rating:?}");
        }
    }

    #[test]
    fn browse_params_refuses_a_page_size_of_zero() {
        let query = BrowseQuery {
            first: Some(0),
            ..BrowseQuery::default()
        };
        assert!(matches!(
            browse_params(&query),
            Err(BeamError::BadRequest { .. })
        ));
    }

    #[test]
    fn browse_params_maps_every_filter_onto_the_query_enums() {
        let query = BrowseQuery {
            first: Some(24),
            after: Some("cursor".to_owned()),
            sort_by: Some(MediaSortField::DateAdded),
            sort_order: Some(SortOrder::Descending),
            media_type: Some(MediaTypeFilter::Show),
            genre: Some("Drama".to_owned()),
            year: Some(1999),
            year_from: Some(1990),
            year_to: Some(2000),
            query: Some("bureau".to_owned()),
            min_rating: Some(70),
        };
        let params = browse_params(&query).expect("a valid query");
        assert_eq!(params.first, Some(24));
        assert_eq!(params.after.as_deref(), Some("cursor"));
        assert!(matches!(params.sort_by, Some(wire::SortBy::DateAdded)));
        assert!(matches!(
            params.sort_order,
            Some(wire::SortOrderFe86ad6c::Desc)
        ));
        assert!(matches!(params.media_type, Some(wire::MediaType::Show)));
        assert_eq!(params.genre.as_deref(), Some("Drama"));
        assert_eq!(params.year, Some(1999));
        assert_eq!(params.year_from, Some(1990));
        assert_eq!(params.year_to, Some(2000));
        assert_eq!(
            params.min_rating,
            Some(7.0),
            "the percentage on the wire's 0-10 scale"
        );
        assert_eq!(params.last, None, "forward paging only");
        assert_eq!(params.before, None);
    }

    #[test]
    fn an_empty_query_sends_no_filters_at_all() {
        let params = browse_params(&BrowseQuery::default()).expect("a valid query");
        assert_eq!(params.first, None);
        assert!(params.sort_by.is_none());
        assert!(params.genre.is_none());
    }

    #[test]
    fn a_negative_metadata_integer_is_treated_as_absent() {
        // JSON Schema has no unsigned type, so these arrive as i64. A negative
        // year is impossible from the server; the point is that it must not
        // wrap into a plausible-looking enormous number.
        assert_eq!(narrow_u32(Some(-1)), None);
        assert_eq!(narrow_u32(Some(1967)), Some(1967));
        assert_eq!(narrow_u32(None), None);
    }

    #[test]
    fn unresolvable_artwork_is_dropped_rather_than_failing_the_page() {
        let record = record();
        assert_eq!(absolute(&record, None), None);
        assert_eq!(
            absolute(&record, Some("/ok.jpg".to_owned())).as_deref(),
            Some("https://beam.local:8000/ok.jpg")
        );
    }
}
