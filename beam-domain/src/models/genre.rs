use uuid::Uuid;

/// A genre, shared across movies and shows via junction tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Genre {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[cfg(feature = "entity")]
impl From<beam_entity::genre::Model> for Genre {
    fn from(model: beam_entity::genre::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            slug: model.slug,
        }
    }
}
