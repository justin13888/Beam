use super::*;
use beam_domain::models::CreateMovieEntry;
use beam_domain::repositories::MovieRepository;

use crate::repositories::SqlMovieRepository;

/// One entry per `(library, movie, edition)` (issue #182): the insert
/// targets the whole triple -- the `NULLS NOT DISTINCT` index, so a second
/// default-edition copy conflicts -- and the read-back matches a missing
/// edition with `IS NULL`, since `= NULL` matches nothing.
#[tokio::test]
async fn find_or_create_entry_inserts_with_do_nothing_on_the_triple_then_reads_it() {
    for edition in [None, Some("Director's Cut")] {
        let db = connection(empty_mock());
        let repo = SqlMovieRepository::new(db.clone());
        let library = Uuid::from_u128(91);
        let movie = Uuid::from_u128(92);
        let _ = repo
            .find_or_create_entry(CreateMovieEntry {
                library_id: library,
                movie_id: movie,
                edition: edition.map(str::to_string),
            })
            .await;
        drop(repo);

        let sql = statements(db);
        assert_eq!(sql.len(), 2, "one insert, one read-back: {sql:?}");
        assert_contains(
            &sql[0],
            r#"ON CONFLICT ("library_id", "movie_id", "edition") DO NOTHING"#,
        );
        assert_filters(&sql[1], "movie_entries", "library_id", "=");
        assert_filters(&sql[1], "movie_entries", "movie_id", "=");
        assert_bound(&sql[1], &library.to_string());
        assert_bound(&sql[1], &movie.to_string());
        match edition {
            Some(edition) => {
                assert_filters(&sql[1], "movie_entries", "edition", "=");
                assert_bound(&sql[1], edition);
            }
            None => assert_filters(&sql[1], "movie_entries", "edition", "IS NULL"),
        }
    }
}
