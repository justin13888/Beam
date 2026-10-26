//! Who may see which titles (issue #263): the libraries an administrator
//! grants a user, and the highest content rating they may see.
//!
//! A title is the unit of access. A user sees a movie or show when one of the
//! libraries it was indexed in is one their policy admits, and its
//! certification is within their ceiling; every season, episode, file,
//! subtitle and piece of art of the title then follows it. A hidden title
//! answers as one that does not exist, so its existence does not leak.

use std::collections::BTreeSet;

use uuid::Uuid;

/// An age band certifications are grouped into, youngest first: what a
/// user's maximum rating is expressed in, since the provider's certifications
/// come from two US systems -- the MPA's film ratings and the TV Parental
/// Guidelines -- that a single ceiling has to compare across.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContentRating {
    /// `G`, `TV-Y`, `TV-G`.
    AllAges,
    /// `TV-Y7`, `TV-Y7-FV`: for children of seven and up.
    OlderChildren,
    /// `PG`, `TV-PG`.
    ParentalGuidance,
    /// `PG-13`, `TV-14`.
    Teen,
    /// `R`, `TV-MA`.
    Mature,
    /// `NC-17`.
    Adult,
}

/// Every certification Beam recognises, in its canonical spelling, and the
/// band it falls in. A certification that is not here -- `NR`, another
/// country's, anything a provider invents -- is treated as no rating at all.
pub const CERTIFICATIONS: [(&str, ContentRating); 12] = [
    ("G", ContentRating::AllAges),
    ("TV-Y", ContentRating::AllAges),
    ("TV-G", ContentRating::AllAges),
    ("TV-Y7", ContentRating::OlderChildren),
    ("TV-Y7-FV", ContentRating::OlderChildren),
    ("PG", ContentRating::ParentalGuidance),
    ("TV-PG", ContentRating::ParentalGuidance),
    ("PG-13", ContentRating::Teen),
    ("TV-14", ContentRating::Teen),
    ("R", ContentRating::Mature),
    ("TV-MA", ContentRating::Mature),
    ("NC-17", ContentRating::Adult),
];

/// The longest certification the store keeps (`*_content_rating_length`).
pub const MAX_CERTIFICATION_CHARS: usize = 32;

impl ContentRating {
    /// Every band, youngest first.
    pub const ALL: [Self; 6] = [
        Self::AllAges,
        Self::OlderChildren,
        Self::ParentalGuidance,
        Self::Teen,
        Self::Mature,
        Self::Adult,
    ];

    /// The spelling the store and the wire carry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AllAges => "all_ages",
            Self::OlderChildren => "older_children",
            Self::ParentalGuidance => "parental_guidance",
            Self::Teen => "teen",
            Self::Mature => "mature",
            Self::Adult => "adult",
        }
    }

    /// The inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|band| band.as_str() == value)
    }

    /// The band a stored certification falls in: `None` for one Beam does not
    /// recognise. Exact, on the canonical spelling [`normalize_certification`]
    /// stores, so that the store's `IN` over [`Self::certifications_within`]
    /// and this agree on every value.
    #[must_use]
    pub fn of_certification(certification: &str) -> Option<Self> {
        CERTIFICATIONS
            .iter()
            .find(|(label, _)| *label == certification)
            .map(|(_, band)| *band)
    }

    /// Every certification this band admits as a ceiling: its own and every
    /// younger band's.
    pub fn certifications_within(self) -> impl Iterator<Item = &'static str> {
        CERTIFICATIONS
            .iter()
            .filter(move |(_, band)| *band <= self)
            .map(|(label, _)| *label)
    }
}

/// A provider's certification as the store keeps it: trimmed, a recognised
/// one in its canonical spelling whatever its case, anything else as given.
/// `None` for an empty one, and for one longer than
/// [`MAX_CERTIFICATION_CHARS`], which no real certification is.
#[must_use]
pub fn normalize_certification(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_CERTIFICATION_CHARS {
        return None;
    }
    let canonical = CERTIFICATIONS
        .iter()
        .find(|(label, _)| label.eq_ignore_ascii_case(trimmed))
        .map_or(trimmed, |(label, _)| label);
    Some(canonical.to_string())
}

/// Which libraries a user may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryAccess {
    /// Every library, those created later included.
    All,
    /// Only these. Empty is a user who sees no library at all.
    Only(BTreeSet<Uuid>),
}

/// What an administrator restricted one user to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPolicy {
    pub libraries: LibraryAccess,
    /// The highest band the user may see; `None` for no ceiling. Under a
    /// ceiling a title with no recognised certification is hidden: an unrated
    /// title is not known to be within it.
    pub max_rating: Option<ContentRating>,
}

impl Default for AccessPolicy {
    fn default() -> Self {
        Self::unrestricted()
    }
}

impl AccessPolicy {
    /// Every library, no ceiling: a user no administrator restricted.
    #[must_use]
    pub const fn unrestricted() -> Self {
        Self {
            libraries: LibraryAccess::All,
            max_rating: None,
        }
    }

    /// Whether the policy admits every title, so a check need read nothing.
    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        matches!(self.libraries, LibraryAccess::All) && self.max_rating.is_none()
    }

    /// Whether the user may see the library `library_id`.
    #[must_use]
    pub fn admits_library(&self, library_id: Uuid) -> bool {
        match &self.libraries {
            LibraryAccess::All => true,
            LibraryAccess::Only(granted) => granted.contains(&library_id),
        }
    }

    /// Whether a title certified `certification` is within the ceiling.
    #[must_use]
    pub fn admits_certification(&self, certification: Option<&str>) -> bool {
        match self.max_rating {
            None => true,
            Some(ceiling) => certification
                .and_then(ContentRating::of_certification)
                .is_some_and(|band| band <= ceiling),
        }
    }

    /// Whether the user may see a title indexed in `libraries` and certified
    /// `certification`. A title indexed in no library is admitted only by a
    /// policy covering every library.
    #[must_use]
    pub fn admits_title(&self, libraries: &BTreeSet<Uuid>, certification: Option<&str>) -> bool {
        let in_library = match &self.libraries {
            LibraryAccess::All => true,
            LibraryAccess::Only(granted) => !granted.is_disjoint(libraries),
        };
        in_library && self.admits_certification(certification)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn library(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn only(ids: &[u128]) -> LibraryAccess {
        LibraryAccess::Only(ids.iter().copied().map(library).collect())
    }

    #[test]
    fn each_band_round_trips_through_its_stored_spelling() {
        for band in ContentRating::ALL {
            assert_eq!(ContentRating::parse(band.as_str()), Some(band));
        }
        assert_eq!(ContentRating::parse("PG-13"), None);
    }

    #[test]
    fn the_bands_run_youngest_first() {
        let mut sorted = ContentRating::ALL;
        sorted.sort();
        assert_eq!(sorted, ContentRating::ALL);
    }

    /// A film rating and a TV rating meant for the same audience share a
    /// band, so one ceiling compares across both systems.
    #[test]
    fn film_and_tv_certifications_for_one_audience_share_a_band() {
        for (film, tv) in [
            ("G", "TV-G"),
            ("PG", "TV-PG"),
            ("PG-13", "TV-14"),
            ("R", "TV-MA"),
        ] {
            assert_eq!(
                ContentRating::of_certification(film),
                ContentRating::of_certification(tv),
                "{film} and {tv}"
            );
        }
        assert!(
            ContentRating::of_certification("NC-17") > ContentRating::of_certification("TV-MA")
        );
        assert!(ContentRating::of_certification("TV-Y7") < ContentRating::of_certification("PG"));
    }

    #[test]
    fn an_unrecognised_or_non_canonical_certification_has_no_band() {
        for certification in ["NR", "Unrated", "12A", "pg-13", " PG", ""] {
            assert_eq!(
                ContentRating::of_certification(certification),
                None,
                "{certification:?}"
            );
        }
    }

    #[test]
    fn normalizing_trims_and_spells_a_recognised_certification_canonically() {
        assert_eq!(
            normalize_certification("  pg-13 "),
            Some("PG-13".to_string())
        );
        assert_eq!(normalize_certification("tv-ma"), Some("TV-MA".to_string()));
        assert_eq!(normalize_certification(" 12A "), Some("12A".to_string()));
        assert_eq!(normalize_certification("   "), None);
        let long = "x".repeat(MAX_CERTIFICATION_CHARS + 1);
        assert_eq!(normalize_certification(&long), None);
        let longest = "x".repeat(MAX_CERTIFICATION_CHARS);
        assert_eq!(normalize_certification(&longest), Some(longest));
    }

    #[test]
    fn the_unrestricted_policy_admits_every_title_and_reads_nothing() {
        let policy = AccessPolicy::unrestricted();
        assert!(policy.is_unrestricted());
        assert!(policy.admits_title(&BTreeSet::new(), None));
        assert!(policy.admits_library(library(9)));
        assert_eq!(AccessPolicy::default(), policy);
    }

    #[test]
    fn either_restriction_makes_a_policy_restricted() {
        let granted = AccessPolicy {
            libraries: only(&[]),
            max_rating: None,
        };
        let ceiling = AccessPolicy {
            libraries: LibraryAccess::All,
            max_rating: Some(ContentRating::Adult),
        };
        assert!(!granted.is_unrestricted());
        assert!(!ceiling.is_unrestricted());
    }

    #[test]
    fn a_grant_admits_a_title_in_any_granted_library_and_no_other() {
        let policy = AccessPolicy {
            libraries: only(&[1, 2]),
            max_rating: None,
        };
        let held_in = |ids: &[u128]| ids.iter().copied().map(library).collect::<BTreeSet<_>>();
        assert!(policy.admits_title(&held_in(&[2, 3]), None));
        assert!(!policy.admits_title(&held_in(&[3]), None));
        assert!(!policy.admits_title(&held_in(&[]), None));
        assert!(policy.admits_library(library(1)));
        assert!(!policy.admits_library(library(3)));
    }

    #[test]
    fn a_ceiling_admits_its_band_and_younger_and_hides_the_unrated() {
        let policy = AccessPolicy {
            libraries: LibraryAccess::All,
            max_rating: Some(ContentRating::Teen),
        };
        assert!(policy.admits_certification(Some("PG-13")));
        assert!(policy.admits_certification(Some("TV-Y")));
        assert!(!policy.admits_certification(Some("R")));
        assert!(!policy.admits_certification(Some("NR")));
        assert!(!policy.admits_certification(None));
    }

    proptest! {
        /// The store filters a ceiling by `IN` over the certifications it
        /// admits; that list and the per-title check must agree on every
        /// recognised certification, or browse and detail would disagree.
        #[test]
        fn the_admitted_certifications_are_exactly_those_the_check_admits(
            ceiling in prop::sample::select(ContentRating::ALL.to_vec()),
        ) {
            let policy = AccessPolicy {
                libraries: LibraryAccess::All,
                max_rating: Some(ceiling),
            };
            let within: BTreeSet<&str> = ceiling.certifications_within().collect();
            for (certification, _) in CERTIFICATIONS {
                prop_assert_eq!(
                    within.contains(certification),
                    policy.admits_certification(Some(certification))
                );
            }
        }

        /// Whatever a provider sends, normalizing it gives a certification
        /// the store accepts, and normalizing that again changes nothing.
        #[test]
        fn normalizing_is_bounded_and_idempotent(raw in ".{0,40}") {
            if let Some(normalized) = normalize_certification(&raw) {
                prop_assert!(!normalized.is_empty());
                prop_assert!(normalized.chars().count() <= MAX_CERTIFICATION_CHARS);
                prop_assert_eq!(normalize_certification(&normalized), Some(normalized.clone()));
            }
        }
    }
}
