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
//! case, punctuation, `&`/`and` and every combining mark in the Combining
//! Diacritical Marks block (U+0300-U+036F: the accents NFKD splits off Latin,
//! Greek and Cyrillic letters) do not separate two titles; a different
//! release year does (`Dune (1984)` and `Dune (2021)` are two films), and so
//! does every combining mark outside that block (`かぎ` and `かき`, `दिल` and
//! `दल` are different words).
//!
//! The fold is by block, not by language, so it also merges letters some
//! languages treat as distinct: Cyrillic `й`/`и`, `ї`/`і`, `ў`/`у`, and Latin
//! `ñ`/`n`, `ä`/`a`. `Мой` and `Мои` of one year share a key. That is the
//! accepted cost of `Amélie` and `Amelie` sharing one (decision D183-6 on
//! PR #214).

use std::ops::RangeInclusive;

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// The Combining Diacritical Marks block: the accents NFKD splits off Latin,
/// Greek and Cyrillic letters (`é` -> `e` + U+0301). These, and only these,
/// are folded away. Every other combining mark -- a kana voicing mark, an
/// Indic vowel sign, a Hebrew point -- distinguishes words in its script, so
/// dropping it would merge different titles. Some marks inside the block do
/// too (the breve of Cyrillic `й`); folding them is the cost the module docs
/// name.
const FOLDED_DIACRITICS: RangeInclusive<char> = '\u{0300}'..='\u{036F}';

/// Separates the normalised title from the year inside a key. Never produced
/// by [`normalize_title`], which maps every non-alphanumeric character to a
/// space, so a key splits back into its two parts unambiguously.
const KEY_SEPARATOR: char = '|';

/// Fold a title into the form two spellings of one title share.
///
/// Compatibility-decomposes (NFKD), drops every combining mark that leaves
/// behind in the Combining Diacritical Marks block -- Latin, Greek and
/// Cyrillic accents alike (`é` -> `e`, but also `й` -> `и`; see
/// [`FOLDED_DIACRITICS`] and the module docs for that accepted cost) -- while
/// keeping every other combining mark, lowercases, spells `&` as `and`, turns every other
/// non-alphanumeric character into a space, collapses runs of spaces, and
/// recomposes (NFC) what is left, so `が` stays `が` whether the filename
/// spelled it precomposed or decomposed. Articles are kept: `The Thing` and
/// `Thing` are different titles often enough that folding them together would
/// merge real films.
///
/// A title with no alphanumeric character at all (`!!!`) has nothing to fold
/// to, so it falls back to its trimmed lowercase form rather than to the empty
/// string -- two different punctuation-only titles stay two titles.
pub fn normalize_title(title: &str) -> String {
    let mut folded = String::with_capacity(title.len());
    for c in title.nfkd() {
        if FOLDED_DIACRITICS.contains(&c) {
            continue;
        }
        if c == '&' {
            folded.push_str(" and ");
            continue;
        }
        // A mark that survives the fold is part of its letter: kept, not
        // read as punctuation. Not all such marks are `Alphabetic` (U+3099,
        // the kana voicing mark, is not).
        if c.is_alphanumeric() || is_combining_mark(c) {
            folded.extend(c.to_lowercase());
        } else {
            folded.push(' ');
        }
    }
    let collapsed = folded
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .nfc()
        .collect::<String>();
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
            ("Ёлки", "Елки", Some(2010)),
            // A known, deliberate collision: the fold is by block, so `й`
            // folds to `и` although Russian reads them as different letters
            // (the accepted cost of `Amélie` = `Amelie`, D183-6 on PR #214).
            ("Мой", "Мои", Some(2010)),
            // One kana, precomposed and decomposed: the same word.
            ("\u{304C}\u{304E}", "\u{304B}\u{3099}\u{304D}\u{3099}", None),
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
            // A combining mark outside the folded block is kept: kana
            // voicing, an Indic vowel sign.
            (("かぎ", None), ("かき", None)),
            (("दिल", None), ("दल", None)),
            (("はは", None), ("ぱぱ", None)),
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

    /// Letters no documented fold touches, across scripts, including pairs
    /// that differ only by a combining mark in their own script (`き`/`ぎ`,
    /// `द`/`दि`) -- the pairs an over-eager mark fold merges.
    const LETTERS: &[&str] = &[
        "a", "e", "s", "7", "ß", "か", "き", "ぎ", "は", "ぱ", "द", "दि", "ल", "한", "ж",
    ];

    /// A title as words of [`LETTERS`] indices.
    fn words() -> impl Strategy<Value = Vec<Vec<usize>>> {
        proptest::collection::vec(proptest::collection::vec(0..LETTERS.len(), 1..4), 1..4)
    }

    /// `words` spelled the way a filename might: `separator` between words,
    /// ASCII letters optionally uppercased, and optionally an acute accent
    /// after every ASCII vowel -- every one a documented fold.
    fn spell(words: &[Vec<usize>], separator: &str, upper: bool, accent: bool) -> String {
        let mut spelled = Vec::new();
        for word in words {
            let mut out = String::new();
            for &letter in word {
                for c in LETTERS[letter].chars() {
                    out.push(if upper { c.to_ascii_uppercase() } else { c });
                    if accent && matches!(c, 'a' | 'e') {
                        out.push('\u{0301}');
                    }
                }
            }
            spelled.push(out);
        }
        spelled.join(separator)
    }

    proptest! {
        #[test]
        fn titles_whose_words_differ_never_share_a_key(
            a in words(),
            at_word in any::<prop::sample::Index>(),
            at_letter in any::<prop::sample::Index>(),
            replacement in 0..LETTERS.len(),
            separators in (
                prop::sample::select(vec![" ", ".", "_", "-", ": "]),
                prop::sample::select(vec![" ", ".", "_", "-", ": "]),
            ),
            upper in any::<(bool, bool)>(),
            accent in any::<(bool, bool)>(),
        ) {
            // `b` is `a` with one letter swapped, so the two are near misses
            // -- `かぎ` beside `かき` -- rather than unrelated strings.
            let mut b = a.clone();
            let word = at_word.index(b.len());
            let letter = at_letter.index(b[word].len());
            b[word][letter] = replacement;
            let plain_a = spell(&a, " ", false, false);
            let plain_b = spell(&b, " ", false, false);
            prop_assume!(plain_a != plain_b);

            let key_a = title_identity_key(&spell(&a, separators.0, upper.0, accent.0), None);
            let key_b = title_identity_key(&spell(&b, separators.1, upper.1, accent.1), None);
            prop_assert_ne!(
                &key_a, &key_b,
                "{:?} and {:?} are different words", plain_a, plain_b
            );
            // And the folds really are folds: each spelling keys as the plain one.
            prop_assert_eq!(key_a, title_identity_key(&plain_a, None));
        }

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
