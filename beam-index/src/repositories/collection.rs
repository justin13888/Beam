//! SQL implementation of [`CollectionRepository`] (issue #262).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ColumnTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait, FromQueryResult, QueryFilter,
    Set, Statement, TransactionTrait, Value,
};
use uuid::Uuid;

use beam_domain::models::access::AccessPolicy;
use beam_domain::models::catalog::TitleKind;
use beam_domain::models::collection::{Collection, CollectionSummary};
use beam_domain::providers::enrichment::CollectionEnrichment;
use beam_domain::repositories::CollectionRepository;
use beam_entity::{collection, collection_movie};

use crate::repositories::access_sql::AccessConditions;

/// SQL-based implementation of the CollectionRepository trait.
#[derive(Debug, Clone)]
pub struct SqlCollectionRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlCollectionRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

/// That a membership `collection_movies cm` is of a movie `access` admits
/// (issue #328), binding its values after those already in `values`; `TRUE`
/// for an unrestricted policy.
fn visible_members(access: &AccessPolicy, values: &mut Vec<Value>) -> String {
    AccessConditions::bind(access, |value| {
        values.push(value);
        format!("${}", values.len())
    })
    .on_id(TitleKind::Movie, "cm.movie_id")
    .unwrap_or_else(|| "TRUE".to_string())
}

#[async_trait]
impl CollectionRepository for SqlCollectionRepository {
    async fn set_movie_collection(
        &self,
        movie_id: Uuid,
        enrichment: Option<&CollectionEnrichment>,
    ) -> Result<(), DbErr> {
        use sea_orm::sea_query::OnConflict;

        // One transaction: a reader sees the movie in its old collection or
        // its new one, never in none between the two.
        let txn = self.db.begin().await?;
        collection_movie::Entity::delete_many()
            .filter(collection_movie::Column::MovieId.eq(movie_id))
            .exec(&txn)
            .await?;
        if let Some(enrichment) = enrichment {
            let now: chrono::DateTime<chrono::FixedOffset> = Utc::now().into();
            let external_ref = enrichment.collection_ref.as_str().to_string();
            collection::Entity::insert(collection::ActiveModel {
                id: Set(Uuid::new_v4()),
                external_ref: Set(external_ref.clone()),
                name: Set(enrichment.name.clone()),
                poster_url: Set(enrichment.poster_url.clone()),
                backdrop_url: Set(enrichment.backdrop_url.clone()),
                created_at: Set(now),
                updated_at: Set(now),
            })
            .on_conflict(
                OnConflict::column(collection::Column::ExternalRef)
                    .update_columns([
                        collection::Column::Name,
                        collection::Column::PosterUrl,
                        collection::Column::BackdropUrl,
                        collection::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&txn)
            .await?;
            let stored = collection::Entity::find()
                .filter(collection::Column::ExternalRef.eq(external_ref.clone()))
                .one(&txn)
                .await?
                .ok_or_else(|| {
                    DbErr::RecordNotFound(format!("collection {external_ref} as it was upserted"))
                })?;
            collection_movie::Entity::insert(collection_movie::ActiveModel {
                movie_id: Set(movie_id),
                collection_id: Set(stored.id),
            })
            .exec(&txn)
            .await?;
        }
        txn.commit().await
    }

    async fn movie_collection(&self, movie_id: Uuid) -> Result<Option<Collection>, DbErr> {
        Ok(collection_movie::Entity::find_by_id(movie_id)
            .find_also_related(collection::Entity)
            .one(self.db.as_ref())
            .await?
            .and_then(|(_, collection)| collection)
            .map(Collection::from))
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<Collection>, DbErr> {
        Ok(collection::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .map(Collection::from))
    }

    async fn movie_ids(
        &self,
        collection_id: Uuid,
        access: &AccessPolicy,
    ) -> Result<Vec<Uuid>, DbErr> {
        #[derive(Debug, FromQueryResult)]
        struct Member {
            movie_id: Uuid,
        }

        let mut values: Vec<Value> = vec![collection_id.into()];
        let visible = visible_members(access, &mut values);
        Ok(Member::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT cm.movie_id FROM collection_movies cm \
                 WHERE cm.collection_id = $1 AND {visible}"
            ),
            values,
        ))
        .all(self.db.as_ref())
        .await?
        .into_iter()
        .map(|member| member.movie_id)
        .collect())
    }

    async fn list(
        &self,
        access: &AccessPolicy,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<CollectionSummary>, DbErr> {
        #[derive(Debug, FromQueryResult)]
        struct Listed {
            id: Uuid,
            external_ref: String,
            name: String,
            poster_url: Option<String>,
            backdrop_url: Option<String>,
            movie_count: i64,
        }

        // `COLLATE "C"` makes `lower` fold ASCII letters only and orders by
        // code point, whatever the database's locale: the in-memory store's
        // order, as people search's.
        let mut values: Vec<Value> = Vec::new();
        let visible = visible_members(access, &mut values);
        values.push(i64::try_from(limit).unwrap_or(i64::MAX).into());
        values.push(i64::try_from(offset).unwrap_or(i64::MAX).into());
        let listed = Listed::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT c.id, c.external_ref, c.name, c.poster_url, c.backdrop_url, \
                        m.movie_count \
                 FROM collections c \
                 JOIN (SELECT cm.collection_id, count(*) AS movie_count \
                       FROM collection_movies cm WHERE {visible} \
                       GROUP BY cm.collection_id) m \
                   ON m.collection_id = c.id \
                 ORDER BY lower(c.name COLLATE \"C\"), c.id \
                 LIMIT ${} OFFSET ${}",
                values.len() - 1,
                values.len()
            ),
            values,
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(listed
            .into_iter()
            .map(|row| {
                let Listed {
                    id,
                    external_ref,
                    name,
                    poster_url,
                    backdrop_url,
                    movie_count,
                } = row;
                CollectionSummary {
                    collection: Collection {
                        id,
                        external_ref,
                        name,
                        poster_url,
                        backdrop_url,
                    },
                    movie_count: u64::try_from(movie_count).unwrap_or(0),
                }
            })
            .collect())
    }

    async fn count(&self, access: &AccessPolicy) -> Result<u64, DbErr> {
        #[derive(Debug, FromQueryResult)]
        struct Counted {
            count: i64,
        }

        let mut values: Vec<Value> = Vec::new();
        let visible = visible_members(access, &mut values);
        let counted = Counted::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT count(DISTINCT cm.collection_id) AS count \
                 FROM collection_movies cm WHERE {visible}"
            ),
            values,
        ))
        .one(self.db.as_ref())
        .await?;
        Ok(counted.map_or(0, |c| u64::try_from(c.count).unwrap_or(0)))
    }
}
