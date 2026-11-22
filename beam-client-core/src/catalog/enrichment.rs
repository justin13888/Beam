//! The metadata operator's views: where each title's enrichment stands, the
//! candidates a fix-match picks from, and the fields an administrator locks.

use super::media::MediaKind;
use super::narrow_u32;
use super::watching::kind_from_wire;
use crate::api::types as wire;

/// Where a title's enrichment stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum EnrichmentStatus {
    /// Queued, or waiting to be tried again after an error.
    Pending,
    /// Matched and fetched.
    Enriched,
    /// Searched for and matched to nothing: it needs a fix-match.
    Unmatched,
    /// Gave up after repeated errors.
    Failed,
}

/// Who pinned a title to its provider id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PinSource {
    /// An NFO file beside the title's media names it.
    Nfo,
    /// An administrator fixed the match.
    Admin,
}

/// A piece of a title's metadata an administrator can lock, so enrichment
/// leaves it as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum MetadataField {
    /// The display title.
    Title,
    /// The title in its original language.
    OriginalTitle,
    /// The synopsis.
    Description,
    /// The release year.
    Year,
    /// The release date; a movie's alone.
    ReleaseDate,
    /// The runtime; a movie's alone.
    Runtime,
    /// The poster artwork.
    Poster,
    /// The backdrop artwork.
    Backdrop,
    /// The rating.
    Rating,
    /// The genres.
    Genres,
}

/// Where one title's enrichment stands, with the title it is for.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MediaEnrichment {
    /// The movie's or show's id.
    pub media_id: String,
    /// Movie or show.
    pub kind: MediaKind,
    /// The title's display title.
    pub title: String,
    /// Release year, where known.
    pub year: Option<u32>,
    /// Where its enrichment stands.
    pub status: EnrichmentStatus,
    /// Failed attempts since the title was last queued.
    pub attempt_count: u32,
    /// The `provider:id` the title is matched to, such as `tmdb:603`.
    pub matched_ref: Option<String>,
    /// How sure the match is, from 0 to 1.
    pub match_confidence: Option<f64>,
    /// The `provider:id` the title is pinned to, which enrichment fetches it by.
    pub pinned_ref: Option<String>,
    /// Who pinned it; set exactly when `pinned_ref` is.
    pub pin_source: Option<PinSource>,
    /// The fields enrichment leaves as they are.
    pub locked_fields: Vec<MetadataField>,
    /// Why the last attempt did not enrich the title.
    pub last_error: Option<String>,
    /// When a pending title is next attempted, as a Unix timestamp.
    pub next_attempt_at_unix: Option<i64>,
    /// When it was last enriched, as a Unix timestamp.
    pub enriched_at_unix: Option<i64>,
    /// When its enrichment last changed, as a Unix timestamp.
    pub updated_at_unix: Option<i64>,
}

/// One page of titles by enrichment status, newest change first.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MediaEnrichmentPage {
    /// The titles on this page.
    pub items: Vec<MediaEnrichment>,
    /// How many titles the filters admit, across every page.
    pub total: u64,
    /// Pass as `after`, with the same filters, for the next page; `None` on
    /// an empty page.
    pub end_cursor: Option<String>,
    /// Whether a page follows this one.
    pub has_next_page: bool,
}

/// A title a provider offers for a fix-match.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MatchCandidate {
    /// The `provider:id` to fix the match to.
    pub external_ref: String,
    /// The provider's title.
    pub title: String,
    /// The title in its original language, where the provider has one.
    pub original_title: Option<String>,
    /// Release year, where known.
    pub year: Option<u32>,
    /// How well it matches the search, from 0 to 1.
    pub score: f64,
}

impl EnrichmentStatus {
    pub(crate) fn from_wire(status: wire::EnrichmentStatus) -> Self {
        match status {
            wire::EnrichmentStatus::Pending => Self::Pending,
            wire::EnrichmentStatus::Enriched => Self::Enriched,
            wire::EnrichmentStatus::Unmatched => Self::Unmatched,
            wire::EnrichmentStatus::Failed => Self::Failed,
        }
    }

    pub(crate) fn to_wire(self) -> wire::EnrichmentStatus {
        match self {
            Self::Pending => wire::EnrichmentStatus::Pending,
            Self::Enriched => wire::EnrichmentStatus::Enriched,
            Self::Unmatched => wire::EnrichmentStatus::Unmatched,
            Self::Failed => wire::EnrichmentStatus::Failed,
        }
    }
}

impl MetadataField {
    fn from_wire(field: wire::MetadataField) -> Self {
        match field {
            wire::MetadataField::Title => Self::Title,
            wire::MetadataField::OriginalTitle => Self::OriginalTitle,
            wire::MetadataField::Description => Self::Description,
            wire::MetadataField::Year => Self::Year,
            wire::MetadataField::ReleaseDate => Self::ReleaseDate,
            wire::MetadataField::Runtime => Self::Runtime,
            wire::MetadataField::Poster => Self::Poster,
            wire::MetadataField::Backdrop => Self::Backdrop,
            wire::MetadataField::Rating => Self::Rating,
            wire::MetadataField::Genres => Self::Genres,
        }
    }

    pub(crate) fn to_wire(self) -> wire::MetadataField {
        match self {
            Self::Title => wire::MetadataField::Title,
            Self::OriginalTitle => wire::MetadataField::OriginalTitle,
            Self::Description => wire::MetadataField::Description,
            Self::Year => wire::MetadataField::Year,
            Self::ReleaseDate => wire::MetadataField::ReleaseDate,
            Self::Runtime => wire::MetadataField::Runtime,
            Self::Poster => wire::MetadataField::Poster,
            Self::Backdrop => wire::MetadataField::Backdrop,
            Self::Rating => wire::MetadataField::Rating,
            Self::Genres => wire::MetadataField::Genres,
        }
    }
}

impl MediaEnrichment {
    /// Normalise a generated enrichment row.
    #[must_use]
    pub fn from_generated(enrichment: wire::MediaEnrichment) -> Self {
        let wire::MediaEnrichment {
            attempt_count,
            enriched_at,
            kind,
            last_error,
            locked_fields,
            match_confidence,
            matched_ref,
            media_id,
            next_attempt_at,
            pin_source,
            pinned_ref,
            status,
            title,
            updated_at,
            year,
        } = enrichment;
        Self {
            media_id: media_id.to_string(),
            kind: kind_from_wire(kind),
            title,
            year: narrow_u32(year),
            status: EnrichmentStatus::from_wire(status),
            attempt_count: narrow_u32(Some(attempt_count)).unwrap_or(0),
            matched_ref,
            // A confidence off its 0-1 scale is not one; it is dropped rather
            // than shown as a percentage over a hundred.
            match_confidence: match_confidence
                .filter(|confidence| (0.0..=1.0).contains(confidence)),
            pinned_ref,
            pin_source: pin_source.map(|source| match source {
                wire::MediaEnrichmentpinSource::Nfo => PinSource::Nfo,
                wire::MediaEnrichmentpinSource::Admin => PinSource::Admin,
            }),
            locked_fields: locked_fields
                .into_iter()
                .map(MetadataField::from_wire)
                .collect(),
            last_error,
            next_attempt_at_unix: next_attempt_at.map(|at| at.0.unix_timestamp()),
            enriched_at_unix: enriched_at.map(|at| at.0.unix_timestamp()),
            updated_at_unix: updated_at.map(|at| at.0.unix_timestamp()),
        }
    }
}

impl MediaEnrichmentPage {
    /// Normalise a generated page of enrichment rows, keeping its cursor and
    /// total.
    #[must_use]
    pub fn from_generated(connection: wire::MediaEnrichmentConnection) -> Self {
        let wire::MediaEnrichmentConnection {
            items,
            page_info,
            total,
        } = connection;
        Self {
            items: items
                .into_iter()
                .map(MediaEnrichment::from_generated)
                .collect(),
            total: u64::try_from(total).unwrap_or(0),
            end_cursor: page_info.end_cursor,
            has_next_page: page_info.has_next_page,
        }
    }
}

impl MatchCandidate {
    /// Normalise a generated fix-match candidate.
    #[must_use]
    pub fn from_generated(candidate: wire::MatchCandidate) -> Self {
        let wire::MatchCandidate {
            external_ref,
            original_title,
            score,
            title,
            year,
        } = candidate;
        Self {
            external_ref,
            title,
            original_title,
            year: narrow_u32(year),
            score,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enrichment(extra: &str) -> wire::MediaEnrichment {
        let json = format!(
            r#"{{"media_id":"00000000-0000-0000-0000-0000000000d1","kind":"show",
                "title":"Dark","status":"unmatched","attempt_count":2,
                "locked_fields":["poster","genres"]{extra}}}"#
        );
        serde_json::from_str(&json).expect("a valid enrichment row")
    }

    /// An unmatched title carries why, and none of a match it does not have.
    #[test]
    fn an_unmatched_title_carries_its_error_and_no_match() {
        let row = MediaEnrichment::from_generated(enrichment(
            r#","year":2017,"last_error":"no candidate scored 0.8",
               "next_attempt_at":"2026-01-01T00:00:00Z",
               "updated_at":"2026-01-01T00:00:10Z""#,
        ));

        assert_eq!(
            row,
            MediaEnrichment {
                media_id: "00000000-0000-0000-0000-0000000000d1".to_owned(),
                kind: MediaKind::Show,
                title: "Dark".to_owned(),
                year: Some(2017),
                status: EnrichmentStatus::Unmatched,
                attempt_count: 2,
                matched_ref: None,
                match_confidence: None,
                pinned_ref: None,
                pin_source: None,
                locked_fields: vec![MetadataField::Poster, MetadataField::Genres],
                last_error: Some("no candidate scored 0.8".to_owned()),
                next_attempt_at_unix: Some(1_767_225_600),
                enriched_at_unix: None,
                updated_at_unix: Some(1_767_225_610),
            }
        );
    }

    /// A pinned title keeps who pinned it; a confidence off its 0-1 scale,
    /// and a negative year or attempt count, read as absent rather than as a
    /// number nobody sent.
    #[test]
    fn a_pinned_title_keeps_its_pin_and_drops_impossible_numbers() {
        let pinned = MediaEnrichment::from_generated(enrichment(
            r#","matched_ref":"tmdb:70523","match_confidence":1.0,
               "pinned_ref":"tmdb:70523","pin_source":"admin""#,
        ));
        assert_eq!(pinned.pinned_ref.as_deref(), Some("tmdb:70523"));
        assert_eq!(pinned.pin_source, Some(PinSource::Admin));
        assert_eq!(pinned.match_confidence, Some(1.0));

        let impossible = MediaEnrichment::from_generated(
            serde_json::from_str(
                r#"{"media_id":"00000000-0000-0000-0000-0000000000d1","kind":"movie",
                "title":"T","status":"enriched","attempt_count":-1,"year":-5,
                "match_confidence":1.5,"pin_source":"nfo","locked_fields":[]}"#,
            )
            .expect("a valid enrichment row"),
        );
        assert_eq!(impossible.match_confidence, None);
        assert_eq!(impossible.year, None);
        assert_eq!(impossible.attempt_count, 0);
        assert_eq!(impossible.pin_source, Some(PinSource::Nfo));
    }

    /// Every field the contract declares survives the trip to the record and
    /// back to the wire, so a lock the screen sets is the lock the server
    /// sees. The declared list is read from the document.
    #[test]
    fn every_metadata_field_round_trips_through_the_record() {
        let document: serde_json::Value =
            serde_json::from_str(include_str!("../../api/openapi.json")).expect("the document");
        let declared = document
            .pointer("/components/schemas/MetadataField/enum")
            .and_then(serde_json::Value::as_array)
            .expect("the contract declares the fields");
        assert!(!declared.is_empty());
        let mut seen = std::collections::HashSet::new();
        for spelling in declared {
            let field: wire::MetadataField =
                serde_json::from_value(spelling.clone()).expect("a declared field");
            let record = MetadataField::from_wire(field);
            assert!(
                seen.insert(record),
                "{spelling} maps onto a field another did"
            );
            assert_eq!(
                serde_json::to_value(record.to_wire()).expect("serialises"),
                *spelling
            );
        }
    }

    /// Every status the contract declares survives the trip both ways, so a
    /// filter the screen picks is the filter the server applies.
    #[test]
    fn every_enrichment_status_round_trips_through_the_record() {
        let document: serde_json::Value =
            serde_json::from_str(include_str!("../../api/openapi.json")).expect("the document");
        let declared = document
            .pointer("/components/schemas/EnrichmentStatus/enum")
            .and_then(serde_json::Value::as_array)
            .expect("the contract declares the statuses");
        let mut seen = std::collections::HashSet::new();
        for spelling in declared {
            let status: wire::EnrichmentStatus =
                serde_json::from_value(spelling.clone()).expect("a declared status");
            let record = EnrichmentStatus::from_wire(status);
            assert!(seen.insert(record), "{spelling} collides");
            assert_eq!(
                serde_json::to_value(record.to_wire()).expect("serialises"),
                *spelling
            );
        }
    }

    #[test]
    fn a_page_of_titles_keeps_its_cursor_and_a_negative_total_reads_as_zero() {
        let page = |total: i64| {
            let json = format!(
                r#"{{"items":[{{"media_id":"00000000-0000-0000-0000-0000000000d1","kind":"movie",
                      "title":"T","status":"failed","attempt_count":5,"locked_fields":[]}}],
                    "page_info":{{"has_next_page":true,"has_previous_page":false,
                                  "end_cursor":"e1"}},"total":{total}}}"#
            );
            MediaEnrichmentPage::from_generated(serde_json::from_str(&json).expect("a valid page"))
        };
        let first = page(31);
        assert_eq!(first.total, 31);
        assert_eq!(first.end_cursor.as_deref(), Some("e1"));
        assert!(first.has_next_page);
        assert_eq!(first.items[0].status, EnrichmentStatus::Failed);
        assert_eq!(page(-1).total, 0);
    }

    #[test]
    fn a_candidate_keeps_its_ref_and_score_and_drops_a_negative_year() {
        let candidate = |year: &str| {
            MatchCandidate::from_generated(
                serde_json::from_str(&format!(
                    r#"{{"external_ref":"tmdb:603","title":"The Matrix",
                        "original_title":null,"year":{year},"score":0.92}}"#
                ))
                .expect("a valid candidate"),
            )
        };
        assert_eq!(
            candidate("1999"),
            MatchCandidate {
                external_ref: "tmdb:603".to_owned(),
                title: "The Matrix".to_owned(),
                original_title: None,
                year: Some(1999),
                score: 0.92,
            }
        );
        assert_eq!(candidate("-1").year, None);
    }
}
