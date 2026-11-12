//! A title's stored model as the wire carries it: the pure mapping from the
//! `beam_domain` movie, show and season to the metadata DTOs. Nothing here
//! reads a store; [`super::DbMetadataService`] gathers what each takes.

use uuid::Uuid;

use crate::models::{
    ArtworkKind, ArtworkVariant, CastCredit, CollectionRef, CrewCredit, EpisodeMetadata,
    ExternalIdentifiers, MediaMetadata, MovieMetadata, Ratings, SeasonMetadata, ShowDates,
    ShowMetadata, ShowRef, Title, UserTitleState, artwork_path,
};
use crate::services::sources::PrimarySource;
use beam_domain::models::catalog::ShowChildCounts;

/// A provider rating as the wire carries it: on its own 0-10 scale.
fn ratings(rating_tmdb: Option<f32>) -> Option<Ratings> {
    rating_tmdb.map(|r| Ratings {
        tmdb: Some(widen_rating(r)),
    })
}

/// The stored single-precision rating as the decimal it was stored from.
///
/// `f64::from(7.2_f32)` is `7.199999809265137`: exact, and not the number
/// anyone wrote down. The shortest decimal that reads back as the same `f32`
/// is -- that is what `f32`'s `Display` prints -- so the wire carries `7.2`.
fn widen_rating(rating: f32) -> f64 {
    rating
        .to_string()
        .parse()
        .unwrap_or_else(|_| f64::from(rating))
}

fn identifiers(
    imdb_id: Option<String>,
    tmdb_id: Option<u32>,
    tvdb_id: Option<u32>,
) -> Option<ExternalIdentifiers> {
    (imdb_id.is_some() || tmdb_id.is_some() || tvdb_id.is_some()).then_some(ExternalIdentifiers {
        imdb_id,
        tmdb_id,
        tvdb_id,
    })
}

/// The parts of a movie's metadata that come from files, which browse does
/// not read.
#[derive(Debug, Default)]
pub(super) struct MovieFiles {
    duration: Option<f64>,
    file_id: Option<Uuid>,
    source_count: Option<u32>,
}

impl From<PrimarySource> for MovieFiles {
    fn from(primary: PrimarySource) -> Self {
        let PrimarySource {
            file_id,
            duration_secs,
            last_episode_number: _,
            source_count,
        } = primary;
        Self {
            duration: duration_secs,
            file_id,
            source_count: Some(source_count),
        }
    }
}

/// A title's cast and crew, as its detail lists them; `None` in a browse
/// result, which does not read them.
pub(super) type Credits = Option<(Vec<CastCredit>, Vec<CrewCredit>)>;

pub(super) fn movie_metadata(
    movie: beam_domain::models::Movie,
    genres: Vec<String>,
    files: MovieFiles,
    credits: Credits,
    collection: Option<CollectionRef>,
) -> MediaMetadata {
    let beam_domain::models::Movie {
        id,
        title,
        identity_key: _,
        pinned_ref: _,
        pin_source: _,
        title_localized,
        description,
        year,
        release_date,
        runtime,
        poster_url,
        backdrop_url,
        tmdb_id,
        imdb_id,
        tvdb_id,
        anilist_id: _,
        rating_tmdb,
        rating_imdb: _,
        content_rating,
        created_at: _,
        updated_at: _,
    } = movie;
    let MovieFiles {
        duration,
        file_id,
        source_count,
    } = files;
    let (cast, crew) = credits.unzip();
    MediaMetadata::Movie(MovieMetadata {
        id,
        title: Title {
            original: title,
            localized: title_localized,
            alternatives: None,
        },
        description,
        year,
        release_date,
        runtime_mins: runtime.map(|d| (d.as_secs() / 60) as u32),
        duration_secs: duration,
        poster_url: poster_url
            .map(|_| artwork_path(ArtworkKind::Movie, id, ArtworkVariant::Poster)),
        backdrop_url: backdrop_url
            .map(|_| artwork_path(ArtworkKind::Movie, id, ArtworkVariant::Backdrop)),
        genres,
        ratings: ratings(rating_tmdb),
        content_rating,
        identifiers: identifiers(imdb_id, tmdb_id, tvdb_id),
        file_id,
        source_count,
        user_state: UserTitleState::default(),
        cast,
        crew,
        collection,
    })
}

pub(super) fn show_metadata(
    show: beam_domain::models::Show,
    genres: Vec<String>,
    counts: ShowChildCounts,
    seasons: Vec<SeasonMetadata>,
    credits: Credits,
) -> MediaMetadata {
    let beam_domain::models::Show {
        id,
        title,
        identity_key: _,
        pinned_ref: _,
        pin_source: _,
        title_localized,
        description,
        year,
        poster_url,
        backdrop_url,
        tmdb_id,
        imdb_id,
        tvdb_id,
        anilist_id: _,
        rating_tmdb,
        content_rating,
        created_at: _,
        updated_at: _,
    } = show;
    let ShowChildCounts {
        seasons: season_count,
        episodes: episode_count,
    } = counts;
    let (cast, crew) = credits.unzip();
    MediaMetadata::Show(ShowMetadata {
        id,
        title: Title {
            original: title,
            localized: title_localized,
            alternatives: None,
        },
        description,
        year,
        poster_url: poster_url.map(|_| artwork_path(ArtworkKind::Show, id, ArtworkVariant::Poster)),
        backdrop_url: backdrop_url
            .map(|_| artwork_path(ArtworkKind::Show, id, ArtworkVariant::Backdrop)),
        genres,
        ratings: ratings(rating_tmdb),
        content_rating,
        identifiers: identifiers(imdb_id, tmdb_id, tvdb_id),
        season_count,
        episode_count,
        seasons,
        user_state: None,
        cast,
        crew,
    })
}

/// The show heading an episode's or a season's detail.
pub(super) fn show_ref(show: &beam_domain::models::Show) -> ShowRef {
    ShowRef {
        id: show.id,
        title: Title {
            original: show.title.clone(),
            localized: show.title_localized.clone(),
            alternatives: None,
        },
        poster_url: show
            .poster_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Show, show.id, ArtworkVariant::Poster)),
        backdrop_url: show
            .backdrop_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Show, show.id, ArtworkVariant::Backdrop)),
    }
}

pub(super) fn season_metadata(
    season: beam_domain::models::Season,
    episodes: Vec<EpisodeMetadata>,
) -> SeasonMetadata {
    let dates = ShowDates {
        first_aired: season.first_aired,
        last_aired: season.last_aired,
    };
    SeasonMetadata {
        id: season.id,
        season_number: season.season_number,
        dates,
        episode_runtime_mins: None,
        episodes,
        poster_url: season
            .poster_url
            .as_ref()
            .map(|_| artwork_path(ArtworkKind::Season, season.id, ArtworkVariant::Poster)),
        genres: vec![],
        ratings: None,
        identifiers: None,
        user_state: None,
    }
}
