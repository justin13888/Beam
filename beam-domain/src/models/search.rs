//! Pure, provider-agnostic helpers shared by the movie/show search repository
//! methods. `Sql*Repository` implementations use real Postgres `pg_trgm`
//! similarity server-side; `InMemory*Repository` fakes use
//! [`title_match_score`] as an offline stand-in so unit tests never need a
//! running database.

/// A rough, offline substitute for Postgres trigram similarity: scores how
/// well `title` matches `query` so the in-memory repository fakes can rank
/// results the same *shape* of way the SQL implementations do (exact >
/// starts-with > contains > no match), without needing pg_trgm itself.
///
/// Returns `0.0` for no match (the caller should exclude these), and a value
/// in `(0.0, 1.0]` otherwise -- higher is a better match.
pub fn title_match_score(title: &str, query: &str) -> f64 {
    let title_lower = title.to_lowercase();
    let query_lower = query.to_lowercase();
    if query_lower.is_empty() {
        return 1.0;
    }
    if title_lower == query_lower {
        3.0
    } else if title_lower.starts_with(&query_lower) {
        2.0
    } else if title_lower.contains(&query_lower) {
        1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_scores_highest() {
        assert_eq!(title_match_score("The Matrix", "the matrix"), 3.0);
    }

    #[test]
    fn prefix_match_scores_above_contains() {
        let prefix = title_match_score("The Matrix Reloaded", "the matrix");
        let contains = title_match_score("The Matrix Reloaded", "matrix reloaded");
        assert!(prefix < 3.0);
        assert!(contains > 0.0);
        assert!(prefix > contains || (prefix - contains).abs() < f64::EPSILON);
    }

    #[test]
    fn no_match_scores_zero() {
        assert_eq!(title_match_score("Inception", "matrix"), 0.0);
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(title_match_score("Anything", ""), 1.0);
    }
}
