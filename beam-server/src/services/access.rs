//! Who may see which titles (issue #263): reads a user's
//! [`AccessPolicy`] and answers, for any id a route is given, whether the
//! policy hides what it names.
//!
//! A title is the unit of access ([`beam_domain::models::access`]). Every
//! season, episode, file and sidecar of a title is hidden exactly when the
//! title is, and a route answers a hidden one as it answers an id that names
//! nothing -- `404` -- so its existence does not leak.
//!
//! An unrestricted policy hides nothing, and every check returns before
//! reading anything: a server whose administrator never restricts anyone pays
//! one policy read per request and nothing more.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use sea_orm::DbErr;
use uuid::Uuid;

use beam_domain::models::access::AccessPolicy;
use beam_domain::models::file::MediaFileContent;
use beam_domain::models::watch_state::TitleRef;
use beam_domain::repositories::{
    AccessPolicyRepository, FileRepository, MovieRepository, ShowRepository,
};

/// Every store [`AccessService`] reads.
#[derive(Debug)]
pub struct AccessRepositories {
    pub policies: Arc<dyn AccessPolicyRepository>,
    pub movies: Arc<dyn MovieRepository>,
    pub shows: Arc<dyn ShowRepository>,
    pub files: Arc<dyn FileRepository>,
}

/// Reads access policies and applies them to ids.
#[derive(Debug)]
pub struct AccessService {
    policies: Arc<dyn AccessPolicyRepository>,
    movies: Arc<dyn MovieRepository>,
    shows: Arc<dyn ShowRepository>,
    files: Arc<dyn FileRepository>,
}

impl AccessService {
    pub fn new(repositories: AccessRepositories) -> Self {
        let AccessRepositories {
            policies,
            movies,
            shows,
            files,
        } = repositories;
        Self {
            policies,
            movies,
            shows,
            files,
        }
    }

    /// The user's policy.
    pub async fn policy(&self, user_id: Uuid) -> Result<AccessPolicy, DbErr> {
        self.policies.find(user_id).await
    }

    /// Replace the user's policy; see [`AccessPolicyRepository::replace`].
    pub async fn replace(&self, user_id: Uuid, policy: &AccessPolicy) -> Result<(), DbErr> {
        self.policies.replace(user_id, policy).await
    }

    /// Which of `titles` the policy admits. A title that does not exist is
    /// not admitted. A fixed number of reads, however many titles.
    pub async fn visible_titles(
        &self,
        policy: &AccessPolicy,
        titles: &[TitleRef],
    ) -> Result<HashSet<TitleRef>, DbErr> {
        if policy.is_unrestricted() {
            return Ok(titles.iter().copied().collect());
        }
        let mut movie_ids = Vec::new();
        let mut show_ids = Vec::new();
        for title in titles {
            match *title {
                TitleRef::Movie(id) => movie_ids.push(id),
                TitleRef::Show(id) => show_ids.push(id),
            }
        }
        let mut visible = HashSet::new();
        if !movie_ids.is_empty() {
            let libraries = self.movies.library_ids(&movie_ids).await?;
            for movie in self.movies.find_by_ids(&movie_ids).await? {
                let held_in = libraries.get(&movie.id).cloned().unwrap_or_default();
                if policy.admits_title(&held_in, movie.content_rating.as_deref()) {
                    visible.insert(TitleRef::Movie(movie.id));
                }
            }
        }
        if !show_ids.is_empty() {
            let libraries = self.shows.library_ids(&show_ids).await?;
            for show in self.shows.find_by_ids(&show_ids).await? {
                let held_in = libraries.get(&show.id).cloned().unwrap_or_default();
                if policy.admits_title(&held_in, show.content_rating.as_deref()) {
                    visible.insert(TitleRef::Show(show.id));
                }
            }
        }
        Ok(visible)
    }

    /// Whether the policy hides the title `title`. One that does not exist is
    /// not hidden: there is nothing to hide, and the caller answers it as
    /// unknown anyway.
    pub async fn hides_title(&self, policy: &AccessPolicy, title: TitleRef) -> Result<bool, DbErr> {
        if policy.is_unrestricted() {
            return Ok(false);
        }
        let (libraries, certification) = match title {
            TitleRef::Movie(id) => {
                let Some(movie) = self.movies.find_by_id(id).await? else {
                    return Ok(false);
                };
                let mut libraries = self.movies.library_ids(&[id]).await?;
                (
                    libraries.remove(&id).unwrap_or_default(),
                    movie.content_rating,
                )
            }
            TitleRef::Show(id) => {
                let Some(show) = self.shows.find_by_id(id).await? else {
                    return Ok(false);
                };
                let mut libraries = self.shows.library_ids(&[id]).await?;
                (
                    libraries.remove(&id).unwrap_or_default(),
                    show.content_rating,
                )
            }
        };
        Ok(!policy.admits_title(&libraries, certification.as_deref()))
    }

    /// The title `id` names or belongs to: a movie or show itself, or the
    /// show of a season or an episode. `None` for an id that names none of
    /// them.
    pub async fn title_of(&self, id: Uuid) -> Result<Option<TitleRef>, DbErr> {
        if self.movies.find_by_id(id).await?.is_some() {
            return Ok(Some(TitleRef::Movie(id)));
        }
        if self.shows.find_by_id(id).await?.is_some() {
            return Ok(Some(TitleRef::Show(id)));
        }
        if let Some(episode) = self.shows.find_episode_by_id(id).await? {
            return self.show_of_season(episode.season_id).await;
        }
        self.show_of_season(id).await
    }

    async fn show_of_season(&self, season_id: Uuid) -> Result<Option<TitleRef>, DbErr> {
        Ok(self
            .shows
            .find_season_by_id(season_id)
            .await?
            .map(|season| TitleRef::Show(season.show_id)))
    }

    /// Whether the policy hides what `id` names: a movie, a show, or a season
    /// or episode of a show it hides. An id naming nothing is not hidden.
    pub async fn hides(&self, policy: &AccessPolicy, id: Uuid) -> Result<bool, DbErr> {
        if policy.is_unrestricted() {
            return Ok(false);
        }
        match self.title_of(id).await? {
            Some(title) => self.hides_title(policy, title).await,
            None => Ok(false),
        }
    }

    /// Whether the policy hides the file `file_id`: one of a title it hides,
    /// or -- for a file no title holds -- one in a library it does not grant,
    /// or any such file under a rating ceiling, since nothing certifies it.
    /// An id naming no file is not hidden.
    pub async fn hides_file(&self, policy: &AccessPolicy, file_id: Uuid) -> Result<bool, DbErr> {
        if policy.is_unrestricted() {
            return Ok(false);
        }
        let Some(file) = self.files.find_by_id(file_id).await? else {
            return Ok(false);
        };
        let title = match file.content {
            Some(MediaFileContent::Movie { movie_entry_id, .. }) => self
                .movies
                .find_entry_by_id(movie_entry_id)
                .await?
                .map(|entry| TitleRef::Movie(entry.movie_id)),
            Some(MediaFileContent::Episode { episode_id, .. }) => {
                match self.shows.find_episode_by_id(episode_id).await? {
                    Some(episode) => self.show_of_season(episode.season_id).await?,
                    None => None,
                }
            }
            None => None,
        };
        match title {
            Some(title) => self.hides_title(policy, title).await,
            None => Ok(!policy.admits_title(&BTreeSet::from([file.library_id]), None)),
        }
    }
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod access_tests;
