//! Pure, offline scoring of enrichment-provider search results against a
//! parsed local title, and of where an absolutely numbered episode sits in
//! a provider's seasons. No I/O -- fully unit-testable without a provider.

use beam_domain::providers::enrichment::{
    EpisodeEnrichment, MovieSearchHit, SeasonEnrichment, ShowSearchHit,
};

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchScore {
    pub title_score: f64,
    pub total_score: f64,
}

impl MatchScore {
    fn accepted(&self, min_confidence: f64) -> bool {
        self.total_score >= min_confidence && self.title_score >= ACCEPT_TITLE_THRESHOLD
    }
}

/// How well a candidate titled `candidate_title` (or `candidate_original`),
/// released in `candidate_year`, matches the query: the score every ranking
/// here, and an administrator's candidate list, orders by.
pub fn score(
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

/// A provider's popularity as the ranking reads it: a missing or non-finite
/// one (`NaN`, an infinity) counts as no popularity at all, so a malformed
/// figure neither panics the ranking nor outranks every real one.
fn ranked_popularity(popularity: Option<f64>) -> f64 {
    popularity.filter(|value| value.is_finite()).unwrap_or(0.0)
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
        // `max_by` answers the *last* of equal maxima; walking the hits
        // backwards makes that the earliest, so a full tie goes to the
        // earlier provider.
        .rev()
        .max_by(|(a_hit, a_score), (b_hit, b_score)| {
            a_score
                .total_score
                .total_cmp(&b_score.total_score)
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
                    ranked_popularity(a_hit.popularity)
                        .total_cmp(&ranked_popularity(b_hit.popularity))
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
        // `max_by` answers the *last* of equal maxima; walking the hits
        // backwards makes that the earliest, so a full tie goes to the
        // earlier provider.
        .rev()
        .max_by(|(a_hit, a_score), (b_hit, b_score)| {
            a_score
                .total_score
                .total_cmp(&b_score.total_score)
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
                    ranked_popularity(a_hit.popularity)
                        .total_cmp(&ranked_popularity(b_hit.popularity))
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
    scored.sort_by(|a, b| b.1.total_score.total_cmp(&a.1.total_score));
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
    scored.sort_by(|a, b| b.1.total_score.total_cmp(&a.1.total_score));
    scored.truncate(n);
    scored
}

/// The season a provider files its specials under. Specials sit outside
/// the absolute order: a running number counts the regular episodes only.
pub const SPECIALS_SEASON: u32 = 0;

/// The episode a provider places at `absolute` in its absolute order, and
/// the season it is in (issue #267): what an absolutely numbered file
/// (`Show - 030`) is, in that provider's seasons.
///
/// `seasons` are the provider's seasons, in any order. A provider that
/// numbers its episodes absolutely itself (TheTVDB) is read by those
/// numbers, and only by them: an episode it gives no number is not in its
/// order. Otherwise the order is the regular seasons one after another, by
/// season then episode number. Either way the specials season is left out,
/// and nothing is at `0`.
pub fn absolute_episode(
    seasons: &[SeasonEnrichment],
    absolute: u32,
) -> Option<(u32, &EpisodeEnrichment)> {
    if absolute == 0 {
        return None;
    }
    let mut regular: Vec<&SeasonEnrichment> = seasons
        .iter()
        .filter(|season| season.season_number != SPECIALS_SEASON)
        .collect();
    regular.sort_by_key(|season| season.season_number);
    let mut order = regular.into_iter().flat_map(|season| {
        let mut episodes: Vec<&EpisodeEnrichment> = season.episodes.iter().collect();
        episodes.sort_by_key(|episode| episode.episode_number);
        episodes
            .into_iter()
            .map(move |episode| (season.season_number, episode))
    });
    let numbered = seasons
        .iter()
        .filter(|season| season.season_number != SPECIALS_SEASON)
        .flat_map(|season| &season.episodes)
        .any(|episode| episode.absolute_number.is_some());
    if numbered {
        order.find(|(_, episode)| episode.absolute_number == Some(absolute))
    } else {
        order.nth((absolute - 1) as usize)
    }
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
        let (_, score) = result.expect("an exact title and year is accepted");
        assert_eq!(score.title_score, 1.0);
        assert!(
            (score.total_score - 1.0).abs() < 1e-9,
            "the whole title weight and the whole year bonus: {score:?}"
        );
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
        let (_, score) = best_movie_match("Dune", Some(2020), &hits, DEFAULT_MIN_CONFIDENCE)
            .expect("a year off by one is accepted");
        assert!(
            (score.total_score - 0.85).abs() < 1e-9,
            "half the year bonus: {score:?}"
        );
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

    /// A real tie on the total: an exact title with no year (0.7 x 1.0)
    /// against a title three edits off a seven-letter one released in the
    /// query's year (0.7 x 4/7 + 0.3). The exact title wins it, ahead of the
    /// exact year and a higher popularity, in either list order -- for a
    /// movie and for a show alike.
    #[test]
    fn an_exact_title_wins_a_tie_on_the_total_ahead_of_the_year_and_popularity() {
        let query = ("abcdefg", Some(2000));
        let mut exact = hit("abcdefg", None);
        exact.external_ref = ExternalMediaRef::new("tmdb", "exact");
        let mut dated = hit("abcdxyz", Some(2000));
        dated.external_ref = ExternalMediaRef::new("tmdb", "dated");
        dated.popularity = Some(99.0);
        let exact_score = score(query.0, query.1, &exact.title, None, exact.year);
        let dated_score = score(query.0, query.1, &dated.title, None, dated.year);
        assert_eq!(
            exact_score.total_score, dated_score.total_score,
            "the premise: the two tie on the total"
        );
        assert!(exact_score.accepted(DEFAULT_MIN_CONFIDENCE));
        assert!(dated_score.accepted(DEFAULT_MIN_CONFIDENCE));

        for hits in [
            vec![exact.clone(), dated.clone()],
            vec![dated.clone(), exact.clone()],
        ] {
            let (best, _) = best_movie_match(query.0, query.1, &hits, DEFAULT_MIN_CONFIDENCE)
                .expect("both clear the bar");
            assert_eq!(best.external_ref.as_str(), "tmdb:exact");

            let shows: Vec<ShowSearchHit> = hits.iter().map(as_show).collect();
            let (best, _) = best_show_match(query.0, query.1, &shows, DEFAULT_MIN_CONFIDENCE)
                .expect("both clear the bar");
            assert_eq!(best.external_ref.as_str(), "tmdb:exact");
        }
    }

    /// With the score, the title and the year all equal, the more popular
    /// hit wins, in either list order.
    #[test]
    fn popularity_decides_a_tie_on_everything_else() {
        let mut quiet = hit("Heat", Some(1995));
        quiet.external_ref = ExternalMediaRef::new("tmdb", "quiet");
        quiet.popularity = Some(1.0);
        let mut popular = quiet.clone();
        popular.external_ref = ExternalMediaRef::new("tmdb", "popular");
        popular.popularity = Some(2.0);

        for hits in [
            vec![quiet.clone(), popular.clone()],
            vec![popular.clone(), quiet.clone()],
        ] {
            let (best, _) = best_movie_match("Heat", Some(1995), &hits, DEFAULT_MIN_CONFIDENCE)
                .expect("both clear the bar");
            assert_eq!(best.external_ref.as_str(), "tmdb:popular");
            let shows: Vec<ShowSearchHit> = hits.iter().map(as_show).collect();
            let (best, _) = best_show_match("Heat", Some(1995), &shows, DEFAULT_MIN_CONFIDENCE)
                .expect("both clear the bar");
            assert_eq!(best.external_ref.as_str(), "tmdb:popular");
        }
    }

    fn as_show(hit: &MovieSearchHit) -> ShowSearchHit {
        ShowSearchHit {
            external_ref: hit.external_ref.clone(),
            title: hit.title.clone(),
            original_title: hit.original_title.clone(),
            year: hit.year,
            popularity: hit.popularity,
            vote_average: hit.vote_average,
        }
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

    #[test]
    fn an_earlier_provider_wins_a_full_tie() {
        // The same title, year and popularity from two providers, in the
        // order the composite asks them: nothing separates them but that
        // order, and the earlier one is the one stored.
        let mut movies = vec![hit("Arrival", Some(2016)), hit("Arrival", Some(2016))];
        movies[0].external_ref = ExternalMediaRef::new("tmdb", "329865");
        movies[1].external_ref = ExternalMediaRef::new("tvdb", "1105");
        let (best, _) = best_movie_match("Arrival", Some(2016), &movies, DEFAULT_MIN_CONFIDENCE)
            .expect("both hits clear the bar");
        assert_eq!(best.external_ref.as_str(), "tmdb:329865");

        movies.reverse();
        let (best, _) = best_movie_match("Arrival", Some(2016), &movies, DEFAULT_MIN_CONFIDENCE)
            .expect("both hits clear the bar");
        assert_eq!(best.external_ref.as_str(), "tvdb:1105");

        let mut shows = vec![
            show_hit("Frieren", Some(2023)),
            show_hit("Frieren", Some(2023)),
        ];
        shows[0].external_ref = ExternalMediaRef::new("tmdb", "209867");
        shows[1].external_ref = ExternalMediaRef::new("tvdb", "424536");
        let (best, _) = best_show_match("Frieren", Some(2023), &shows, DEFAULT_MIN_CONFIDENCE)
            .expect("both hits clear the bar");
        assert_eq!(best.external_ref.as_str(), "tmdb:209867");

        shows.reverse();
        let (best, _) = best_show_match("Frieren", Some(2023), &shows, DEFAULT_MIN_CONFIDENCE)
            .expect("both hits clear the bar");
        assert_eq!(best.external_ref.as_str(), "tvdb:424536");
    }

    #[test]
    fn a_nan_popularity_does_not_panic_the_ranking() {
        // Otherwise tied hits, so popularity is what decides: a `NaN` (or
        // an infinity) counts as no popularity, and loses to a real one
        // wherever it sits in the list.
        for malformed in [f64::NAN, f64::INFINITY] {
            let mut movies = vec![hit("Arrival", Some(2016)), hit("Arrival", Some(2016))];
            movies[0].external_ref = ExternalMediaRef::new("tmdb", "1");
            movies[0].popularity = Some(malformed);
            movies[1].external_ref = ExternalMediaRef::new("tvdb", "2");
            movies[1].popularity = Some(12.5);
            let (best, _) =
                best_movie_match("Arrival", Some(2016), &movies, DEFAULT_MIN_CONFIDENCE)
                    .expect("both hits clear the bar");
            assert_eq!(best.external_ref.as_str(), "tvdb:2");

            let mut shows = vec![
                show_hit("Frieren", Some(2023)),
                show_hit("Frieren", Some(2023)),
            ];
            shows[0].external_ref = ExternalMediaRef::new("tmdb", "1");
            shows[0].popularity = Some(3.0);
            shows[1].external_ref = ExternalMediaRef::new("tvdb", "2");
            shows[1].popularity = Some(malformed);
            let (best, _) = best_show_match("Frieren", Some(2023), &shows, DEFAULT_MIN_CONFIDENCE)
                .expect("both hits clear the bar");
            assert_eq!(best.external_ref.as_str(), "tmdb:1");
        }
    }

    mod invariants {
        use super::*;
        use proptest::prelude::*;

        /// Titles as providers and file names give them: any Unicode, with
        /// punctuation and runs of spaces, and sometimes empty.
        fn any_title() -> impl Strategy<Value = String> {
            prop_oneof![
                "\\PC{0,24}",
                "[a-zA-Z0-9 .:'!&-]{0,24}",
                Just(String::new()),
            ]
        }

        fn any_year() -> impl Strategy<Value = Option<u32>> {
            prop_oneof![
                Just(None),
                (1880u32..2100).prop_map(Some),
                any::<u32>().prop_map(Some)
            ]
        }

        fn any_hit() -> impl Strategy<Value = MovieSearchHit> {
            (
                any_title(),
                proptest::option::of(any_title()),
                any_year(),
                proptest::option::of(any::<f64>()),
                0u32..1000,
            )
                .prop_map(|(title, original_title, year, popularity, id)| {
                    MovieSearchHit {
                        external_ref: ExternalMediaRef::new("tmdb", &id.to_string()),
                        title,
                        original_title,
                        year,
                        popularity,
                        vote_average: None,
                    }
                })
        }

        proptest! {
            /// Normalizing what is already normal changes nothing.
            #[test]
            fn normalize_is_idempotent(title in any_title()) {
                let once = normalize(&title);
                prop_assert_eq!(normalize(&once), once);
            }

            /// Similarity is a fraction, the same either way round, and
            /// whole for a title against itself.
            #[test]
            fn similarity_is_bounded_symmetric_and_whole_for_itself(
                a in any_title(),
                b in any_title(),
            ) {
                let ab = string_similarity(&a, &b);
                prop_assert!((0.0..=1.0).contains(&ab), "{ab}");
                prop_assert_eq!(ab, string_similarity(&b, &a));
                prop_assert_eq!(string_similarity(&a, &a), 1.0);
            }

            /// Any query against any candidate scores without panicking,
            /// within the bounds the weights allow: the title part is a
            /// fraction of its weight, and the year moves it by at most the
            /// bonus or the penalty.
            #[test]
            fn a_score_is_bounded_by_its_weights(
                query in any_title(),
                query_year in any_year(),
                candidate in any_title(),
                original in proptest::option::of(any_title()),
                candidate_year in any_year(),
            ) {
                let scored = score(&query, query_year, &candidate, original.as_deref(), candidate_year);
                prop_assert!((0.0..=1.0).contains(&scored.title_score));
                prop_assert!(
                    (-0.20..=TITLE_WEIGHT + 0.30 + 1e-9).contains(&scored.total_score),
                    "{scored:?}"
                );
            }

            /// The best match is a hit that clears the bar and that no other
            /// hit clearing it outscores; there is none only when no hit
            /// clears it. Never a panic, whatever a provider's popularity.
            #[test]
            fn the_best_match_is_an_accepted_hit_no_other_outscores(
                query in any_title(),
                query_year in any_year(),
                hits in proptest::collection::vec(any_hit(), 0..8),
                min_confidence in 0.0f64..1.0,
            ) {
                let scores: Vec<MatchScore> = hits
                    .iter()
                    .map(|hit| score(&query, query_year, &hit.title, hit.original_title.as_deref(), hit.year))
                    .collect();
                let accepted: Vec<f64> = scores
                    .iter()
                    .filter(|scored| scored.accepted(min_confidence))
                    .map(|scored| scored.total_score)
                    .collect();
                match best_movie_match(&query, query_year, &hits, min_confidence) {
                    None => prop_assert!(accepted.is_empty()),
                    Some((best, best_score)) => {
                        prop_assert!(hits.iter().any(|hit| std::ptr::eq(hit, best)));
                        prop_assert!(best_score.accepted(min_confidence));
                        prop_assert!(accepted.iter().all(|total| *total <= best_score.total_score));
                    }
                }
                let shows: Vec<ShowSearchHit> = hits.iter().map(as_show).collect();
                prop_assert_eq!(
                    best_show_match(&query, query_year, &shows, min_confidence)
                        .map(|(best, _)| best.external_ref.clone()),
                    best_movie_match(&query, query_year, &hits, min_confidence)
                        .map(|(best, _)| best.external_ref.clone()),
                    "a show is ranked by the movie's rules"
                );
            }

            /// The top candidates are at most `n`, best first.
            #[test]
            fn the_top_candidates_are_at_most_n_best_first(
                query in any_title(),
                hits in proptest::collection::vec(any_hit(), 0..8),
                n in 0usize..10,
            ) {
                let top = top_movie_candidates(&query, None, &hits, n);
                prop_assert_eq!(top.len(), n.min(hits.len()));
                prop_assert!(top.windows(2).all(|pair| pair[0].1.total_score >= pair[1].1.total_score));
                let shows: Vec<ShowSearchHit> = hits.iter().map(as_show).collect();
                prop_assert_eq!(top_show_candidates(&query, None, &shows, n), top);
            }
        }
    }

    mod absolute_order {
        use super::*;
        use proptest::prelude::*;

        fn episode(number: u32, absolute: Option<u32>) -> EpisodeEnrichment {
            EpisodeEnrichment {
                episode_number: number,
                absolute_number: absolute,
                title: Some(format!("e{number}")),
                ..Default::default()
            }
        }

        /// A season of `count` episodes, unnumbered absolutely.
        fn season(number: u32, count: u32) -> SeasonEnrichment {
            SeasonEnrichment {
                season_number: number,
                episodes: (1..=count).map(|n| episode(n, None)).collect(),
                ..Default::default()
            }
        }

        fn placed(seasons: &[SeasonEnrichment], absolute: u32) -> Option<(u32, u32)> {
            absolute_episode(seasons, absolute)
                .map(|(season, episode)| (season, episode.episode_number))
        }

        #[test]
        fn the_order_runs_through_the_regular_seasons_and_skips_the_specials() {
            // Listed out of order, with specials first, as a provider may.
            let seasons = [season(2, 3), season(0, 4), season(1, 12)];
            for (absolute, expected) in [
                (1, Some((1, 1))),
                (12, Some((1, 12))),
                (13, Some((2, 1))),
                (15, Some((2, 3))),
                (16, None),
                (0, None),
            ] {
                assert_eq!(placed(&seasons, absolute), expected, "absolute {absolute}");
            }
        }

        #[test]
        fn a_season_with_a_gap_in_its_numbers_is_ordered_by_number() {
            let seasons = [SeasonEnrichment {
                season_number: 1,
                episodes: vec![episode(5, None), episode(1, None), episode(2, None)],
                ..Default::default()
            }];
            assert_eq!(placed(&seasons, 3), Some((1, 5)));
        }

        #[test]
        fn a_provider_numbering_absolutely_is_read_by_its_own_numbers() {
            // TheTVDB counts across seasons itself, and its count need not be
            // the seasons' positions: here a double-length season-one finale
            // takes two numbers.
            let seasons = [
                SeasonEnrichment {
                    season_number: 1,
                    episodes: vec![episode(1, Some(1)), episode(2, Some(2))],
                    ..Default::default()
                },
                SeasonEnrichment {
                    season_number: 2,
                    episodes: vec![episode(1, Some(4)), episode(2, Some(5))],
                    ..Default::default()
                },
                // A special carrying an absolute number is still outside the
                // order.
                SeasonEnrichment {
                    season_number: SPECIALS_SEASON,
                    episodes: vec![episode(1, Some(3))],
                    ..Default::default()
                },
            ];
            assert_eq!(placed(&seasons, 4), Some((2, 1)));
            assert_eq!(placed(&seasons, 5), Some((2, 2)));
            assert_eq!(placed(&seasons, 3), None, "no regular episode is 3");
        }

        /// A provider that numbers any regular episode absolutely is read by
        /// its numbers alone: an episode it left unnumbered is not in its
        /// order, and a number is never read as a position instead.
        #[test]
        fn a_numbered_provider_is_never_read_by_position() {
            let seasons = [
                SeasonEnrichment {
                    season_number: 1,
                    episodes: vec![episode(1, None), episode(2, Some(1))],
                    ..Default::default()
                },
                SeasonEnrichment {
                    season_number: 2,
                    episodes: vec![episode(1, None)],
                    ..Default::default()
                },
            ];
            assert_eq!(placed(&seasons, 1), Some((1, 2)));
            assert_eq!(
                placed(&seasons, 2),
                None,
                "the second position exists, but no episode is numbered 2"
            );
            assert_eq!(placed(&seasons, 3), None);
            assert_eq!(placed(&seasons, 0), None);
        }

        /// A number only a special carries does not make the provider a
        /// numbering one: the regular seasons are still read by position.
        #[test]
        fn a_numbered_special_alone_leaves_the_order_positional() {
            let seasons = [
                season(1, 2),
                SeasonEnrichment {
                    season_number: SPECIALS_SEASON,
                    episodes: vec![episode(1, Some(1))],
                    ..Default::default()
                },
            ];
            assert_eq!(placed(&seasons, 1), Some((1, 1)));
            assert_eq!(placed(&seasons, 2), Some((1, 2)));
        }

        /// Nothing is placed for a provider with no seasons, or only specials.
        #[test]
        fn no_regular_season_places_nothing() {
            assert_eq!(placed(&[], 1), None);
            assert_eq!(placed(&[season(SPECIALS_SEASON, 3)], 1), None);
        }

        #[test]
        fn the_placed_episode_carries_the_providers_metadata() {
            let seasons = [season(1, 2), season(2, 2)];
            let (_, episode) = absolute_episode(&seasons, 3).expect("in the order");
            assert_eq!(episode.title.as_deref(), Some("e1"));
        }

        proptest! {
            /// The positional order is the seasons laid end to end: every
            /// number from 1 to the regular episode count places exactly one
            /// episode, in season-then-episode order, and none past it does.
            #[test]
            fn the_positional_order_is_a_bijection_onto_the_regular_episodes(
                counts in proptest::collection::vec(0u32..30, 0..8),
                specials in 0u32..10,
            ) {
                let mut seasons: Vec<SeasonEnrichment> = counts
                    .iter()
                    .enumerate()
                    .map(|(index, count)| season(index as u32 + 1, *count))
                    .collect();
                seasons.push(season(SPECIALS_SEASON, specials));
                let total: u32 = counts.iter().sum();

                let order: Vec<(u32, u32)> =
                    (1..=total).filter_map(|n| placed(&seasons, n)).collect();
                prop_assert_eq!(order.len(), total as usize);
                prop_assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
                prop_assert!(order.iter().all(|(season, _)| *season != SPECIALS_SEASON));
                prop_assert_eq!(placed(&seasons, total + 1), None);
            }
        }
    }
}
