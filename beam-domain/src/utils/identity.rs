//! A title's identity key: what the indexer matches a file to an existing
//! movie or show by (issue #183).
//!
//! The display title is not an identity. Enrichment rewrites it with the
//! provider's spelling -- `Amelie` becomes `Amélie`, `Spider Man` becomes
//! `Spider-Man` -- so a lookup by display title misses the enriched row on the
//! next file and creates a duplicate. The key is derived once, from the
//! filename parse, stored in its own column, and never touched by enrichment.
//!
//! The key is deliberately forgiving about spelling and strict about year:
//! case, accents, punctuation and `&`/`and` do not separate two titles, a
//! different release year does (`Dune (1984)` and `Dune (2021)` are two films).

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Separates the normalised title from the year inside a key. Never produced
/// by [`normalize_title`], which maps every non-alphanumeric character to a
/// space, so a key splits back into its two parts unambiguously.
const KEY_SEPARATOR: char = '|';

/// Fold a title into the form two spellings of one title share.
///
/// Compatibility-decomposes (NFKD), drops the combining marks that leaves
/// behind (`é` -> `e`), lowercases, spells `&` as `and`, turns every other
/// non-alphanumeric character into a space, and collapses runs of spaces.
/// Articles are kept: `The Thing` and `Thing` are different titles often
/// enough that folding them together would merge real films.
///
/// A title with no alphanumeric character at all (`!!!`) has nothing to fold
/// to, so it falls back to its trimmed lowercase form rather than to the empty
/// string -- two different punctuation-only titles stay two titles.
pub fn normalize_title(title: &str) -> String {
    let mut folded = String::with_capacity(title.len());
    for c in title.nfkd() {
        if is_combining_mark(c) {
            continue;
        }
        if c == '&' {
            folded.push_str(" and ");
            continue;
        }
        if c.is_alphanumeric() {
            folded.extend(c.to_lowercase());
        } else {
            folded.push(' ');
        }
    }
    let collapsed = folded.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        title
            .trim()
            .to_lowercase()
            .replace(KEY_SEPARATOR, " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        collapsed
    }
}

/// The identity key for a title parsed as `title` released in `year`:
/// `"{normalised title}|{year}"`, or `"{normalised title}|"` with no year.
pub fn title_identity_key(title: &str, year: Option<u32>) -> String {
    let normalized = normalize_title(title);
    match year {
        Some(year) => format!("{normalized}{KEY_SEPARATOR}{year}"),
        None => format!("{normalized}{KEY_SEPARATOR}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn spellings_of_one_title_share_a_key() {
        // Each row: two spellings a filename and a provider really disagree
        // on, which must land on one title.
        let same: &[(&str, &str, Option<u32>)] = &[
            ("Amélie", "Amelie", Some(2001)),
            ("Spider-Man", "Spider Man", Some(2002)),
            (
                "Spider-Man: Homecoming",
                "Spider Man Homecoming",
                Some(2017),
            ),
            ("Fast & Furious", "Fast and Furious", Some(2009)),
            ("THE MATRIX", "The Matrix", Some(1999)),
            (
                "Léon: The Professional",
                "Leon The Professional",
                Some(1994),
            ),
            ("Mr. Robot", "Mr Robot", None),
            ("  Arrival  ", "Arrival", Some(2016)),
        ];
        for (a, b, year) in same {
            assert_eq!(
                title_identity_key(a, *year),
                title_identity_key(b, *year),
                "{a:?} and {b:?} must be one title"
            );
        }
    }

    #[test]
    fn different_titles_keep_different_keys() {
        type Parse<'a> = (&'a str, Option<u32>);
        let different: &[(Parse, Parse)] = &[
            // A remake is a different film.
            (("Dune", Some(1984)), ("Dune", Some(2021))),
            // A yearless parse is not guessed into a year.
            (("Dune", None), ("Dune", Some(2021))),
            // Articles are kept.
            (("The Thing", Some(1982)), ("Thing", Some(1982))),
            // Two punctuation-only titles do not both fold to nothing.
            (("!!!", None), ("???", None)),
        ];
        for ((a, ay), (b, by)) in different {
            assert_ne!(
                title_identity_key(a, *ay),
                title_identity_key(b, *by),
                "{a:?} ({ay:?}) and {b:?} ({by:?}) must stay apart"
            );
        }
    }

    #[test]
    fn the_key_is_the_folded_title_and_the_year() {
        assert_eq!(title_identity_key("Amélie", Some(2001)), "amelie|2001");
        assert_eq!(title_identity_key("Mr. Robot", None), "mr robot|");
    }

    #[test]
    fn non_latin_titles_keep_their_letters() {
        // Folding strips marks, never whole scripts: a CJK title must not
        // collapse to the punctuation fallback and collide with another.
        assert_eq!(normalize_title("千と千尋の神隠し"), "千と千尋の神隠し");
        assert_ne!(
            title_identity_key("千と千尋の神隠し", Some(2001)),
            title_identity_key("もののけ姫", Some(2001))
        );
    }

    proptest! {
        #[test]
        fn normalising_never_panics_and_is_idempotent(title in "\\PC{0,40}") {
            let once = normalize_title(&title);
            prop_assert_eq!(normalize_title(&once), once);
        }

        #[test]
        fn the_folded_title_is_lowercase_with_no_separator_and_no_edge_space(
            title in "\\PC{0,40}",
        ) {
            let folded = normalize_title(&title);
            prop_assert!(!folded.contains(KEY_SEPARATOR));
            prop_assert!(!folded.contains("  "));
            prop_assert_eq!(folded.trim(), folded.as_str());
            // Lowercasing is a no-op on it. Not `!c.is_uppercase()`: symbols
            // such as `🅐` carry the Uppercase property but have no lowercase.
            prop_assert_eq!(folded.to_lowercase(), folded.clone());
        }

        #[test]
        fn case_and_separators_do_not_change_the_key(
            words in proptest::collection::vec("[a-zA-Z0-9]{1,8}", 1..5),
            separator in prop::sample::select(vec![" ", ".", "_", "-", ": ", " - "]),
            year in proptest::option::of(1900u32..2100),
        ) {
            let spaced = words.join(" ");
            let separated = words.join(separator).to_uppercase();
            prop_assert_eq!(
                title_identity_key(&spaced, year),
                title_identity_key(&separated, year)
            );
        }

        #[test]
        fn different_years_never_share_a_key(
            title in "\\PC{0,20}",
            a in 1900u32..2100,
            b in 1900u32..2100,
        ) {
            prop_assume!(a != b);
            prop_assert_ne!(title_identity_key(&title, Some(a)), title_identity_key(&title, Some(b)));
            prop_assert_ne!(title_identity_key(&title, Some(a)), title_identity_key(&title, None));
        }
    }
}
