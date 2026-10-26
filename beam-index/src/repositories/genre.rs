use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::Genre;
use beam_domain::models::access::AccessPolicy;
use beam_domain::models::catalog::TitleKind;
use beam_domain::repositories::GenreRepository;
use beam_domain::repositories::genre::{slugify, sort_genre_names};

use crate::repositories::access_sql::AccessConditions;

/// SQL-based implementation of the GenreRepository trait.
#[derive(Debug, Clone)]
pub struct SqlGenreRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlGenreRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// The genre names of each owner in `ids`, read through the junction
    /// `junction` whose owner column is `owner` -- one statement, none for no
    /// ids.
    async fn names_by_owner(
        &self,
        junction: &str,
        owner: &str,
        ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<String>>, DbErr> {
        use sea_orm::{DbBackend, FromQueryResult, Statement, Value};

        #[derive(Debug, FromQueryResult)]
        struct Named {
            owner_id: Uuid,
            name: String,
        }

        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders: Vec<String> = (1..=ids.len()).map(|n| format!("${n}")).collect();
        let values: Vec<Value> = ids.iter().map(|id| (*id).into()).collect();
        let sql = format!(
            "SELECT j.{owner} AS owner_id, g.name FROM {junction} j \
             JOIN genres g ON g.id = j.genre_id WHERE j.{owner} IN ({})",
            placeholders.join(", ")
        );
        let rows = Named::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;
        let mut names: HashMap<Uuid, Vec<String>> = HashMap::new();
        for Named { owner_id, name } in rows {
            names.entry(owner_id).or_default().push(name);
        }
        for list in names.values_mut() {
            sort_genre_names(list);
        }
        Ok(names)
    }

    async fn upsert_genres(&self, names: &[String]) -> Result<Vec<Uuid>, DbErr> {
        use beam_entity::genre;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        let mut ids = Vec::with_capacity(names.len());
        for name in names {
            let slug = slugify(name);
            let existing = genre::Entity::find()
                .filter(genre::Column::Slug.eq(slug.clone()))
                .one(self.db.as_ref())
                .await?;
            let id = match existing {
                Some(model) => model.id,
                None => {
                    let new_genre = genre::ActiveModel {
                        id: Set(Uuid::new_v4()),
                        name: Set(name.clone()),
                        slug: Set(slug),
                    };
                    new_genre.insert(self.db.as_ref()).await?.id
                }
            };
            ids.push(id);
        }
        Ok(ids)
    }
}

#[async_trait]
impl GenreRepository for SqlGenreRepository {
    async fn set_movie_genres(&self, movie_id: Uuid, names: &[String]) -> Result<(), DbErr> {
        use beam_entity::movie_genre;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        let ids = self.upsert_genres(names).await?;

        movie_genre::Entity::delete_many()
            .filter(movie_genre::Column::MovieId.eq(movie_id))
            .exec(self.db.as_ref())
            .await?;

        for genre_id in ids {
            movie_genre::ActiveModel {
                movie_id: Set(movie_id),
                genre_id: Set(genre_id),
            }
            .insert(self.db.as_ref())
            .await?;
        }

        Ok(())
    }

    async fn set_show_genres(&self, show_id: Uuid, names: &[String]) -> Result<(), DbErr> {
        use beam_entity::show_genre;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        let ids = self.upsert_genres(names).await?;

        show_genre::Entity::delete_many()
            .filter(show_genre::Column::ShowId.eq(show_id))
            .exec(self.db.as_ref())
            .await?;

        for genre_id in ids {
            show_genre::ActiveModel {
                show_id: Set(show_id),
                genre_id: Set(genre_id),
            }
            .insert(self.db.as_ref())
            .await?;
        }

        Ok(())
    }

    async fn find_all(&self) -> Result<Vec<Genre>, DbErr> {
        use beam_entity::genre;
        use sea_orm::EntityTrait;

        let models = genre::Entity::find().all(self.db.as_ref()).await?;
        Ok(models.into_iter().map(Genre::from).collect())
    }

    async fn find_carried(&self, access: &AccessPolicy) -> Result<Vec<Genre>, DbErr> {
        use beam_entity::genre;
        use sea_orm::{DbBackend, FromQueryResult, Statement, Value};

        let mut values: Vec<Value> = Vec::new();
        let conditions = AccessConditions::bind(access, |value| {
            values.push(value);
            format!("${}", values.len())
        });
        let carried_by = |junction: &str, kind: TitleKind, owner: &str| {
            let visible = conditions
                .on_id(kind, &format!("j.{owner}"))
                .map(|visible| format!(" AND {visible}"))
                .unwrap_or_default();
            format!("EXISTS (SELECT 1 FROM {junction} j WHERE j.genre_id = g.id{visible})")
        };
        let sql = format!(
            "SELECT g.id, g.name, g.slug FROM genres g WHERE {} OR {}",
            carried_by("movie_genres", TitleKind::Movie, "movie_id"),
            carried_by("show_genres", TitleKind::Show, "show_id"),
        );
        let models = genre::Model::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(models.into_iter().map(Genre::from).collect())
    }

    async fn movie_genre_names(
        &self,
        movie_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<String>>, DbErr> {
        self.names_by_owner("movie_genres", "movie_id", movie_ids)
            .await
    }

    async fn show_genre_names(
        &self,
        show_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<String>>, DbErr> {
        self.names_by_owner("show_genres", "show_id", show_ids)
            .await
    }
}
