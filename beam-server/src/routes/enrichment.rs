//! `/v1/admin/enrichment` and `/v1/admin/media/{id}/…`: an administrator's
//! control of metadata enrichment (issue #185). Every operation is admin-only
//! (`AdminAuth`).
//!
//! The mutations answer at once: a fix-match pins the title and queues it, a
//! refresh queues titles -- keeping each match, or with `rematch=true`
//! matching each afresh -- and the enrichment worker does the fetching. Their
//! outcome arrives on the admin event stream as `enrichment` events (FR-309),
//! and in the list here.

use kynos::prelude::*;
use kynos::response::status::Accepted;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use beam_domain::models::enrichment::{EnrichmentListFilter, MetadataField as DomainField};
use beam_index::services::enrichment::control::{ControlError, RefreshScope};

use crate::models::{
    EnrichmentStatus, FieldError, FixMatchRequest, MatchCandidate, MatchCandidateConnection,
    MediaEnrichment, MediaEnrichmentConnection, MediaTypeFilter, PageInfo, RefreshQueued,
    SetFieldLocksRequest,
};
use crate::routes::api_error::{
    AdminAuth, EnrichmentListError, FieldLocksError, FixMatchError, InternalError,
    LibraryRefreshError, MatchCandidatesError, MediaEnrichmentError,
};
use crate::routes::tags::Admin;
use crate::services::enrichment_cursor;
use crate::services::metadata::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use crate::state::AppState;

/// What `/v1/admin/media/{id}/…` captures.
#[derive(Debug, Schema, PathParams)]
pub struct MediaIdPath {
    /// Media id: a movie's or a show's.
    pub id: Uuid,
}

/// What `/v1/admin/libraries/{id}/refresh` captures.
#[derive(Debug, Schema, PathParams)]
pub struct LibraryIdPath {
    /// Library id.
    pub id: Uuid,
}

/// Which titles the enrichment list shows, and which page.
#[derive(Debug, Default, Serialize, Deserialize, Schema, QueryParams)]
pub struct EnrichmentListQuery {
    /// Only titles in this status; every status when absent. `unmatched` and
    /// `failed` are the ones to fix.
    pub status: Option<EnrichmentStatus>,
    /// Only movies, or only shows; both when absent.
    pub kind: Option<MediaTypeFilter>,
    /// This many titles (1-100, default 20).
    //
    // Kynos 0.3.0 neither documents nor enforces `#[schema]` bounds on an
    // `Option` query field (tracked with #223); the handler enforces them.
    #[schema(minimum = 1, maximum = 100)]
    pub first: Option<u32>,
    /// An opaque cursor -- a page's `end_cursor` -- to page on from.
    pub after: Option<String>,
}

/// How a refresh treats each title's match.
#[derive(Debug, Default, Serialize, Deserialize, Schema, QueryParams)]
pub struct RefreshQuery {
    /// Discard each title's match and search for it again, by its pin if it
    /// has one, else by its name; `false` (the default) keeps each match and
    /// fetches it afresh.
    pub rematch: Option<bool>,
}

/// What to search a title's candidates by.
#[derive(Debug, Default, Serialize, Deserialize, Schema, QueryParams)]
pub struct MatchCandidatesQuery {
    /// Search for this instead of the title's own title.
    pub query: Option<String>,
    /// Prefer this release year instead of the title's own.
    pub year: Option<u32>,
}

/// Every title by enrichment status, newest change first (FR-303): what is
/// waiting, what was enriched, and -- with the last error -- what was left
/// unmatched or failed.
#[kynos::get("/admin/enrichment", tag = Admin, operation_id = "listEnrichment")]
pub async fn list_enrichment(
    _auth: AdminAuth,
    Query(query): Query<EnrichmentListQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<MediaEnrichmentConnection>, EnrichmentListError> {
    let EnrichmentListQuery {
        status,
        kind,
        first,
        after,
    } = query;
    let size = first.unwrap_or(DEFAULT_PAGE_SIZE);
    let Some(size) = std::num::NonZeroU32::new(size).filter(|size| size.get() <= MAX_PAGE_SIZE)
    else {
        return Err(EnrichmentListError::InvalidPagination(format!(
            "a page holds 1 to {MAX_PAGE_SIZE} titles, not {size}"
        )));
    };
    let after = after
        .as_deref()
        .map(enrichment_cursor::decode)
        .transpose()
        .map_err(|err| EnrichmentListError::InvalidCursor(err.to_string()))?;
    let has_previous_page = after.is_some();

    let page = state
        .services
        .enrichment_control
        .list(
            EnrichmentListFilter {
                status: status.map(Into::into),
                kind: kind.map(Into::into),
            },
            after,
            size,
        )
        .await
        .map_err(|err| EnrichmentListError::Internal(err.to_string()))?;
    let cursor_of = |item: &beam_index::services::enrichment::control::TitleEnrichment| {
        item.position.as_ref().map(enrichment_cursor::encode)
    };
    let page_info = PageInfo {
        has_next_page: page.has_next_page,
        has_previous_page,
        start_cursor: page.items.first().and_then(cursor_of),
        end_cursor: page.items.last().and_then(cursor_of),
    };
    Ok(Json(MediaEnrichmentConnection {
        items: page.items.into_iter().map(MediaEnrichment::from).collect(),
        page_info,
        total: page.total,
    }))
}

/// Where one title's enrichment stands.
#[kynos::get(
    "/admin/media/{id}/enrichment",
    tag = Admin,
    operation_id = "getMediaEnrichment"
)]
pub async fn get_media_enrichment(
    _auth: AdminAuth,
    Path(path): Path<MediaIdPath>,
    Inject(state): Inject<AppState>,
) -> Result<Json<MediaEnrichment>, MediaEnrichmentError> {
    let detail = state
        .services
        .enrichment_control
        .detail(path.id)
        .await
        .map_err(|err| match err {
            ControlError::MediaNotFound(_) => MediaEnrichmentError::MediaNotFound(err.to_string()),
            _ => MediaEnrichmentError::Internal(err.to_string()),
        })?;
    Ok(Json(MediaEnrichment::from(detail)))
}

/// The configured providers' candidates for a title, best first, to fix its
/// match to. Searches by the title's own title and year unless `query` or
/// `year` says otherwise. At most 10.
#[kynos::get(
    "/admin/media/{id}/match-candidates",
    tag = Admin,
    operation_id = "searchMatchCandidates"
)]
pub async fn search_match_candidates(
    _auth: AdminAuth,
    Path(path): Path<MediaIdPath>,
    Query(query): Query<MatchCandidatesQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Json<MatchCandidateConnection>, MatchCandidatesError> {
    let MatchCandidatesQuery { query, year } = query;
    let candidates = state
        .services
        .enrichment_control
        .candidates(path.id, query.as_deref(), year)
        .await
        .map_err(|err| match err {
            ControlError::MediaNotFound(_) => MatchCandidatesError::MediaNotFound(err.to_string()),
            ControlError::ProviderNotConfigured(detail) => {
                MatchCandidatesError::ProviderNotConfigured(detail)
            }
            ControlError::Provider(_) => MatchCandidatesError::ProviderError(err.to_string()),
            _ => MatchCandidatesError::Internal(err.to_string()),
        })?;
    Ok(Json(MatchCandidateConnection {
        items: candidates.into_iter().map(MatchCandidate::from).collect(),
        page_info: PageInfo {
            has_next_page: false,
            has_previous_page: false,
            start_cursor: None,
            end_cursor: None,
        },
    }))
}

/// Fix a title's match to a provider id.
///
/// The title is pinned to `external_ref` -- an administrator's pin, which
/// outranks an NFO's and which no NFO replaces -- and queued with its old
/// match cleared; the enrichment worker then fetches it by that id, with no
/// search. Answers at once with the title as queued; the outcome arrives as
/// an `enrichment` event. `DELETE` undoes it.
#[kynos::post(
    "/admin/media/{id}/match",
    tag = Admin,
    operation_id = "fixMediaMatch"
)]
pub async fn fix_media_match(
    auth: AdminAuth,
    Path(path): Path<MediaIdPath>,
    Inject(state): Inject<AppState>,
    Json(body): Json<FixMatchRequest>,
) -> Result<Accepted<Json<MediaEnrichment>>, FixMatchError> {
    let FixMatchRequest { external_ref } = body;
    let detail = state
        .services
        .enrichment_control
        .fix_match(path.id, &external_ref, &auth.0.user_id)
        .await
        .map_err(|err| match err {
            ControlError::MediaNotFound(_) => FixMatchError::MediaNotFound(err.to_string()),
            ControlError::ProviderNotConfigured(detail) => {
                FixMatchError::ProviderNotConfigured(detail)
            }
            ControlError::ExternalRefTaken(detail) => FixMatchError::ExternalRefTaken(detail),
            ControlError::InvalidExternalRef(detail) => FixMatchError::ValidationFailed {
                errors: vec![FieldError {
                    pointer: "/external_ref".to_owned(),
                    detail,
                }],
            },
            _ => FixMatchError::Internal(err.to_string()),
        })?;
    Ok(Accepted::new(Json(MediaEnrichment::from(detail))))
}

/// Clear an administrator's fixed match.
///
/// The administrator's pin goes; the title goes back to its NFO's pin, if an
/// NFO beside its files names one, or to being searched for by its name, and
/// is queued to be matched afresh. On a title no administrator pinned this
/// changes nothing -- its match is kept and it is not queued -- since an
/// NFO's pin is the NFO's to change; to match any title afresh, refresh it
/// with `rematch=true`. Either way the title is left with no administrator's
/// pin.
#[kynos::delete(
    "/admin/media/{id}/match",
    tag = Admin,
    operation_id = "clearMediaMatch"
)]
pub async fn clear_media_match(
    auth: AdminAuth,
    Path(path): Path<MediaIdPath>,
    Inject(state): Inject<AppState>,
) -> Result<NoContent, MediaEnrichmentError> {
    state
        .services
        .enrichment_control
        .clear_match(path.id, &auth.0.user_id)
        .await
        .map_err(|err| match err {
            ControlError::MediaNotFound(_) => MediaEnrichmentError::MediaNotFound(err.to_string()),
            _ => MediaEnrichmentError::Internal(err.to_string()),
        })?;
    Ok(NoContent)
}

/// Lock exactly these fields of a title, so enrichment leaves them as they
/// are; every field not listed is unlocked. A show has no release date or
/// runtime to lock. Takes effect from the title's next pass; queues none.
#[kynos::put(
    "/admin/media/{id}/enrichment/locks",
    tag = Admin,
    operation_id = "setMediaFieldLocks"
)]
pub async fn set_media_field_locks(
    auth: AdminAuth,
    Path(path): Path<MediaIdPath>,
    Inject(state): Inject<AppState>,
    Json(body): Json<SetFieldLocksRequest>,
) -> Result<Json<MediaEnrichment>, FieldLocksError> {
    let SetFieldLocksRequest { locked_fields } = body;
    let fields: Vec<DomainField> = locked_fields.into_iter().map(Into::into).collect();
    let detail = state
        .services
        .enrichment_control
        .set_locks(path.id, &fields, &auth.0.user_id)
        .await
        .map_err(|err| match err {
            ControlError::MediaNotFound(_) => FieldLocksError::MediaNotFound(err.to_string()),
            ControlError::FieldNotLockable(unlockable) => FieldLocksError::ValidationFailed {
                errors: unlockable
                    .into_iter()
                    .map(|(index, detail)| FieldError {
                        pointer: format!("/locked_fields/{index}"),
                        detail,
                    })
                    .collect(),
            },
            _ => FieldLocksError::Internal(err.to_string()),
        })?;
    Ok(Json(MediaEnrichment::from(detail)))
}

/// Queue every title for another enrichment pass, whatever its status
/// (FR-308). Each keeps its match, fetched afresh, unless `rematch`; one
/// that has none is searched for.
#[kynos::post(
    "/admin/media/refresh",
    tag = Admin,
    operation_id = "refreshAllMediaMetadata"
)]
pub async fn refresh_all_media_metadata(
    auth: AdminAuth,
    Query(query): Query<RefreshQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Accepted<Json<RefreshQueued>>, InternalError> {
    let RefreshQuery { rematch } = query;
    let queued_count = state
        .services
        .enrichment_control
        .refresh(RefreshScope::All, rematch.unwrap_or(false), &auth.0.user_id)
        .await
        .map_err(|err| InternalError::Internal(err.to_string()))?;
    Ok(Accepted::new(Json(RefreshQueued { queued_count })))
}

/// Queue every title in one library for another enrichment pass, whatever
/// its status (FR-308): each keeps its match unless `rematch`.
#[kynos::post(
    "/admin/libraries/{id}/refresh",
    tag = Admin,
    operation_id = "refreshLibraryMetadata"
)]
pub async fn refresh_library_metadata(
    auth: AdminAuth,
    Path(path): Path<LibraryIdPath>,
    Query(query): Query<RefreshQuery>,
    Inject(state): Inject<AppState>,
) -> Result<Accepted<Json<RefreshQueued>>, LibraryRefreshError> {
    let RefreshQuery { rematch } = query;
    let queued_count = state
        .services
        .enrichment_control
        .refresh(
            RefreshScope::Library(path.id),
            rematch.unwrap_or(false),
            &auth.0.user_id,
        )
        .await
        .map_err(|err| match err {
            ControlError::LibraryNotFound(_) => {
                LibraryRefreshError::LibraryNotFound(err.to_string())
            }
            _ => LibraryRefreshError::Internal(err.to_string()),
        })?;
    Ok(Accepted::new(Json(RefreshQueued { queued_count })))
}

#[cfg(test)]
#[path = "enrichment_tests.rs"]
mod enrichment_tests;
