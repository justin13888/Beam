//! A user's watchlist and favourites (issue #262).
//!
//! Each is a list of top-level titles -- movies and shows, never a season or
//! an episode -- that the user put there, newest addition first. A title is
//! on a list at most once: adding it again keeps it where it was.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::models::watch_state::TitleRef;

/// Which of a user's lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UserList {
    /// What the user means to watch.
    Watchlist,
    /// What the user marked as a favourite.
    Favourites,
}

impl UserList {
    /// Every list, in a fixed order.
    pub const ALL: [Self; 2] = [Self::Watchlist, Self::Favourites];

    /// The spelling the store and a cursor carry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Watchlist => "watchlist",
            Self::Favourites => "favourites",
        }
    }

    /// The inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|list| list.as_str() == value)
    }
}

/// One title on one of a user's lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserListEntry {
    pub id: Uuid,
    pub user_id: Uuid,
    pub list: UserList,
    pub title: TitleRef,
    /// When the user added it: the order a list is read in, newest first.
    pub added_at: DateTime<Utc>,
}

/// Where a page of a list ends: its last entry's `(added_at, id)`, the order
/// a list is read in, newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserListPosition {
    pub added_at: DateTime<Utc>,
    pub id: Uuid,
}

impl From<&UserListEntry> for UserListPosition {
    fn from(entry: &UserListEntry) -> Self {
        Self {
            added_at: entry.added_at,
            id: entry.id,
        }
    }
}

#[cfg(feature = "entity")]
impl TryFrom<beam_entity::user_list_entry::Model> for UserListEntry {
    type Error = sea_orm::DbErr;

    fn try_from(model: beam_entity::user_list_entry::Model) -> Result<Self, Self::Error> {
        let beam_entity::user_list_entry::Model {
            id,
            user_id,
            list,
            movie_id,
            show_id,
            added_at,
        } = model;
        let list = UserList::parse(&list).ok_or_else(|| {
            sea_orm::DbErr::Custom(format!("user list entry {id} is on unknown list {list:?}"))
        })?;
        // The table's CHECK holds exactly one of the two; a row with neither
        // or both is refused here rather than guessed at.
        let title = match (movie_id, show_id) {
            (Some(movie_id), None) => TitleRef::Movie(movie_id),
            (None, Some(show_id)) => TitleRef::Show(show_id),
            _ => {
                return Err(sea_orm::DbErr::Custom(format!(
                    "user list entry {id} names neither one movie nor one show"
                )));
            }
        };
        Ok(Self {
            id,
            user_id,
            list,
            title,
            added_at: added_at.with_timezone(&Utc),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_list_parses_back_from_its_spelling() {
        for list in UserList::ALL {
            assert_eq!(UserList::parse(list.as_str()), Some(list));
        }
        assert_eq!(UserList::parse("Watchlist"), None, "spellings are exact");
        assert_eq!(UserList::parse("history"), None);
    }
}

#[cfg(all(test, feature = "entity"))]
mod entity_conversion_tests {
    use super::*;

    fn model(
        list: &str,
        movie_id: Option<Uuid>,
        show_id: Option<Uuid>,
    ) -> beam_entity::user_list_entry::Model {
        beam_entity::user_list_entry::Model {
            id: Uuid::from_u128(1),
            user_id: Uuid::from_u128(2),
            list: list.to_string(),
            movie_id,
            show_id,
            added_at: DateTime::UNIX_EPOCH.into(),
        }
    }

    /// An entry is on a list Beam knows and names exactly one movie or one
    /// show; a row breaking either is refused, never read as a guess.
    #[test]
    fn a_row_converts_only_on_a_known_list_naming_one_title() {
        let (movie, show) = (Some(Uuid::from_u128(10)), Some(Uuid::from_u128(11)));
        let cases = [
            (
                "a movie on the watchlist",
                model("watchlist", movie, None),
                Some((UserList::Watchlist, TitleRef::Movie(Uuid::from_u128(10)))),
            ),
            (
                "a show in favourites",
                model("favourites", None, show),
                Some((UserList::Favourites, TitleRef::Show(Uuid::from_u128(11)))),
            ),
            ("an unknown list", model("history", movie, None), None),
            (
                "a list in the wrong case",
                model("Watchlist", movie, None),
                None,
            ),
            ("no title", model("watchlist", None, None), None),
            (
                "both a movie and a show",
                model("watchlist", movie, show),
                None,
            ),
        ];
        for (name, row, expected) in cases {
            let converted = UserListEntry::try_from(row);
            match expected {
                Some((list, title)) => {
                    let entry = converted.unwrap_or_else(|err| panic!("{name}: {err}"));
                    assert_eq!((entry.list, entry.title), (list, title), "{name}");
                }
                None => assert!(
                    matches!(&converted, Err(sea_orm::DbErr::Custom(message))
                        if message.contains(&Uuid::from_u128(1).to_string())),
                    "{name}: {converted:?}"
                ),
            }
        }
    }
}
