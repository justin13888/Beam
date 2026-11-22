use std::path::PathBuf;

use proptest::prelude::*;

use super::super::beneath_pattern;

/// Whether `text` matches the SQL `LIKE` `pattern` whose escape is `\`:
/// `%` is any run of characters, `_` any one, and `\` makes the next
/// character literal. Written from the SQL standard's definition, not
/// from [`beneath_pattern`].
fn like(pattern: &str, text: &str) -> bool {
    enum Token {
        Any,
        One,
        Literal(char),
    }
    let mut tokens = Vec::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        tokens.push(match c {
            '%' => Token::Any,
            '_' => Token::One,
            '\\' => Token::Literal(chars.next().expect("an escape escapes something")),
            c => Token::Literal(c),
        });
    }
    let text: Vec<char> = text.chars().collect();
    // matches[i][j]: the first i tokens match the first j characters.
    let mut matches = vec![vec![false; text.len() + 1]; tokens.len() + 1];
    matches[0][0] = true;
    for (i, token) in tokens.iter().enumerate() {
        for j in 0..=text.len() {
            matches[i + 1][j] = match token {
                Token::Any => matches[i][j] || (j > 0 && matches[i + 1][j - 1]),
                Token::One => j > 0 && matches[i][j - 1],
                Token::Literal(c) => j > 0 && matches[i][j - 1] && text[j - 1] == *c,
            };
        }
    }
    matches[tokens.len()][text.len()]
}

/// An absolute path of 1 to 4 names drawn from characters that are
/// wildcards or the escape in `LIKE`, or that merely look alike.
fn path() -> impl Strategy<Value = PathBuf> {
    prop::collection::vec("[ab_%\\\\]{1,3}", 1..=4).prop_map(|names| {
        let mut path = PathBuf::from("/");
        path.extend(names);
        path
    })
}

proptest! {
    #[test]
    fn the_pattern_matches_only_strict_descendants(
        dir in path(),
        under in path(),
        trailing_separator in any::<bool>(),
    ) {
        // Half the paths share the directory as a prefix, so both
        // answers are exercised.
        let candidates = [dir.join(under.strip_prefix("/").unwrap()), under.clone(), dir.clone()];
        let shown = if trailing_separator {
            PathBuf::from(format!("{}/", dir.display()))
        } else {
            dir.clone()
        };
        let pattern = beneath_pattern(&shown);
        for path in candidates {
            let beneath = path.starts_with(&dir) && path != dir;
            prop_assert_eq!(
                like(&pattern, &path.to_string_lossy()),
                beneath,
                "{:?} against {:?} ({:?})", path, dir, pattern
            );
        }
    }
}
