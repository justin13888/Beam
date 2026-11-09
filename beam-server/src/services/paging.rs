//! The page size every `first`/`last`-bounded list holds its request to.

use std::num::NonZeroU32;

/// `first`, defaulted to `default` and held to `1..=max`.
///
/// # Errors
///
/// A size outside `1..=max`, as the 400 detail naming the bound and the
/// `noun` the list holds: `a page holds 1 to {max} {noun}, not {size}`.
pub fn page_size(
    first: Option<u32>,
    default: u32,
    max: u32,
    noun: &str,
) -> Result<NonZeroU32, String> {
    let size = first.unwrap_or(default);
    NonZeroU32::new(size)
        .filter(|size| size.get() <= max)
        .ok_or_else(|| format!("a page holds 1 to {max} {noun}, not {size}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_bounds_are_inclusive_and_nothing_past_them_is_admitted() {
        let (default, max) = (20, 100);
        for (first, expected) in [
            (None, Ok(default)),
            (Some(0), Err(0)),
            (Some(1), Ok(1)),
            (Some(max), Ok(max)),
            (Some(max + 1), Err(max + 1)),
            (Some(u32::MAX), Err(u32::MAX)),
        ] {
            let size = page_size(first, default, max, "people");
            match expected {
                Ok(expected) => assert_eq!(size.map(NonZeroU32::get), Ok(expected), "{first:?}"),
                Err(refused) => assert_eq!(
                    size,
                    Err(format!("a page holds 1 to {max} people, not {refused}")),
                    "{first:?}"
                ),
            }
        }
    }

    #[test]
    fn a_default_outside_the_bounds_is_refused_like_a_request() {
        assert_eq!(
            page_size(None, 0, 50, "items"),
            Err("a page holds 1 to 50 items, not 0".to_owned())
        );
    }

    proptest! {
        /// A size is admitted exactly when it is in `1..=max`, and then
        /// unchanged; one refused names itself, the bound and the noun.
        #[test]
        fn a_size_is_admitted_unchanged_or_refused_by_name(first in any::<u32>(), max in 1u32..1_000) {
            match page_size(Some(first), 1, max, "titles") {
                Ok(size) => {
                    prop_assert!((1..=max).contains(&first));
                    prop_assert_eq!(size.get(), first);
                }
                Err(detail) => {
                    prop_assert!(!(1..=max).contains(&first));
                    prop_assert_eq!(detail, format!("a page holds 1 to {max} titles, not {first}"));
                }
            }
        }
    }
}
