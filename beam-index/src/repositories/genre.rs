use async_trait::async_trait;
use sea_orm::{DatabaseConnection, DbErr};
use uuid::Uuid;

use beam_domain::models::Genre;
use beam_domain::repositories::GenreRepository;
use beam_domain::repositories::genre::slugify;

/// SQL-based implementation of the GenreRepository trait.
#[derive(Debug, Clone)]
pub struct SqlGenreRepository {
    db: DatabaseConnection,
}

impl SqlGenreRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    async fn upsert_genres(&self, names: &[String]) -> Result<Vec<Uuid>, DbErr> {
        use beam_entity::genre;
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

        let mut ids = Vec::with_capacity(names.len());
        for name in names {
            let slug = slugify(name);
            let existing = genre::Entity::find()
                .filter(genre::Column::Slug.eq(slug.clone()))
                .one(&self.db)
                .await?;
            let id = match existing {
                Some(model) => model.id,
                None => {
                    let new_genre = genre::ActiveModel {
                        id: Set(Uuid::new_v4()),
                        name: Set(name.clone()),
                        slug: Set(slug),
                    };
                    new_genre.insert(&self.db).await?.id
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
            .exec(&self.db)
            .await?;

        for genre_id in ids {
            movie_genre::ActiveModel {
                movie_id: Set(movie_id),
                genre_id: Set(genre_id),
            }
            .insert(&self.db)
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
            .exec(&self.db)
            .await?;

        for genre_id in ids {
            show_genre::ActiveModel {
                show_id: Set(show_id),
                genre_id: Set(genre_id),
            }
            .insert(&self.db)
            .await?;
        }

        Ok(())
    }

    async fn find_all(&self) -> Result<Vec<Genre>, DbErr> {
        use beam_entity::genre;
        use sea_orm::EntityTrait;

        let models = genre::Entity::find().all(&self.db).await?;
        Ok(models.into_iter().map(Genre::from).collect())
    }
}
