//! [`ShowRepository::rehome_episode`](beam_domain::repositories::ShowRepository::rehome_episode)
//! over SQL (issue #334), apart from the rest of the show repository for size.

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement, TransactionTrait};
use uuid::Uuid;

use super::watch_state::carry_rows;
use beam_domain::models::Episode;
use beam_domain::models::watch_state::WatchTarget;

/// Move `episode_id` to episode `episode_number` of season `season_number`
/// of its show, merging it into an episode already there, in one
/// transaction; see the trait method for the contract.
pub(super) async fn rehome_episode(
    db: &DatabaseConnection,
    episode_id: Uuid,
    season_number: u32,
    episode_number: u32,
) -> Result<Option<Episode>, DbErr> {
    use beam_entity::{episode, files, season};
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, Set};

    let txn = db.begin().await?;
    // Locked, so a concurrent re-home of the same episode waits for this
    // one and then finds it gone or already moved.
    let Some(source) = episode::Entity::find_by_id(episode_id)
        .lock_exclusive()
        .one(&txn)
        .await?
    else {
        return Ok(None);
    };
    let Some(source_season) = season::Entity::find_by_id(source.season_id)
        .one(&txn)
        .await?
    else {
        return Ok(None);
    };
    let show_id = source_season.show_id;
    season::Entity::insert(season::ActiveModel {
        id: Set(Uuid::new_v4()),
        show_id: Set(show_id),
        season_number: Set(season_number as i32),
        ..Default::default()
    })
    .on_conflict_do_nothing_on([season::Column::ShowId, season::Column::SeasonNumber])
    .exec_without_returning(&txn)
    .await?;
    let target_season = season::Entity::find()
        .filter(season::Column::ShowId.eq(show_id))
        .filter(season::Column::SeasonNumber.eq(season_number as i32))
        .one(&txn)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "season {season_number} of show {show_id} is not readable after \
                 find-or-create"
            ))
        })?;
    let occupant = episode::Entity::find()
        .filter(episode::Column::SeasonId.eq(target_season.id))
        .filter(episode::Column::EpisodeNumber.eq(episode_number as i32))
        .one(&txn)
        .await?;

    let home = match occupant {
        Some(occupant) if occupant.id == source.id => {
            txn.commit().await?;
            return Ok(Some(Episode::from(occupant)));
        }
        Some(occupant) => {
            // Merged: the occupant takes the files (and with them their
            // chapters and trickplay) and the watch state, then the moved
            // row goes.
            files::Entity::update_many()
                .col_expr(files::Column::EpisodeId, Expr::value(occupant.id))
                .filter(files::Column::EpisodeId.eq(source.id))
                .exec(&txn)
                .await?;
            let target = |episode_id| WatchTarget::Episode {
                episode_id,
                show_id,
            };
            carry_rows(&txn, target(source.id), target(occupant.id)).await?;
            episode::Entity::delete_by_id(source.id).exec(&txn).await?;
            let absolute_number = occupant.absolute_number.or(source.absolute_number);
            episode::Entity::update_many()
                .col_expr(
                    episode::Column::AbsoluteNumber,
                    Expr::value(absolute_number),
                )
                .filter(episode::Column::Id.eq(occupant.id))
                .exec(&txn)
                .await?;
            beam_entity::episode::Model {
                absolute_number,
                ..occupant
            }
        }
        None => {
            episode::Entity::update_many()
                .col_expr(episode::Column::SeasonId, Expr::value(target_season.id))
                .col_expr(
                    episode::Column::EpisodeNumber,
                    Expr::value(episode_number as i32),
                )
                .filter(episode::Column::Id.eq(source.id))
                .exec(&txn)
                .await?;
            beam_entity::episode::Model {
                season_id: target_season.id,
                episode_number: episode_number as i32,
                ..source.clone()
            }
        }
    };
    // A season the move empties goes with it.
    txn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM seasons AS s WHERE s.id = $1 \
           AND NOT EXISTS (SELECT 1 FROM episodes AS e WHERE e.season_id = s.id)",
        [source.season_id.into()],
    ))
    .await?;
    txn.commit().await?;
    Ok(Some(Episode::from(home)))
}
