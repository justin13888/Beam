//! Wire types for an administrator's control of enrichment (issue #185):
//! where each title stands (FR-303), the candidates a fix-match picks from,
//! field locks, refreshes (FR-308), enrichment events on the admin stream
//! (FR-309), and each metadata provider's configuration (FR-307).
//!
//! Named without `Dto`/`Response` suffixes, per the wire conventions; the
//! domain types they mirror stay free of any web-framework derive.

use beam_domain::models::catalog;
use beam_domain::models::enrichment as domain;
use beam_domain::models::pin as domain_pin;
use beam_index::services::enrichment::control::{
    MatchCandidate as DomainCandidate, TitleEnrichment,
};
use beam_index::services::notification::EnrichmentEvent as DomainEvent;
use chrono::{DateTime, Utc};
use kynos::Schema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::search::{PageInfo, TitleKind, UnknownVariant};

impl From<catalog::TitleKind> for TitleKind {
    fn from(kind: catalog::TitleKind) -> Self {
        match kind {
            catalog::TitleKind::Movie => TitleKind::Movie,
            catalog::TitleKind::Show => TitleKind::Show,
        }
    }
}

/// Where a title's enrichment stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentStatus {
    /// Waiting for the enrichment worker, or to be retried.
    Pending,
    /// Matched and written.
    Enriched,
    /// No candidate was trusted, or the chosen id has no title: fix the
    /// match.
    Unmatched,
    /// Gave up after repeated errors.
    Failed,
}

impl EnrichmentStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Enriched => "enriched",
            Self::Unmatched => "unmatched",
            Self::Failed => "failed",
        }
    }
}

impl From<domain::EnrichmentStatus> for EnrichmentStatus {
    fn from(status: domain::EnrichmentStatus) -> Self {
        match status {
            domain::EnrichmentStatus::Pending => Self::Pending,
            domain::EnrichmentStatus::Enriched => Self::Enriched,
            domain::EnrichmentStatus::Unmatched => Self::Unmatched,
            domain::EnrichmentStatus::Failed => Self::Failed,
        }
    }
}

impl From<EnrichmentStatus> for domain::EnrichmentStatus {
    fn from(status: EnrichmentStatus) -> Self {
        match status {
            EnrichmentStatus::Pending => Self::Pending,
            EnrichmentStatus::Enriched => Self::Enriched,
            EnrichmentStatus::Unmatched => Self::Unmatched,
            EnrichmentStatus::Failed => Self::Failed,
        }
    }
}

impl std::fmt::Display for EnrichmentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for EnrichmentStatus {
    type Err = UnknownVariant;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        domain::EnrichmentStatus::ALL
            .into_iter()
            .map(Self::from)
            .find(|status| status.as_str() == value)
            .ok_or_else(|| UnknownVariant::new("pending, enriched, unmatched, failed", value))
    }
}

impl kynos::schema::ParamValue for EnrichmentStatus {}

/// A piece of a title's metadata an administrator can lock, so enrichment
/// leaves it as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum MetadataField {
    Title,
    OriginalTitle,
    Description,
    Year,
    /// Movies only.
    ReleaseDate,
    /// Movies only.
    Runtime,
    Poster,
    Backdrop,
    Rating,
    Genres,
}

impl From<domain::MetadataField> for MetadataField {
    fn from(field: domain::MetadataField) -> Self {
        match field {
            domain::MetadataField::Title => Self::Title,
            domain::MetadataField::OriginalTitle => Self::OriginalTitle,
            domain::MetadataField::Description => Self::Description,
            domain::MetadataField::Year => Self::Year,
            domain::MetadataField::ReleaseDate => Self::ReleaseDate,
            domain::MetadataField::Runtime => Self::Runtime,
            domain::MetadataField::Poster => Self::Poster,
            domain::MetadataField::Backdrop => Self::Backdrop,
            domain::MetadataField::Rating => Self::Rating,
            domain::MetadataField::Genres => Self::Genres,
        }
    }
}

impl From<MetadataField> for domain::MetadataField {
    fn from(field: MetadataField) -> Self {
        match field {
            MetadataField::Title => Self::Title,
            MetadataField::OriginalTitle => Self::OriginalTitle,
            MetadataField::Description => Self::Description,
            MetadataField::Year => Self::Year,
            MetadataField::ReleaseDate => Self::ReleaseDate,
            MetadataField::Runtime => Self::Runtime,
            MetadataField::Poster => Self::Poster,
            MetadataField::Backdrop => Self::Backdrop,
            MetadataField::Rating => Self::Rating,
            MetadataField::Genres => Self::Genres,
        }
    }
}

/// Who pinned a title to its provider id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum PinSource {
    /// A Kodi `.nfo` beside the media.
    Nfo,
    /// An administrator's fix-match, which no NFO replaces.
    Admin,
}

impl From<domain_pin::PinSource> for PinSource {
    fn from(source: domain_pin::PinSource) -> Self {
        match source {
            domain_pin::PinSource::Nfo => Self::Nfo,
            domain_pin::PinSource::Admin => Self::Admin,
        }
    }
}

/// Where one title's enrichment stands, with the title it is for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Schema)]
pub struct MediaEnrichment {
    /// The movie's or show's id.
    pub media_id: Uuid,
    pub kind: TitleKind,
    /// The title's display title.
    pub title: String,
    pub year: Option<u32>,
    pub status: EnrichmentStatus,
    /// Failed attempts since the title was last queued.
    pub attempt_count: u32,
    /// The `"provider:id"` the title is matched to, such as `tmdb:603`.
    pub matched_ref: Option<String>,
    /// How sure the match is, from 0 to 1; 1 for a title fetched by its pin.
    pub match_confidence: Option<f32>,
    /// The `"provider:id"` the title is pinned to, which enrichment fetches
    /// it by.
    pub pinned_ref: Option<String>,
    /// Who pinned it; set exactly when `pinned_ref` is.
    pub pin_source: Option<PinSource>,
    /// The fields enrichment leaves as they are.
    pub locked_fields: Vec<MetadataField>,
    /// Why the last attempt did not enrich the title.
    pub last_error: Option<String>,
    /// When a pending title is next attempted, after an error.
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub enriched_at: Option<DateTime<Utc>>,
    /// When its enrichment last changed; absent for a title not queued yet.
    pub updated_at: Option<DateTime<Utc>>,
}

impl From<TitleEnrichment> for MediaEnrichment {
    fn from(view: TitleEnrichment) -> Self {
        let TitleEnrichment {
            target,
            title,
            year,
            pinned_ref,
            pin_source,
            status,
            attempts,
            matched_ref,
            match_confidence,
            last_error,
            next_attempt_at,
            enriched_at,
            locked_fields,
            updated_at,
            position: _,
        } = view;
        Self {
            media_id: target.id(),
            kind: target.kind().into(),
            title,
            year,
            status: status.into(),
            attempt_count: attempts,
            matched_ref,
            match_confidence,
            pinned_ref,
            pin_source: pin_source.map(PinSource::from),
            locked_fields: locked_fields.iter().map(MetadataField::from).collect(),
            last_error,
            next_attempt_at,
            enriched_at,
            updated_at,
        }
    }
}

/// A page of titles by enrichment status.
///
/// Newest change first. Pass `page_info.end_cursor` as `after` for the next
/// page, with the same filters.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct MediaEnrichmentConnection {
    pub items: Vec<MediaEnrichment>,
    pub page_info: PageInfo,
    /// How many titles the filters admit, across every page.
    pub total: u64,
}

/// A title a provider offers for a fix-match.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Schema)]
pub struct MatchCandidate {
    /// The `"provider:id"` to fix the match to.
    pub external_ref: String,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u32>,
    /// How well it matches the search, from 0 to 1, as the enrichment worker
    /// scores a candidate.
    pub score: f64,
}

impl From<DomainCandidate> for MatchCandidate {
    fn from(candidate: DomainCandidate) -> Self {
        let DomainCandidate {
            external_ref,
            title,
            original_title,
            year,
            score,
        } = candidate;
        Self {
            external_ref,
            title,
            original_title,
            year,
            score,
        }
    }
}

/// The candidates for a fix-match, best first: one page, never more.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct MatchCandidateConnection {
    pub items: Vec<MatchCandidate>,
    /// Always a single page.
    pub page_info: PageInfo,
}

/// Body of `POST /v1/admin/media/{id}/match`.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct FixMatchRequest {
    /// The `"provider:id"` to fix the match to, such as `tmdb:603`, from a
    /// provider that is configured.
    pub external_ref: String,
}

/// Body of `PUT /v1/admin/media/{id}/enrichment/locks`.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct SetFieldLocksRequest {
    /// Every field to lock; any field not listed is unlocked.
    pub locked_fields: Vec<MetadataField>,
}

/// What a refresh queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct RefreshQueued {
    /// Titles queued for another pass.
    pub queued_count: u64,
}

/// A metadata provider enrichment can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum MetadataProvider {
    Tmdb,
    Anilist,
}

impl MetadataProvider {
    /// Every provider, in the order enrichment asks them.
    pub const ALL: [MetadataProvider; 2] = [MetadataProvider::Tmdb, MetadataProvider::Anilist];

    /// The name `available_providers` reports it by.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tmdb => "tmdb",
            Self::Anilist => "anilist",
        }
    }
}

/// Whether a provider is set up (FR-307).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentProviderState {
    /// Configured and in use.
    Configured,
    /// Not configured: titles it would enrich stay un-enriched.
    NotConfigured,
    /// Configured, but the client could not be built.
    Unavailable,
}

/// One provider's configuration, for the admin status (FR-307).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct EnrichmentProviderStatus {
    pub provider: MetadataProvider,
    pub state: EnrichmentProviderState,
    /// What to do about it, when it is not configured or unavailable.
    pub detail: Option<String>,
}

/// The title an `enrichment` admin event reports on (FR-309).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Schema)]
pub struct EnrichmentEvent {
    pub media_id: Uuid,
    pub kind: TitleKind,
    /// The display title, when the title still exists.
    pub title: Option<String>,
    pub status: EnrichmentStatus,
    pub matched_ref: Option<String>,
    /// Why it was not enriched; `null` once it was.
    pub error: Option<String>,
}

impl From<DomainEvent> for EnrichmentEvent {
    fn from(event: DomainEvent) -> Self {
        let DomainEvent {
            target,
            title,
            status,
            matched_ref,
            error,
        } = event;
        Self {
            media_id: target.id(),
            kind: target.kind().into(),
            title,
            status: status.into(),
            matched_ref,
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn every_documented_status_parses_as_a_query_value() {
        for status in domain::EnrichmentStatus::ALL.map(EnrichmentStatus::from) {
            let json = serde_json::to_string(&status).expect("serializes");
            let wire = json.trim_matches('"');
            assert_eq!(EnrichmentStatus::from_str(wire).unwrap(), status);
            assert_eq!(status.to_string(), wire);
            assert_eq!(
                EnrichmentStatus::from(domain::EnrichmentStatus::from(status)),
                status
            );
        }
        assert!(EnrichmentStatus::from_str("Pending").is_err());
    }

    #[test]
    fn every_field_is_spelled_on_the_wire_as_it_is_stored() {
        for field in domain::MetadataField::ALL {
            let wire = serde_json::to_string(&MetadataField::from(field)).expect("serializes");
            assert_eq!(wire.trim_matches('"'), field.as_str());
            assert_eq!(
                domain::MetadataField::from(MetadataField::from(field)),
                field
            );
        }
    }

    #[test]
    fn every_pin_source_is_spelled_on_the_wire_as_it_is_stored() {
        for source in domain_pin::PinSource::ALL {
            let wire = serde_json::to_string(&PinSource::from(source)).expect("serializes");
            assert_eq!(wire.trim_matches('"'), source.as_str());
        }
    }

    #[test]
    fn every_provider_is_spelled_on_the_wire_as_available_providers_names_it() {
        for provider in MetadataProvider::ALL {
            let wire = serde_json::to_string(&provider).expect("serializes");
            assert_eq!(wire.trim_matches('"'), provider.as_str());
        }
    }
}
