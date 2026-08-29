use async_trait::async_trait;
use sea_orm::DbErr;
use uuid::Uuid;

use crate::models::genre::Genre;

/// Genres, shared across movies/shows via junction tables and upserted by
/// slug so the same genre name reuses one row across titles.
#[async_trait]
pub trait GenreRepository: Send + Sync + std::fmt::Debug {
    /// Upsert genres by slug and replace the movie's complete genre set with
    /// them (idempotent -- safe to call again with the same or updated names).
    async fn set_movie_genres(&self, movie_id: Uuid, names: &[String]) -> Result<(), DbErr>;
    /// Same as `set_movie_genres`, for shows.
    async fn set_show_genres(&self, show_id: Uuid, names: &[String]) -> Result<(), DbErr>;
    async fn find_all(&self) -> Result<Vec<Genre>, DbErr>;
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

#[mutants::skip]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory {
    use super::*;
    use parking_lot::RwLock;
    use std::collections::HashMap;

    #[derive(Debug, Default)]
    pub struct InMemoryGenreRepository {
        genres: RwLock<HashMap<String, Genre>>, // keyed by slug
        movie_genres: RwLock<HashMap<Uuid, Vec<Uuid>>>,
        show_genres: RwLock<HashMap<Uuid, Vec<Uuid>>>,
    }

    impl InMemoryGenreRepository {
        fn upsert_genres(&self, names: &[String]) -> Vec<Uuid> {
            let mut genres = self.genres.write();
            names
                .iter()
                .map(|name| {
                    let slug = slugify(name);
                    genres
                        .entry(slug.clone())
                        .or_insert_with(|| Genre {
                            id: Uuid::new_v4(),
                            name: name.clone(),
                            slug,
                        })
                        .id
                })
                .collect()
        }

        pub fn genres_for_movie(&self, movie_id: Uuid) -> Vec<Genre> {
            let ids = self
                .movie_genres
                .read()
                .get(&movie_id)
                .cloned()
                .unwrap_or_default();
            let genres = self.genres.read();
            ids.iter()
                .filter_map(|id| genres.values().find(|g| g.id == *id).cloned())
                .collect()
        }

        pub fn genres_for_show(&self, show_id: Uuid) -> Vec<Genre> {
            let ids = self
                .show_genres
                .read()
                .get(&show_id)
                .cloned()
                .unwrap_or_default();
            let genres = self.genres.read();
            ids.iter()
                .filter_map(|id| genres.values().find(|g| g.id == *id).cloned())
                .collect()
        }
    }

    #[async_trait]
    impl GenreRepository for InMemoryGenreRepository {
        async fn set_movie_genres(&self, movie_id: Uuid, names: &[String]) -> Result<(), DbErr> {
            let ids = self.upsert_genres(names);
            self.movie_genres.write().insert(movie_id, ids);
            Ok(())
        }

        async fn set_show_genres(&self, show_id: Uuid, names: &[String]) -> Result<(), DbErr> {
            let ids = self.upsert_genres(names);
            self.show_genres.write().insert(show_id, ids);
            Ok(())
        }

        async fn find_all(&self) -> Result<Vec<Genre>, DbErr> {
            Ok(self.genres.read().values().cloned().collect())
        }
    }
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
