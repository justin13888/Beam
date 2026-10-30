use uuid::Uuid;

/// A genre, shared across movies and shows via junction tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Genre {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[cfg(feature = "entity")]
impl From<beam_entity::genre::Model> for Genre {
    fn from(model: beam_entity::genre::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            slug: model.slug,
        }
    }
}

/// The order a title's genres are listed in: case-insensitively by name, then
/// by the name itself so the order is total. Done here rather than by a SQL
/// `ORDER BY`, whose result depends on the database's collation.
pub fn sort_genre_names(names: &mut [String]) {
    names.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
}

/// Normalizes a genre name into a URL-safe slug (lowercase, non-alphanumeric
/// runs collapsed to a single `-`, no leading/trailing `-`).
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_was_sep = true; // avoid a leading '-'
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            slug.push('-');
            last_was_sep = true;
        }
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Science Fiction"), "science-fiction");
    }

    #[test]
    fn slugify_collapses_punctuation() {
        assert_eq!(slugify("Action & Adventure!!"), "action-adventure");
    }

    #[test]
    fn slugify_no_leading_trailing_dash() {
        assert_eq!(slugify("  Drama  "), "drama");
    }
}

#[cfg(test)]
mod slugify_properties {
    use super::*;
    use proptest::prelude::*;

    // A slug goes into a URL, so its shape is a contract with every client
    // that builds one. These are the invariants that hold for any input --
    // the table-driven cases above only pin three realistic genre names.
    proptest! {
        #[test]
        fn a_slug_is_lowercase_alphanumeric_and_dashes(name in ".*") {
            let slug = slugify(&name);
            prop_assert!(
                slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "slug {slug:?} from {name:?} contains a character that is not URL-safe"
            );
        }

        #[test]
        fn a_slug_never_starts_or_ends_with_a_separator(name in ".*") {
            let slug = slugify(&name);
            prop_assert!(!slug.starts_with('-'), "leading dash in {slug:?}");
            prop_assert!(!slug.ends_with('-'), "trailing dash in {slug:?}");
        }

        #[test]
        fn slugifying_a_slug_changes_nothing(name in ".*") {
            let slug = slugify(&name);
            prop_assert_eq!(slugify(&slug), slug.clone());
        }

        #[test]
        fn any_alphanumeric_input_produces_a_non_empty_slug(
            name in "[A-Za-z0-9][A-Za-z0-9 &!?-]{0,40}"
        ) {
            prop_assert!(
                !slugify(&name).is_empty(),
                "{name:?} has alphanumerics but slugified to nothing"
            );
        }

        #[test]
        fn slugs_never_contain_a_run_of_separators(name in ".*") {
            prop_assert!(
                !slugify(&name).contains("--"),
                "consecutive separators in the slug of {name:?}"
            );
        }
    }
}
