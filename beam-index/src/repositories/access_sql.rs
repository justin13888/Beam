//! An [`AccessPolicy`] (issue #263) as SQL conditions on a title, shared by
//! every store that lists titles or what hangs off them: the catalogue, the
//! watchlist and favourites, collections, people and genres (issue #328).
//!
//! A caller restricted to some libraries sees a title only when
//! `library_movies` / `library_shows` associates it with one of them, and a
//! caller under a rating ceiling only one whose `content_rating` is a
//! certification within it. Both are bound as `IN` lists the size of the
//! grant and of the ceiling's certifications; an unrestricted caller's
//! statement has neither. An empty grant or ceiling admits nothing, which
//! `IN ()` cannot spell, so it is `FALSE`.

use sea_orm::Value;

use beam_domain::models::access::{AccessPolicy, LibraryAccess};
use beam_domain::models::catalog::TitleKind;

/// A policy's placeholders, bound once and read by as many conditions as a
/// statement needs.
#[derive(Debug)]
pub(crate) struct AccessConditions {
    /// The placeholders of the libraries the caller may see; `None` for
    /// every library.
    libraries: Option<Vec<String>>,
    /// The placeholders of the certifications within the caller's ceiling;
    /// `None` for no ceiling.
    certifications: Option<Vec<String>>,
}

/// The title table, and the library junction and its title column, of `kind`.
fn tables(kind: TitleKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        TitleKind::Movie => ("movies", "library_movies", "movie_id"),
        TitleKind::Show => ("shows", "library_shows", "show_id"),
    }
}

impl AccessConditions {
    /// Bind `access`'s values through `push`, which binds a value and
    /// returns its placeholder.
    pub(crate) fn bind(access: &AccessPolicy, mut push: impl FnMut(Value) -> String) -> Self {
        let AccessPolicy {
            libraries,
            max_rating,
        } = access;
        Self {
            libraries: match libraries {
                LibraryAccess::All => None,
                LibraryAccess::Only(granted) => {
                    Some(granted.iter().map(|id| push((*id).into())).collect())
                }
            },
            certifications: max_rating.map(|ceiling| {
                ceiling
                    .certifications_within()
                    .map(|label| push(label.into()))
                    .collect()
            }),
        }
    }

    /// That a title of `kind` whose id is the SQL expression `id` is in a
    /// granted library; `None` for a policy granting every library.
    fn library(&self, kind: TitleKind, id: &str) -> Option<String> {
        let (_, junction, column) = tables(kind);
        match self.libraries.as_deref()? {
            [] => Some("FALSE".to_string()),
            granted => Some(format!(
                "EXISTS (SELECT 1 FROM {junction} l WHERE l.{column} = {id} \
                 AND l.library_id IN ({}))",
                granted.join(", ")
            )),
        }
    }

    /// That the certification the SQL expression `certification` reads is
    /// within the ceiling; `None` for no ceiling.
    fn rating(&self, certification: &str) -> Option<String> {
        match self.certifications.as_deref()? {
            [] => Some("FALSE".to_string()),
            within => Some(format!("{certification} IN ({})", within.join(", "))),
        }
    }

    /// The conditions on a title of `kind` whose id is the SQL expression
    /// `id` and whose certification is `certification`: none for an
    /// unrestricted policy.
    pub(crate) fn on(&self, kind: TitleKind, id: &str, certification: &str) -> Vec<String> {
        [self.library(kind, id), self.rating(certification)]
            .into_iter()
            .flatten()
            .collect()
    }

    /// [`Self::on`] for a title whose row the statement does not read, its
    /// certification looked up by its id: one condition, or `None` for an
    /// unrestricted policy.
    pub(crate) fn on_id(&self, kind: TitleKind, id: &str) -> Option<String> {
        let (table, _, _) = tables(kind);
        let rating = self.rating("t.content_rating").map(|rating| {
            format!("EXISTS (SELECT 1 FROM {table} t WHERE t.id = {id} AND {rating})")
        });
        let conditions: Vec<String> = [self.library(kind, id), rating]
            .into_iter()
            .flatten()
            .collect();
        (!conditions.is_empty()).then(|| conditions.join(" AND "))
    }

    /// The condition on a row holding one title in either of two columns --
    /// a movie's id in `movie_id`, or a show's in `show_id`, the other
    /// `NULL` -- that `access` admits it: `None` for an unrestricted policy.
    pub(crate) fn on_either(&self, movie_id: &str, show_id: &str) -> Option<String> {
        let movie = self.on_id(TitleKind::Movie, movie_id)?;
        let show = self.on_id(TitleKind::Show, show_id)?;
        Some(format!(
            "(({movie_id} IS NOT NULL AND {movie}) OR ({show_id} IS NOT NULL AND {show}))"
        ))
    }
}

#[cfg(test)]
mod tests {
    use beam_domain::models::access::ContentRating;
    use uuid::Uuid;

    use super::*;

    /// Bind `access` as `$1`, `$2`, ..., returning the conditions and what
    /// was bound.
    fn bound(access: &AccessPolicy) -> (AccessConditions, Vec<Value>) {
        let mut values = Vec::new();
        let conditions = AccessConditions::bind(access, |value| {
            values.push(value);
            format!("${}", values.len())
        });
        (conditions, values)
    }

    #[test]
    fn an_unrestricted_policy_binds_and_conditions_nothing() {
        let (conditions, values) = bound(&AccessPolicy::unrestricted());
        assert!(values.is_empty());
        assert!(
            conditions
                .on(TitleKind::Movie, "movies.id", "movies.content_rating")
                .is_empty()
        );
        assert_eq!(conditions.on_id(TitleKind::Show, "e.show_id"), None);
        assert_eq!(conditions.on_either("e.movie_id", "e.show_id"), None);
    }

    #[test]
    fn a_grant_and_a_ceiling_check_the_title_named_by_id() {
        let kids = Uuid::from_u128(0x1d);
        let (conditions, values) = bound(&AccessPolicy {
            libraries: LibraryAccess::Only([kids].into()),
            max_rating: Some(ContentRating::AllAges),
        });
        assert_eq!(values[0], Value::from(kids));
        assert_eq!(
            values[1..].to_vec(),
            ContentRating::AllAges
                .certifications_within()
                .map(Value::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            conditions.on_id(TitleKind::Movie, "cm.movie_id").as_deref(),
            Some(
                "EXISTS (SELECT 1 FROM library_movies l WHERE l.movie_id = cm.movie_id \
                 AND l.library_id IN ($1)) AND EXISTS (SELECT 1 FROM movies t \
                 WHERE t.id = cm.movie_id AND t.content_rating IN ($2, $3, $4))"
            )
        );
        assert_eq!(
            conditions.on_either("c.movie_id", "c.show_id").as_deref(),
            Some(
                "((c.movie_id IS NOT NULL AND EXISTS (SELECT 1 FROM library_movies l \
                 WHERE l.movie_id = c.movie_id AND l.library_id IN ($1)) AND EXISTS \
                 (SELECT 1 FROM movies t WHERE t.id = c.movie_id AND t.content_rating \
                 IN ($2, $3, $4))) OR (c.show_id IS NOT NULL AND EXISTS (SELECT 1 FROM \
                 library_shows l WHERE l.show_id = c.show_id AND l.library_id IN ($1)) \
                 AND EXISTS (SELECT 1 FROM shows t WHERE t.id = c.show_id AND \
                 t.content_rating IN ($2, $3, $4))))"
            )
        );
    }

    #[test]
    fn a_ceiling_alone_reads_the_certification_and_an_empty_grant_admits_nothing() {
        let (conditions, _) = bound(&AccessPolicy {
            libraries: LibraryAccess::All,
            max_rating: Some(ContentRating::Adult),
        });
        let on_movie = conditions
            .on_id(TitleKind::Movie, "j.movie_id")
            .expect("a ceiling is a condition");
        assert!(!on_movie.contains("library_movies"), "{on_movie}");
        assert!(
            on_movie.starts_with("EXISTS (SELECT 1 FROM movies t WHERE t.id = j.movie_id"),
            "{on_movie}"
        );

        let (nothing, values) = bound(&AccessPolicy {
            libraries: LibraryAccess::Only(Default::default()),
            max_rating: None,
        });
        assert!(values.is_empty());
        assert_eq!(
            nothing.on_id(TitleKind::Show, "j.show_id").as_deref(),
            Some("FALSE")
        );
    }
}
