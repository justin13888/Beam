//! `/v1/artwork/{kind}/{id}/{variant}` -- poster, backdrop and thumbnail art,
//! and people's portraits, served by Beam rather than by the provider's CDN
//! (ADR-0015).
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
//! row read and a read of the cached file, which the OS page cache is holding;
//! the 304 itself carries no body. The validator comes from the row alone, but
//! `Served::deliver` asks the source for its complete length before it
//! evaluates `If-None-Match`, so [`deliver`] has the bytes in hand by then.
//!
//! **Why there is no SSRF surface.** The only URL ever fetched is one that
//! enrichment itself wrote onto a row. Nothing a client sends is a URL -- it is
//! an id and two enum variants -- so there is no allowlist to maintain and no
//! bypass to find.

use beam_domain::models::access::AccessPolicy;
use beam_domain::providers::artwork::ArtworkFetchError;
use kynos::http::etag::ETag;
use kynos::prelude::*;
use kynos::response::range::served::{Conditions, Served};
use kynos::response::range::source::InMemory;
use serde::{Deserialize, Serialize};
use tracing::error;
use uuid::Uuid;

use crate::models::media::{ArtworkKind, ArtworkSize, ArtworkVariant};
use crate::routes::api_error::{ArtworkError, ServerFault, SessionAuth};
use crate::routes::delivery::{AnyMedia, MediaRanges, RuntimeDelivery};
use crate::routes::playback::{caller_access, hides};
use crate::routes::tags::Media;
use crate::state::AppState;

/// What the artwork endpoint captures.
#[derive(Debug, Schema, PathParams)]
pub struct ArtworkPath {
    /// What the artwork belongs to: a kind of title, or a person.
    pub kind: ArtworkKind,
    /// The title's or the person's ID.
    pub id: Uuid,
    /// Which image of it.
    pub variant: ArtworkVariant,
}

/// How the artwork endpoint is sized.
#[derive(Debug, Default, Serialize, Deserialize, Schema, QueryParams)]
pub struct ArtworkQuery {
    /// Scale the image to this width, keeping its aspect ratio; omit it for
    /// the original. An image already no wider than this, or a GIF or AVIF,
    /// is served at its original size -- Beam never upscales. Each size is
    /// its own resource with its own `ETag`.
    pub size: Option<ArtworkSize>,
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
/// this kind of title. An id that is not an id at all is not among them: the
/// `Path` extractor answers it 400 before the handler runs. A portrait or a
/// collection's art is `None` too when `policy` hides every title its person
/// or collection is on (issue #328).
async fn upstream_url(
    state: &AppState,
    path: &ArtworkPath,
    policy: &AccessPolicy,
) -> Result<Option<String>, ArtworkError> {
    let id = path.id;
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
                ArtworkVariant::Thumbnail | ArtworkVariant::Profile => None,
            }),
        (ArtworkKind::Show, variant) => services
            .show_repo
            .find_by_id(id)
            .await
            .map_err(lookup_failed)?
            .and_then(|show| match variant {
                ArtworkVariant::Poster => show.poster_url,
                ArtworkVariant::Backdrop => show.backdrop_url,
                ArtworkVariant::Thumbnail | ArtworkVariant::Profile => None,
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
        (ArtworkKind::Person, ArtworkVariant::Profile) => services
            .people
            .find_person(id, policy)
            .await
            .map_err(lookup_failed)?
            .and_then(|person| person.profile_url),
        (ArtworkKind::Collection, variant) => services
            .collections
            .find_collection(id, policy)
            .await
            .map_err(lookup_failed)?
            .and_then(|collection| match variant {
                ArtworkVariant::Poster => collection.poster_url,
                ArtworkVariant::Backdrop => collection.backdrop_url,
                ArtworkVariant::Thumbnail | ArtworkVariant::Profile => None,
            }),
        // A season backdrop, an episode poster or a person's poster is not a
        // thing Beam stores.
        (ArtworkKind::Season | ArtworkKind::Episode | ArtworkKind::Person, _) => None,
    };

    Ok(url)
}

fn lookup_failed(err: sea_orm::DbErr) -> ArtworkError {
    error!(?err, "artwork title lookup failed");
    ArtworkError::Internal(ServerFault::logged("Failed to look up artwork"))
}

/// Builds the delivery both methods share.
async fn deliver(
    state: &AppState,
    auth: &SessionAuth,
    path: &ArtworkPath,
    size: Option<ArtworkSize>,
    conditions: &Conditions,
) -> Result<ArtworkDelivery, ArtworkError> {
    // The art of a title the caller may not see is the art of a title that
    // does not exist (issue #263). A person's portrait and a collection's art
    // belong to no one title: they are served while the policy admits one
    // title the person or collection is on (issue #328).
    // Both lookups log their cause where they build the fault, so it moves
    // across as is.
    let (_, policy) = caller_access(state, auth).await?;
    let of_a_title = matches!(
        path.kind,
        ArtworkKind::Movie | ArtworkKind::Show | ArtworkKind::Season | ArtworkKind::Episode
    );
    if of_a_title && hides(state, &policy, path.id).await? {
        return Err(ArtworkError::NotFound("No artwork for this title".into()));
    }
    let Some(url) = upstream_url(state, path, &policy).await? else {
        return Err(ArtworkError::NotFound("No artwork for this title".into()));
    };

    let fetched = match size {
        Some(size) => state.services.artwork.get_sized(&url, size.width()).await,
        None => state.services.artwork.get(&url).await,
    };
    let image = fetched.map_err(|err| {
        match err {
            // The provider no longer has it, or never had it. To a viewer this
            // is the same as a title with no art: a placeholder, not an error
            // page. The cache remembers the failure so a whole grid of clients
            // does not re-ask a dead upstream.
            ArtworkFetchError::NotFound => {
                ArtworkError::NotFound("No artwork for this title".into())
            }
            // The fetcher declined the URL before any request left: it is not
            // `https`, or does not parse. Nothing upstream was asked anything,
            // so this cannot be the provider's fault -- enrichment wrote a URL
            // onto a row that Beam itself will not fetch. That is bad data Beam
            // stored, which is what `internal` means.
            ArtworkFetchError::Refused {
                url: refused,
                reason,
            } => {
                error!(%url, %refused, %reason, "stored artwork url refused by the fetcher");
                ArtworkError::Internal(ServerFault::logged("Failed to fetch artwork"))
            }
            // The provider answered and Beam could not use the answer, or the
            // provider did not answer at all.
            ArtworkFetchError::Unsupported { content_type } => {
                error!(%url, %content_type, "artwork provider returned a non-image");
                ArtworkError::UpstreamFailed("Failed to fetch artwork".into())
            }
            ArtworkFetchError::TooLarge { limit } => {
                error!(%url, limit, "artwork provider returned a body over the ceiling");
                ArtworkError::UpstreamFailed("Failed to fetch artwork".into())
            }
            ArtworkFetchError::Upstream { status } => {
                error!(%url, status, "artwork provider returned an error status");
                ArtworkError::UpstreamFailed("Failed to fetch artwork".into())
            }
            ArtworkFetchError::Transport(reason) => {
                error!(%url, %reason, "artwork provider could not be reached");
                ArtworkError::UpstreamFailed("Failed to fetch artwork".into())
            }
        }
    })?;

    // No `last_modified`: the file's mtime records when Beam cached the image,
    // not when the artwork changed, so it would be a validator that moves for
    // the wrong reason. The `ETag` is the cache key -- a digest of the provider
    // URL -- which moves exactly when enrichment points the title somewhere
    // else.
    // `private`: whether the art is served at all depends on who asks, so a
    // shared cache must not answer one viewer with what it stored for
    // another (issue #263).
    let served = Served::<_, AnyMedia>::new(InMemory::new(image.bytes))
        .etag(ETag::strong(image.key.to_string()))
        .cache_control("private, max-age=86400");

    let delivery = served.deliver(conditions).await.map_err(|err| {
        error!(?err, "failed to read cached artwork");
        ArtworkError::Internal(ServerFault::logged("Failed to read artwork"))
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
/// `size` asks for a narrower rendition, resized once and cached beside the
/// original; each size has its own `ETag`.
///
/// A title with no art, an id that does not exist, and a variant that does
/// not apply to this kind of title are all `404` -- every client already
/// renders a placeholder for that. An id that is not a UUID is a `400`, as on every
/// other route that takes a media id.
#[kynos::get("/artwork/{kind}/{id}/{variant}", tag = Media, operation_id = "getArtwork")]
#[tracing::instrument(skip_all)]
pub async fn get_artwork(
    auth: SessionAuth,
    Path(path): Path<ArtworkPath>,
    Query(query): Query<ArtworkQuery>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<ArtworkDelivery, ArtworkError> {
    let ArtworkQuery { size } = query;
    deliver(&state, &auth, &path, size, &conditions).await
}

/// Headers for [`get_artwork`], without the image.
///
/// Declared rather than synthesised: Kynos does not derive a `HEAD` from a
/// `GET`, because the two are separate operations in the description.
#[kynos::head("/artwork/{kind}/{id}/{variant}", tag = Media, operation_id = "headArtwork")]
#[tracing::instrument(skip_all)]
pub async fn head_artwork(
    auth: SessionAuth,
    Path(path): Path<ArtworkPath>,
    Query(query): Query<ArtworkQuery>,
    conditions: Conditions,
    Inject(state): Inject<AppState>,
) -> Result<ArtworkDelivery, ArtworkError> {
    let ArtworkQuery { size } = query;
    deliver(&state, &auth, &path, size, &conditions).await
}

#[path = "artwork_tests.rs"]
mod artwork_tests;
