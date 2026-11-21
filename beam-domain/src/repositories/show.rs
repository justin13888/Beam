use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DbErr;
use uuid::Uuid;

use std::collections::{BTreeSet, HashMap};

use crate::models::catalog::ShowChildCounts;
use crate::models::enrichment::{FieldLocks, MetadataField};
use crate::models::pin::{PinSource, ProviderPin};
use crate::models::show::{CreateEpisode, CreateShow, Episode, Season, Show};
use crate::providers::enrichment::{SeasonEnrichment, ShowEnrichment};
use crate::utils::next_up::OutlineEpisode;

/// Persistence for shows, their seasons and their episodes.
///
/// A show is keyed and kept live exactly as a movie is -- see
/// [`crate::repositories::MovieRepository`]: the indexer matches a series
/// folder to a show by its identity key, never its display title, and a show
/// is live -- listed by the [`crate::repositories::CatalogRepository`] --
/// while at least one episode has a present file.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait ShowRepository: Send + Sync + std::fmt::Debug {
    /// Live or not.
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Show>, DbErr>;
    /// Every show, live or not.
    async fn find_all(&self) -> Result<Vec<Show>, DbErr>;
    /// The shows among `ids`, as [`crate::repositories::MovieRepository::find_by_ids`].
    async fn find_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Show>, DbErr>;
    /// How many seasons and episodes each of `show_ids` has, counting every
    /// season and episode row whether or not a file backs it -- the same rows
    /// a show's detail lists. A show with no season is absent from the map.
    /// One statement however many ids, and none for an empty slice.
    async fn child_counts(
        &self,
        show_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, ShowChildCounts>, DbErr>;
    /// The show keyed `create.identity_key`, inserting it only if no show
    /// carries that key yet. An existing show is returned unchanged; a show
    /// with no key is never matched.
    ///
    /// Atomic: concurrent calls for one key all return the same row. This is
    /// what lets two episodes of a new show be indexed at once without the
    /// second failing on, or duplicating, the first's show.
    async fn find_or_create_by_identity(&self, create: CreateShow) -> Result<Show, DbErr>;
    /// Every show with no identity key, for the indexer's backfill, oldest
    /// first (`created_at`, then `id`), so of two legacy duplicates the
    /// backfill keys the original.
    async fn find_unkeyed(&self) -> Result<Vec<Show>, DbErr>;
    /// Give the keyless show `show_id` the key `identity_key`, derived by
    /// version `version` of the rules. Returns `false`, changing nothing, when
    /// the show does not exist, already has a key, or another show already
    /// holds `identity_key`.
    async fn assign_identity_key(
        &self,
        show_id: Uuid,
        identity_key: &str,
        version: u16,
    ) -> Result<bool, DbErr>;
    /// The show keyed `identity_key`, if any.
    async fn find_by_identity_key(&self, identity_key: &str) -> Result<Option<Show>, DbErr>;
    /// Every keyed show whose key an older version of the rules than
    /// `version` derived, oldest first -- see
    /// [`crate::repositories::MovieRepository::find_keyed_before_version`].
    async fn find_keyed_before_version(&self, version: u16) -> Result<Vec<Show>, DbErr>;
    /// Replace the key of `show_id` with `identity_key` (`None`: keyless),
    /// derived by version `version` of the rules, exactly as
    /// [`crate::repositories::MovieRepository::rekey`] does for a movie.
    async fn rekey(
        &self,
        show_id: Uuid,
        identity_key: Option<String>,
        version: u16,
    ) -> Result<bool, DbErr>;
    /// The show `pin` names (issue #184): the one pinned to it, else the
    /// oldest one enrichment matched to that provider id (its `tmdb_id`,
    /// `imdb_id`, `tvdb_id` or `anilist_id`). A file whose NFO names `pin`
    /// joins this show whatever its path is keyed as.
    async fn find_by_pin(&self, pin: &ProviderPin) -> Result<Option<Show>, DbErr>;
    /// Pin `show_id` to `pin`, set by `source`, replacing any pin it had --
    /// except that an NFO's pin never replaces an administrator's (FR-312).
    /// Returns `false`, changing nothing, when the show does not exist, another
    /// show is already pinned to `pin`, or `source` is an NFO and the show is
    /// pinned by an administrator.
    async fn set_pinned_ref(
        &self,
        show_id: Uuid,
        pin: &ProviderPin,
        source: PinSource,
    ) -> Result<bool, DbErr>;
    /// Unpin `show_id` if an administrator pinned it; see
    /// `MovieRepository::clear_admin_pin`.
    async fn clear_admin_pin(&self, show_id: Uuid) -> Result<bool, DbErr>;
    /// Delete every episode created before `created_before` that no file row
    /// references, then every season left with no episode, then every show
    /// created before `created_before` left with no season, returning how many
    /// shows went. As for movies, a soft-deleted file row still counts.
    /// Deleting a show takes its library associations, enrichment state and
    /// genre links with it.
    async fn delete_orphaned(&self, created_before: DateTime<Utc>) -> Result<u64, DbErr>;
    async fn ensure_library_association(
        &self,
        library_id: Uuid,
        show_id: Uuid,
    ) -> Result<(), DbErr>;
    /// The libraries each of `show_ids` is associated with, as
    /// [`crate::repositories::MovieRepository::library_ids`] reads a movie's.
    async fn library_ids(&self, show_ids: &[Uuid]) -> Result<HashMap<Uuid, BTreeSet<Uuid>>, DbErr>;
    /// The season numbered `season_number` of `show_id`, inserting it only
    /// if no such season exists yet.
    ///
    /// Atomic: concurrent calls for one pair all return the same row.
    async fn find_or_create_season(
        &self,
        show_id: Uuid,
        season_number: u32,
    ) -> Result<Season, DbErr>;
    async fn find_seasons_by_show_id(&self, show_id: Uuid) -> Result<Vec<Season>, DbErr>;
    async fn find_episodes_by_season_id(&self, season_id: Uuid) -> Result<Vec<Episode>, DbErr>;
    /// The episode numbered `create.episode_number` in `create.season_id`,
    /// inserting it only if no such episode exists yet.
    ///
    /// An episode is one logical row per `(season_id, episode_number)` -- the
    /// pair carries a unique index -- however many files are indexed for it: a
    /// 1080p and a 720p rip of the same episode are two sources of one
    /// episode. When the episode already exists it is returned **unchanged**:
    /// `create.title` and `create.runtime` are used only to populate a new row,
    /// so a later file's filename parse never overwrites what the first file
    /// (or enrichment since) established. Mirrors the movie side, where the
    /// indexer reuses a movie found by identity key without touching it.
    ///
    /// Atomic: concurrent calls for one pair all return the same row.
    ///
    /// An `Absolute` episode is first looked for by its running number among
    /// the season's show's episodes indexed into the same season
    /// ([`Episode::absolute_number`], [`Episode::absolute_season_number`]),
    /// wherever [`Self::rehome_episode`] has since moved it, so a rescan of
    /// its file never moves it back (issue #334), while the same bare number
    /// in another season's folder is another episode. A new row records its
    /// numbering. An `Absolute` file that finds a `Standard` episode with no
    /// running number at its place -- one indexed before numbering was
    /// recorded -- upgrades it to the file's numbering, so enrichment can
    /// re-home it; any other existing episode is returned unchanged.
    async fn find_or_create_episode(&self, create: CreateEpisode) -> Result<Episode, DbErr>;
    /// Move `episode_id` to episode `episode_number` of season
    /// `season_number` of its show (issue #334), creating that season if
    /// need be, and return the episode it now is: itself, moved with its
    /// files, watch state and chapters, or -- where an episode already holds
    /// that place -- that episode, which takes its files and every user's
    /// watch state (folded as `WatchStateRepository::carry` folds them) and
    /// its `absolute_number` and `absolute_season_number` where it had none,
    /// the moved row being deleted.
    /// A season the move leaves with no episode is deleted. Moving an
    /// episode to where it is changes nothing; `None` when it does not
    /// exist. Atomic.
    async fn rehome_episode(
        &self,
        episode_id: Uuid,
        season_number: u32,
        episode_number: u32,
    ) -> Result<Option<Episode>, DbErr>;
    /// Reverse lookup from a `MediaFileContent::Episode { episode_id, .. }` back
    /// to the episode -- used together with `find_season_by_id` to resolve a
    /// file id to its show for continue-watching.
    async fn find_episode_by_id(&self, episode_id: Uuid) -> Result<Option<Episode>, DbErr>;
    /// Reverse lookup from `Episode::season_id` to the season (and, via
    /// `Season::show_id`, the show).
    async fn find_season_by_id(&self, season_id: Uuid) -> Result<Option<Season>, DbErr>;
    /// Every episode of `show_id`, by season and episode number, each with
    /// whether a present file backs it -- what next-up walks (issue #188).
    /// One statement however many episodes the show has.
    async fn episode_outline(&self, show_id: Uuid) -> Result<Vec<OutlineEpisode>, DbErr>;
    /// [`Self::episode_outline`] of each of `show_ids`, by show. A show with
    /// no episodes, or no such show, is absent. One statement however many
    /// shows, none for an empty slice.
    async fn episode_outlines(
        &self,
        show_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<OutlineEpisode>>, DbErr>;
    /// Apply enrichment-provider data to an existing show. Overwrites the
    /// current values except those `locks` holds, same as
    /// `MovieRepository::apply_enrichment`, and likewise never touches the
    /// identity key.
    async fn apply_enrichment(
        &self,
        show_id: Uuid,
        enrichment: &ShowEnrichment,
        locks: &FieldLocks,
    ) -> Result<(), DbErr>;
    /// Apply a season's enrichment to the show's *existing* season/episode
    /// rows. Never fabricates a season or episode that scanning hasn't
    /// already created from a real file -- rows with no local counterpart
    /// are silently skipped. Returns the number of episodes updated.
    async fn apply_season_enrichment(
        &self,
        show_id: Uuid,
        enrichment: &SeasonEnrichment,
    ) -> Result<u32, DbErr>;
}

// Out of line for size. Excluded from mutation by path in
// `.cargo/mutants.toml`: an attribute proc macro is unstable on a non-inline
// `mod`, so `#[mutants::skip]` cannot go here.
#[path = "show_in_memory.rs"]
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory;

#[cfg(test)]
#[path = "show_contract_tests.rs"]
mod contract_over_in_memory;
