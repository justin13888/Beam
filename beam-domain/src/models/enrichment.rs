use std::collections::BTreeSet;
use std::num::NonZeroU32;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::models::catalog::TitleKind;

/// Which title an enrichment row is for. Exactly one of movie/show, mirroring
/// the dual-nullable-FK-plus-CHECK pattern used by the `files` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnrichmentTargetId {
    Movie(Uuid),
    Show(Uuid),
}

impl EnrichmentTargetId {
    /// The title's id, whichever kind it is.
    #[must_use]
    pub const fn id(self) -> Uuid {
        match self {
            Self::Movie(id) | Self::Show(id) => id,
        }
    }

    /// Which kind of title this is.
    #[must_use]
    pub const fn kind(self) -> TitleKind {
        match self {
            Self::Movie(_) => TitleKind::Movie,
            Self::Show(_) => TitleKind::Show,
        }
    }
}

/// A piece of a title's metadata enrichment writes, which an administrator
/// can lock so enrichment leaves it as it is (issue #185). The external ids
/// are not among them: they are the match itself, which an administrator
/// changes by fixing the match, not by locking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetadataField {
    Title,
    OriginalTitle,
    Description,
    Year,
    /// A movie's release date. Shows have none.
    ReleaseDate,
    /// A movie's runtime. A show's runtime is its episodes'.
    Runtime,
    Poster,
    Backdrop,
    Rating,
    Genres,
}

impl MetadataField {
    /// Every field, in declaration order.
    pub const ALL: [MetadataField; 10] = [
        MetadataField::Title,
        MetadataField::OriginalTitle,
        MetadataField::Description,
        MetadataField::Year,
        MetadataField::ReleaseDate,
        MetadataField::Runtime,
        MetadataField::Poster,
        MetadataField::Backdrop,
        MetadataField::Rating,
        MetadataField::Genres,
    ];

    /// How the field is stored in `metadata_enrichment.locked_fields` (held
    /// to these by a `CHECK`) and spelled on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MetadataField::Title => "title",
            MetadataField::OriginalTitle => "original_title",
            MetadataField::Description => "description",
            MetadataField::Year => "year",
            MetadataField::ReleaseDate => "release_date",
            MetadataField::Runtime => "runtime",
            MetadataField::Poster => "poster",
            MetadataField::Backdrop => "backdrop",
            MetadataField::Rating => "rating",
            MetadataField::Genres => "genres",
        }
    }

    /// The field a stored name names.
    #[must_use]
    pub fn parse(stored: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.as_str() == stored)
    }

    /// Whether enrichment writes this field on a title of `kind` -- and so
    /// whether locking it there means anything.
    #[must_use]
    pub const fn applies_to(self, kind: TitleKind) -> bool {
        match self {
            MetadataField::ReleaseDate | MetadataField::Runtime => {
                matches!(kind, TitleKind::Movie)
            }
            MetadataField::Title
            | MetadataField::OriginalTitle
            | MetadataField::Description
            | MetadataField::Year
            | MetadataField::Poster
            | MetadataField::Backdrop
            | MetadataField::Rating
            | MetadataField::Genres => true,
        }
    }
}

/// The fields of one title enrichment must leave as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldLocks(BTreeSet<MetadataField>);

impl FieldLocks {
    /// No field locked: enrichment writes everything.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether enrichment must leave `field` alone.
    #[must_use]
    pub fn is_locked(&self, field: MetadataField) -> bool {
        self.0.contains(&field)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The locked fields, in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = MetadataField> + '_ {
        self.0.iter().copied()
    }

    /// The stored form: each field's name, in declaration order.
    #[must_use]
    pub fn to_stored(&self) -> Vec<String> {
        self.iter().map(|field| field.as_str().to_owned()).collect()
    }

    /// Reads the stored form back. A name no field has is skipped: the
    /// column's `CHECK` admits none, so one could only come from a newer
    /// build's migration, and ignoring its lock is the safe reading.
    #[must_use]
    pub fn from_stored(stored: &[String]) -> Self {
        stored
            .iter()
            .filter_map(|name| MetadataField::parse(name))
            .collect()
    }
}

impl FromIterator<MetadataField> for FieldLocks {
    fn from_iter<I: IntoIterator<Item = MetadataField>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnrichmentStatus {
    Pending,
    Enriched,
    Unmatched,
    Failed,
}

impl EnrichmentStatus {
    /// Every status, for exhaustive checks.
    pub const ALL: [EnrichmentStatus; 4] = [
        EnrichmentStatus::Pending,
        EnrichmentStatus::Enriched,
        EnrichmentStatus::Unmatched,
        EnrichmentStatus::Failed,
    ];
}

/// Row counts per [`EnrichmentStatus`], backing the admin status endpoint's
/// queue overview (issue #85).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnrichmentStatusCounts {
    pub pending: u64,
    pub enriched: u64,
    pub unmatched: u64,
    pub failed: u64,
}

/// Per-title enrichment queue/status row.
#[derive(Debug, Clone)]
pub struct EnrichmentState {
    pub id: Uuid,
    pub target: EnrichmentTargetId,
    pub status: EnrichmentStatus,
    pub attempts: u32,
    /// When this row becomes eligible for another attempt. `None` while
    /// `Pending` and never yet attempted.
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub enriched_at: Option<DateTime<Utc>>,
    pub match_confidence: Option<f32>,
    /// Canonical `"provider:id"` string, e.g. `"tmdb:603"`.
    pub matched_ref: Option<String>,
    pub force_refresh: bool,
    pub last_error: Option<String>,
    /// The fields enrichment leaves as they are (issue #185).
    pub locked_fields: FieldLocks,
    /// When the row last changed: queued, attempted, or locked.
    pub updated_at: DateTime<Utc>,
}

impl EnrichmentState {
    /// The position this row holds in the admin list.
    #[must_use]
    pub fn list_position(&self) -> EnrichmentListPosition {
        EnrichmentListPosition {
            updated_at: self.updated_at,
            id: self.id,
        }
    }
}

/// Which enrichment rows the admin list shows (FR-303).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnrichmentListFilter {
    /// Only rows in this status; every status when `None`.
    pub status: Option<EnrichmentStatus>,
    /// Only titles of this kind; both when `None`.
    pub kind: Option<TitleKind>,
}

impl EnrichmentListFilter {
    /// Whether `state` is one this filter lists.
    #[must_use]
    pub fn admits(&self, state: &EnrichmentState) -> bool {
        self.status.is_none_or(|status| status == state.status)
            && self.kind.is_none_or(|kind| kind == state.target.kind())
    }
}

/// Where a page of the admin list starts: after the row that changed at
/// `updated_at` with id `id`. The list runs newest change first, ties broken
/// by id, descending too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnrichmentListPosition {
    pub updated_at: DateTime<Utc>,
    pub id: Uuid,
}

/// One page of the admin list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnrichmentListQuery {
    pub filter: EnrichmentListFilter,
    /// Rows strictly after this position; from the newest when `None`.
    pub after: Option<EnrichmentListPosition>,
    /// At most this many rows.
    pub limit: NonZeroU32,
}

#[cfg(feature = "entity")]
impl From<beam_entity::metadata_enrichment::EnrichmentStatus> for EnrichmentStatus {
    fn from(status: beam_entity::metadata_enrichment::EnrichmentStatus) -> Self {
        use beam_entity::metadata_enrichment::EnrichmentStatus as DbStatus;
        match status {
            DbStatus::Pending => EnrichmentStatus::Pending,
            DbStatus::Enriched => EnrichmentStatus::Enriched,
            DbStatus::Unmatched => EnrichmentStatus::Unmatched,
            DbStatus::Failed => EnrichmentStatus::Failed,
        }
    }
}

#[cfg(feature = "entity")]
impl From<EnrichmentStatus> for beam_entity::metadata_enrichment::EnrichmentStatus {
    fn from(status: EnrichmentStatus) -> Self {
        use beam_entity::metadata_enrichment::EnrichmentStatus as DbStatus;
        match status {
            EnrichmentStatus::Pending => DbStatus::Pending,
            EnrichmentStatus::Enriched => DbStatus::Enriched,
            EnrichmentStatus::Unmatched => DbStatus::Unmatched,
            EnrichmentStatus::Failed => DbStatus::Failed,
        }
    }
}

#[cfg(feature = "entity")]
impl From<beam_entity::metadata_enrichment::Model> for EnrichmentState {
    fn from(model: beam_entity::metadata_enrichment::Model) -> Self {
        let target = match (model.movie_id, model.show_id) {
            (Some(id), None) => EnrichmentTargetId::Movie(id),
            (None, Some(id)) => EnrichmentTargetId::Show(id),
            _ => unreachable!(
                "metadata_enrichment rows always have exactly one of movie_id/show_id set"
            ),
        };
        Self {
            id: model.id,
            target,
            status: model.status.into(),
            attempts: model.attempts as u32,
            next_attempt_at: model.next_attempt_at.map(|d| d.with_timezone(&Utc)),
            enriched_at: model.enriched_at.map(|d| d.with_timezone(&Utc)),
            match_confidence: model.match_confidence,
            matched_ref: model.matched_ref,
            force_refresh: model.force_refresh,
            last_error: model.last_error,
            locked_fields: FieldLocks::from_stored(&model.locked_fields),
            updated_at: model.updated_at.with_timezone(&Utc),
        }
    }
}

#[cfg(test)]
mod field_tests {
    use super::*;

    #[test]
    fn every_field_reads_back_from_its_stored_form_and_nothing_else_does() {
        for field in MetadataField::ALL {
            assert_eq!(MetadataField::parse(field.as_str()), Some(field));
        }
        for stored in ["", "Title", "tmdb_id", "poster_url", "title "] {
            assert_eq!(MetadataField::parse(stored), None, "{stored:?}");
        }
    }

    #[test]
    fn locks_round_trip_through_their_stored_form_in_declaration_order() {
        let locks: FieldLocks = [MetadataField::Genres, MetadataField::Title]
            .into_iter()
            .collect();
        assert_eq!(locks.to_stored(), vec!["title", "genres"]);
        assert_eq!(FieldLocks::from_stored(&locks.to_stored()), locks);
    }

    #[test]
    fn a_filter_admits_exactly_the_rows_of_its_status_and_kind() {
        let row = |target, status| EnrichmentState {
            id: Uuid::new_v4(),
            target,
            status,
            attempts: 0,
            next_attempt_at: None,
            enriched_at: None,
            match_confidence: None,
            matched_ref: None,
            force_refresh: false,
            last_error: None,
            locked_fields: FieldLocks::none(),
            updated_at: Utc::now(),
        };
        let movie = row(
            EnrichmentTargetId::Movie(Uuid::new_v4()),
            EnrichmentStatus::Failed,
        );
        let show = row(
            EnrichmentTargetId::Show(Uuid::new_v4()),
            EnrichmentStatus::Unmatched,
        );
        let filter = |status, kind| EnrichmentListFilter { status, kind };

        assert!(filter(None, None).admits(&movie) && filter(None, None).admits(&show));
        assert!(filter(Some(EnrichmentStatus::Failed), None).admits(&movie));
        assert!(!filter(Some(EnrichmentStatus::Failed), None).admits(&show));
        assert!(filter(None, Some(TitleKind::Show)).admits(&show));
        assert!(!filter(None, Some(TitleKind::Show)).admits(&movie));
        assert!(!filter(Some(EnrichmentStatus::Unmatched), Some(TitleKind::Movie)).admits(&show));
    }
}

#[cfg(all(test, feature = "entity"))]
mod entity_conversion_tests {
    use super::*;

    fn model(
        movie_id: Option<Uuid>,
        show_id: Option<Uuid>,
    ) -> beam_entity::metadata_enrichment::Model {
        let now: chrono::DateTime<chrono::FixedOffset> = Utc::now().into();
        beam_entity::metadata_enrichment::Model {
            id: Uuid::new_v4(),
            movie_id,
            show_id,
            status: beam_entity::metadata_enrichment::EnrichmentStatus::Pending,
            attempts: 2,
            next_attempt_at: None,
            enriched_at: None,
            match_confidence: None,
            matched_ref: None,
            force_refresh: false,
            last_error: None,
            locked_fields: vec!["poster".to_owned(), "someday".to_owned()],
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_movie_row_becomes_a_movie_target() {
        let movie_id = Uuid::new_v4();
        let state = EnrichmentState::from(model(Some(movie_id), None));
        assert_eq!(state.target, EnrichmentTargetId::Movie(movie_id));
    }

    #[test]
    fn a_show_row_becomes_a_show_target() {
        // The two arms are near-identical and trivially transposable; getting
        // them the wrong way round would send every show through the movie
        // enrichment path.
        let show_id = Uuid::new_v4();
        let state = EnrichmentState::from(model(None, Some(show_id)));
        assert_eq!(state.target, EnrichmentTargetId::Show(show_id));
    }

    #[test]
    fn stored_locks_read_back_and_a_name_no_field_has_is_ignored() {
        let state = EnrichmentState::from(model(Some(Uuid::new_v4()), None));
        assert_eq!(
            state.locked_fields,
            [MetadataField::Poster].into_iter().collect::<FieldLocks>()
        );
    }

    #[test]
    #[should_panic(expected = "exactly one of movie_id/show_id")]
    fn a_row_targeting_neither_is_a_broken_invariant_not_a_silent_default() {
        // The schema's check constraint makes this unreachable; if it ever is
        // reached, the row is corrupt and guessing a target would enrich the
        // wrong title.
        let _ = EnrichmentState::from(model(None, None));
    }

    #[test]
    #[should_panic(expected = "exactly one of movie_id/show_id")]
    fn a_row_targeting_both_is_a_broken_invariant_too() {
        let _ = EnrichmentState::from(model(Some(Uuid::new_v4()), Some(Uuid::new_v4())));
    }
}
