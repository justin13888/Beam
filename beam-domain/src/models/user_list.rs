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
