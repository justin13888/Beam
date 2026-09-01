//! `/v1/artwork/{kind}/{id}/{variant}` -- poster, backdrop and thumbnail art,
//! served by Beam rather than by the provider's CDN (ADR-0015).
//!
//! **Why the URL names the title rather than the image.** A content-addressed
//! path would let every response be `immutable`, but it changes whenever
//! enrichment refreshes a title's art, and clients store these URLs: the
//! Android downloads screen writes `posterUrl` down at enqueue time so a
//! downloaded title can render with no network at all. A URL that 404s after
//! the next enrichment pass would break exactly the offline case this endpoint
//! exists to fix. So the URL is stable and the *validator* moves instead: the
//! `ETag` is the cache key, which is a digest of the provider URL, so it
//! changes precisely when the artwork does. Revalidation costs one primary-key
//! row read and a 304 with no body.
//!
//! **Why there is no SSRF surface.** The only URL ever fetched is one that
//! enrichment itself wrote onto a row. Nothing a client sends is a URL -- it is
//! an id and two enum variants -- so there is no allowlist to maintain and no
//! bypass to find.

use beam_domain::providers::artwork::ArtworkFetchError;
use kynos::http::etag::ETag;
use kynos::prelude::*;
use kynos::response::range::served::{Conditions, Served};
use kynos::response::range::source::InMemory;
use tracing::error;
use uuid::Uuid;

use crate::models::media::{ArtworkKind, ArtworkVariant};
use crate::routes::api_error::{DeliveryError, SessionAuth};
use crate::routes::delivery::{AnyMedia, MediaRanges, RuntimeDelivery};
use crate::routes::tags::Media;
use crate::state::AppState;

/// What the artwork endpoint captures.
#[derive(Debug, Schema, PathParams)]
pub struct ArtworkPath {
    /// Which kind of title the artwork belongs to.
    pub kind: ArtworkKind,
    /// The title's ID.
    pub id: String,
    /// Which image of that title.
    pub variant: ArtworkVariant,
}

/// The media-type ranges an artwork delivery answers with.
///
/// One range: whatever the provider returned is an image or it was refused
/// before it could be cached.
pub struct ArtworkRanges;

impl MediaRanges for ArtworkRanges {
    const RANGES: &'static [&'static str] = &["image/*"];
}

/// A delivery of one cached image.
pub type ArtworkDelivery = RuntimeDelivery<ArtworkRanges>;

/// The provider URL enrichment stored for this title and variant, if any.
///
/// `None` covers three cases that are one answer to a client: the title does
/// not exist, it exists but has no art yet, and the variant does not apply to
/// this kind of title.
async fn upstream_url(
    state: &AppState,
    path: &ArtworkPath,
) -> Result<Option<String>, DeliveryError> {
    let Ok(id) = Uuid::parse_str(&path.id) else {
        return Ok(None);
    };
    let services = &state.services;

    let url = match (path.kind, path.variant) {
        (ArtworkKind::Movie, variant) => services
            .movie_repo
            .find_by_id(id)
            .await
            .map_err(lookup_failed)?
            .and_then(|movie| match variant {
                ArtworkVariant::Poster => movie.poster_url,
                ArtworkVariant::Backdrop => movie.backdrop_url,
                ArtworkVariant::Thumbnail => None,
            }),
        (ArtworkKind::Show, variant) => services
            .show_repo
            .find_by_id(id)
            .await
            .map_err(lookup_failed)?
            .and_then(|show| match variant {
                ArtworkVariant::Poster => show.poster_url,
                ArtworkVariant::Backdrop => show.backdrop_url,
                ArtworkVariant::Thumbnail => None,
            }),
        (ArtworkKind::Season, ArtworkVariant::Poster) => services
            .show_repo
            .find_season_by_id(id)
            .await
            .map_err(lookup_failed)?
            .and_then(|season| season.poster_url),
        (ArtworkKind::Episode, ArtworkVariant::Thumbnail) => services
            .show_repo
            .find_episode_by_id(id)
            .await
            .map_err(lookup_failed)?
            .and_then(|episode| episode.thumbnail_url),
        // A season backdrop or an episode poster is not a thing Beam stores.
        (ArtworkKind::Season | ArtworkKind::Episode, _) => None,
    };

    Ok(url)
}

fn lookup_failed(err: sea_orm::DbErr) -> DeliveryError {
    error!(?err, "artwork title lookup failed");
    DeliveryError::Internal("Failed to look up artwork".into())
}

/// Builds the delivery both methods share.
async fn deliver(
    state: &AppState,
    path: &ArtworkPath,
    conditions: &Conditions,
) -> Result<ArtworkDelivery, DeliveryError> {
    let Some(url) = upstream_url(state, path).await? else {
        return Err(DeliveryError::NotFound("No artwork for this title".into()));
    };

    let image = state.services.artwork.get(&url).await.map_err(|err| {
        match err {
            // The provider no longer has it, or never had it. To a viewer this
            // is the same as a title with no art: a placeholder, not an error
            // page. The cache remembers the failure so a whole grid of clients
            // does not re-ask a dead upstream.
            ArtworkFetchError::NotFound => {
                DeliveryError::NotFound("No artwork for this title".into())
            }
            other => {
                error!(%url, %other, "artwork could not be fetched");
                DeliveryError::Internal("Failed to fetch artwork".into())
            }
        }
    })?;

    // No `last_modified`: the file's mtime records when Beam cached the image,
    // not when the artwork changed, so it would be a validator that moves for
    // the wrong reason. The `ETag` is the cache key -- a digest of the provider
    // URL -- which moves exactly when enrichment points the title somewhere
    // else.
    let served = Served::<_, AnyMedia>::new(InMemory::new(image.bytes))
        .etag(ETag::strong(image.key.to_string()))
        .cache_control("public, max-age=86400");

    let delivery = served.deliver(conditions).await.map_err(|err| {
        error!(?err, "failed to read cached artwork");
        DeliveryError::Internal("Failed to read artwork".into())
    })?;

    Ok(ArtworkDelivery::new(
        delivery,
        image.format.content_type().to_owned(),
    ))
}

/// Poster, backdrop or thumbnail art for one title.
///
/// Beam fetches the image from the metadata provider once and serves it from
/// its own cache thereafter, so a viewer's browser never contacts TMDB or
/// AniList and those CDNs never see who is looking at what (ADR-0015).
///
/// The URL is stable across re-enrichment; the strong `ETag` is what changes
/// when a title's artwork does, so clients revalidate rather than re-download.
/// A title with no art, an id that does not exist, and a variant that does not
/// apply to this kind of title are all `404` -- every client already renders a
/// placeholder for that.
#[kynos::get("/artwork/{kind}/{id}/{variant}", tag = Media, operation_id = "getArtwork")]
#[tracing::instrument(skip_all)]
pub async fn get_artwork(
    _auth: SessionAuth,
    Path(path): Path<ArtworkPath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<ArtworkDelivery, DeliveryError> {
    deliver(&state, &path, &conditions).await
}

/// Headers for [`get_artwork`], without the image.
///
/// Declared rather than synthesised: Kynos does not derive a `HEAD` from a
/// `GET`, because the two are separate operations in the description.
#[kynos::head("/artwork/{kind}/{id}/{variant}", tag = Media, operation_id = "headArtwork")]
#[tracing::instrument(skip_all)]
pub async fn head_artwork(
    _auth: SessionAuth,
    Path(path): Path<ArtworkPath>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<ArtworkDelivery, DeliveryError> {
    deliver(&state, &path, &conditions).await
}

#[path = "artwork_tests.rs"]
mod artwork_tests;
