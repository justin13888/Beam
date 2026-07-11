//! Pure, offline scoring of enrichment-provider search results against a
//! parsed local title. No I/O -- fully unit-testable without a provider.

use beam_domain::providers::enrichment::{MovieSearchHit, ShowSearchHit};

/// A hit is accepted only when it clears both bars: a low total score can
/// still slip through on a strong year match alone otherwise. The total-score
/// bar is caller-supplied (operator-tunable via `BEAM_ENRICH_MIN_CONFIDENCE`);
/// [`DEFAULT_MIN_CONFIDENCE`] is the historical hardcoded value and the config
/// default. The title bar stays a fixed structural floor.
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.70;
const ACCEPT_TITLE_THRESHOLD: f64 = 0.55;

const TITLE_WEIGHT: f64 = 0.70;

fn normalize(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut last_was_space = true;
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            last_was_space = false;
        } else if !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
    }
    out.trim_end().to_string()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// Normalized string similarity in `[0.0, 1.0]`; `1.0` is an exact match
/// (after normalization).
fn string_similarity(a: &str, b: &str) -> f64 {
    let a = normalize(a);
    let b = normalize(b);
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein(&a, &b) as f64 / max_len as f64)
}

/// Best similarity against either the primary or original-language title.
fn title_similarity(
    query_title: &str,
    candidate_title: &str,
    candidate_original: Option<&str>,
) -> f64 {
    let primary = string_similarity(query_title, candidate_title);
    match candidate_original {
        Some(original) => primary.max(string_similarity(query_title, original)),
        None => primary,
    }
}

fn year_bonus(query_year: Option<u32>, candidate_year: Option<u32>) -> f64 {
    match (query_year, candidate_year) {
        (Some(q), Some(c)) => match (q as i64 - c as i64).abs() {
            0 => 0.30,
            1 => 0.15,
            _ => -0.20,
        },
        _ => 0.0,
    }
}

fn is_exact_title(
    query_title: &str,
    candidate_title: &str,
    candidate_original: Option<&str>,
) -> bool {
    let query = normalize(query_title);
    normalize(candidate_title) == query || candidate_original.is_some_and(|o| normalize(o) == query)
}

#[derive(Debug, Clone, Copy)]
pub struct MatchScore {
    pub title_score: f64,
    pub total_score: f64,
}

impl MatchScore {
    fn accepted(&self, min_confidence: f64) -> bool {
        self.total_score >= min_confidence && self.title_score >= ACCEPT_TITLE_THRESHOLD
    }
}

fn score(
    query_title: &str,
    query_year: Option<u32>,
    candidate_title: &str,
    candidate_original: Option<&str>,
    candidate_year: Option<u32>,
) -> MatchScore {
    let title_score = title_similarity(query_title, candidate_title, candidate_original);
    let total_score = title_score * TITLE_WEIGHT + year_bonus(query_year, candidate_year);
    MatchScore {
        title_score,
        total_score,
    }
}

/// Picks the best-scoring hit that clears both accept thresholds, breaking
/// ties by: score, then exact title match, then exact year, then popularity,
/// then provider list order (earlier providers are queried first by the
/// caller, and `hits` preserves that order across providers).
pub fn best_movie_match<'a>(
    query_title: &str,
    query_year: Option<u32>,
    hits: &'a [MovieSearchHit],
    min_confidence: f64,
) -> Option<(&'a MovieSearchHit, MatchScore)> {
    hits.iter()
        .map(|hit| {
            (
                hit,
                score(
                    query_title,
                    query_year,
                    &hit.title,
                    hit.original_title.as_deref(),
                    hit.year,
                ),
            )
        })
        .filter(|(_, s)| s.accepted(min_confidence))
        .max_by(|(a_hit, a_score), (b_hit, b_score)| {
            a_score
                .total_score
                .partial_cmp(&b_score.total_score)
                .unwrap()
                .then_with(|| {
                    let a_exact =
                        is_exact_title(query_title, &a_hit.title, a_hit.original_title.as_deref());
                    let b_exact =
                        is_exact_title(query_title, &b_hit.title, b_hit.original_title.as_deref());
                    a_exact.cmp(&b_exact)
                })
                .then_with(|| {
                    let a_year_exact = query_year.is_some() && a_hit.year == query_year;
                    let b_year_exact = query_year.is_some() && b_hit.year == query_year;
                    a_year_exact.cmp(&b_year_exact)
                })
                .then_with(|| {
                    a_hit
                        .popularity
                        .unwrap_or(0.0)
                        .partial_cmp(&b_hit.popularity.unwrap_or(0.0))
                        .unwrap()
                })
        })
}

/// Same rules as [`best_movie_match`], for shows.
pub fn best_show_match<'a>(
    query_title: &str,
    query_year: Option<u32>,
    hits: &'a [ShowSearchHit],
    min_confidence: f64,
) -> Option<(&'a ShowSearchHit, MatchScore)> {
    hits.iter()
        .map(|hit| {
            (
                hit,
                score(
                    query_title,
                    query_year,
                    &hit.title,
                    hit.original_title.as_deref(),
                    hit.year,
                ),
            )
        })
        .filter(|(_, s)| s.accepted(min_confidence))
        .max_by(|(a_hit, a_score), (b_hit, b_score)| {
            a_score
                .total_score
                .partial_cmp(&b_score.total_score)
                .unwrap()
                .then_with(|| {
                    let a_exact =
                        is_exact_title(query_title, &a_hit.title, a_hit.original_title.as_deref());
                    let b_exact =
                        is_exact_title(query_title, &b_hit.title, b_hit.original_title.as_deref());
                    a_exact.cmp(&b_exact)
                })
                .then_with(|| {
                    let a_year_exact = query_year.is_some() && a_hit.year == query_year;
                    let b_year_exact = query_year.is_some() && b_hit.year == query_year;
                    a_year_exact.cmp(&b_year_exact)
                })
                .then_with(|| {
                    a_hit
                        .popularity
                        .unwrap_or(0.0)
                        .partial_cmp(&b_hit.popularity.unwrap_or(0.0))
                        .unwrap()
                })
        })
}

/// Top-N candidates by total score, for logging when nothing clears the
/// accept threshold (admins can see what almost matched).
pub fn top_movie_candidates(
    query_title: &str,
    query_year: Option<u32>,
    hits: &[MovieSearchHit],
    n: usize,
) -> Vec<(String, MatchScore)> {
    let mut scored: Vec<(String, MatchScore)> = hits
        .iter()
        .map(|hit| {
            (
                format!("{} ({:?}) [{}]", hit.title, hit.year, hit.external_ref),
                score(
                    query_title,
                    query_year,
                    &hit.title,
                    hit.original_title.as_deref(),
                    hit.year,
                ),
            )
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_score.partial_cmp(&a.1.total_score).unwrap());
    scored.truncate(n);
    scored
}

/// Top-N candidates by total score, for logging when nothing clears the
/// accept threshold.
pub fn top_show_candidates(
    query_title: &str,
    query_year: Option<u32>,
    hits: &[ShowSearchHit],
    n: usize,
) -> Vec<(String, MatchScore)> {
    let mut scored: Vec<(String, MatchScore)> = hits
        .iter()
        .map(|hit| {
            (
                format!("{} ({:?}) [{}]", hit.title, hit.year, hit.external_ref),
                score(
                    query_title,
                    query_year,
                    &hit.title,
                    hit.original_title.as_deref(),
                    hit.year,
                ),
            )
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_score.partial_cmp(&a.1.total_score).unwrap());
    scored.truncate(n);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use beam_domain::providers::enrichment::ExternalMediaRef;

    fn hit(title: &str, year: Option<u32>) -> MovieSearchHit {
        MovieSearchHit {
            external_ref: ExternalMediaRef::new("tmdb", "1"),
            title: title.to_string(),
            original_title: None,
            year,
            popularity: None,
            vote_average: None,
        }
    }

    fn show_hit(title: &str, year: Option<u32>) -> ShowSearchHit {
        ShowSearchHit {
            external_ref: ExternalMediaRef::new("tmdb", "1"),
            title: title.to_string(),
            original_title: None,
            year,
            popularity: None,
            vote_average: None,
        }
    }

    #[test]
    fn exact_title_and_year_accepted() {
        let hits = vec![hit("Blade Runner 2049", Some(2017))];
        let result = best_movie_match(
            "Blade Runner 2049",
            Some(2017),
            &hits,
            DEFAULT_MIN_CONFIDENCE,
        );
        assert!(result.is_some());
        let (_, score) = result.unwrap();
        assert!(score.total_score > 0.9);
    }

    #[test]
    fn exact_title_no_year_query_still_accepted() {
        let hits = vec![hit("The Matrix", Some(1999))];
        let result = best_movie_match("The Matrix", None, &hits, DEFAULT_MIN_CONFIDENCE);
        assert!(result.is_some());
    }

    #[test]
    fn wrong_year_by_one_still_accepted_with_lower_score() {
        let hits = vec![hit("Dune", Some(2021))];
        let result = best_movie_match("Dune", Some(2020), &hits, DEFAULT_MIN_CONFIDENCE);
        assert!(result.is_some());
    }

    #[test]
    fn wildly_different_title_rejected() {
        let hits = vec![hit("Completely Unrelated Film", Some(2017))];
        let result = best_movie_match(
            "Blade Runner 2049",
            Some(2017),
            &hits,
            DEFAULT_MIN_CONFIDENCE,
        );
        assert!(result.is_none());
    }

    #[test]
    fn year_off_by_a_lot_can_reject_marginal_title() {
        // Similar-ish title but not exact, with a big year mismatch: the
        // year penalty should be enough to push a marginal title below the
        // total-score bar even though the title alone might not clear 0.55.
        let hits = vec![hit("The Matrix Reloaded", Some(1975))];
        let result = best_movie_match("The Matrix", Some(2003), &hits, DEFAULT_MIN_CONFIDENCE);
        assert!(result.is_none());
    }

    #[test]
    fn min_confidence_threshold_is_respected() {
        // An exact title with no year query scores exactly the title weight
        // (0.70): it clears a lenient 0.5 bar but not a strict 0.9 one, so the
        // same candidate flips from accepted to rejected purely on the knob.
        let hits = vec![hit("The Matrix", None)];
        let (_, score) = best_movie_match("The Matrix", None, &hits, 0.5)
            .expect("should match at a lenient threshold");
        assert!((score.total_score - 0.70).abs() < 1e-9);
        assert!(
            best_movie_match("The Matrix", None, &hits, 0.9).is_none(),
            "the same candidate must be rejected once the bar is raised above its score"
        );
    }

    #[test]
    fn empty_hits_returns_none() {
        assert!(best_movie_match("Anything", None, &[], DEFAULT_MIN_CONFIDENCE).is_none());
    }

    #[test]
    fn prefers_exact_title_match_over_higher_year_bonus_tie() {
        let hits = vec![
            hit("The Office (US)", Some(2005)),
            hit("The Office", Some(2005)),
        ];
        let result = best_show_match_helper("The Office", Some(2005), &hits);
        assert_eq!(result.unwrap().0.title, "The Office");
    }

    fn best_show_match_helper<'a>(
        title: &str,
        year: Option<u32>,
        hits: &'a [MovieSearchHit],
    ) -> Option<(&'a MovieSearchHit, MatchScore)> {
        best_movie_match(title, year, hits, DEFAULT_MIN_CONFIDENCE)
    }

    #[test]
    fn show_matching_uses_same_rules() {
        let hits = vec![show_hit("Arcane", Some(2021))];
        let result = best_show_match("Arcane", Some(2021), &hits, DEFAULT_MIN_CONFIDENCE);
        assert!(result.is_some());
    }

    #[test]
    fn original_title_considered() {
        let mut candidate = hit("Your Name", Some(2016));
        candidate.title = "Kimi no Na wa.".to_string();
        candidate.original_title = Some("Your Name".to_string());
        let hits = [candidate];
        let result = best_movie_match("Your Name", Some(2016), &hits, DEFAULT_MIN_CONFIDENCE);
        assert!(result.is_some());
    }

    #[test]
    fn punctuation_and_case_ignored() {
        let hits = vec![hit("Se7en", Some(1995))];
        let result = best_movie_match("se7en", Some(1995), &hits, DEFAULT_MIN_CONFIDENCE);
        assert!(result.is_some());
    }

    #[test]
    fn top_candidates_sorted_by_score_descending() {
        let hits = vec![
            hit("Totally Different", Some(1980)),
            hit("Dune", Some(2021)),
            hit("Dune Part Two", Some(2024)),
        ];
        let top = top_movie_candidates("Dune", Some(2021), &hits, 2);
        assert_eq!(top.len(), 2);
        assert!(top[0].1.total_score >= top[1].1.total_score);
    }

    #[test]
    fn levenshtein_distance_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("same", "same"), 0);
    }

    #[test]
    fn top_show_candidates_sorted() {
        let hits = vec![show_hit("Nope", Some(1980)), show_hit("Arcane", Some(2021))];
        let top = top_show_candidates("Arcane", Some(2021), &hits, 5);
        assert_eq!(top.len(), 2);
        assert!(top[0].1.total_score >= top[1].1.total_score);
    }
}
