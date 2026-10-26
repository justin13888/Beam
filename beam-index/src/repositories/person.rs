use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait,
    FromQueryResult, QueryFilter, Set, Statement, TransactionTrait, Value,
};
use uuid::Uuid;

use beam_domain::models::catalog::TitleKind;
use beam_domain::models::person::{CreditRole, Person, PersonCredit, TitleCredit};
use beam_domain::providers::enrichment::CreditEnrichment;
use beam_domain::repositories::PersonRepository;
use beam_domain::repositories::person::distinct_credits;
use beam_entity::{credit, person};

/// SQL-based implementation of the PersonRepository trait.
#[derive(Debug, Clone)]
pub struct SqlPersonRepository {
    db: Arc<DatabaseConnection>,
}

impl SqlPersonRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Replace `title`'s credits with `credits`, upserting their people, in
    /// one transaction: a reader sees the old set or the new one, never a
    /// title with half its cast.
    async fn set_credits(
        &self,
        title: (TitleKind, Uuid),
        credits: &[CreditEnrichment],
    ) -> Result<(), DbErr> {
        let credits = distinct_credits(credits);
        let txn = self.db.begin().await?;
        let people = upsert_people(&txn, &credits).await?;

        let (kind, title_id) = title;
        let owner = match kind {
            TitleKind::Movie => credit::Column::MovieId,
            TitleKind::Show => credit::Column::ShowId,
        };
        credit::Entity::delete_many()
            .filter(owner.eq(title_id))
            .exec(&txn)
            .await?;

        let rows: Vec<credit::ActiveModel> = credits
            .iter()
            .filter_map(|c| {
                people
                    .get(c.person_ref.as_str())
                    .map(|person_id| credit_row(kind, title_id, *person_id, &c.role))
            })
            .collect();
        if !rows.is_empty() {
            credit::Entity::insert_many(rows).exec(&txn).await?;
        }
        txn.commit().await
    }

    async fn credits_of(
        &self,
        owner: credit::Column,
        title_id: Uuid,
    ) -> Result<Vec<TitleCredit>, DbErr> {
        let rows = credit::Entity::find()
            .filter(owner.eq(title_id))
            .find_also_related(person::Entity)
            .all(self.db.as_ref())
            .await?;
        rows.into_iter()
            .filter_map(|(credit, person)| person.map(|person| (credit, person)))
            .map(|(credit, person)| {
                Ok(TitleCredit {
                    person: Person::from(person),
                    role: role_of(&credit)?,
                })
            })
            .collect()
    }
}

/// Upsert the people `credits` name, one statement, and return each one's id
/// by provider id. A person listed twice takes the name and portrait listed
/// last, as storing the credits one by one would.
async fn upsert_people(
    db: &impl ConnectionTrait,
    credits: &[&CreditEnrichment],
) -> Result<HashMap<String, Uuid>, DbErr> {
    use sea_orm::sea_query::OnConflict;

    let mut latest: HashMap<&str, &CreditEnrichment> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for credit in credits {
        let key = credit.person_ref.as_str();
        if latest.insert(key, credit).is_none() {
            order.push(key);
        }
    }
    if order.is_empty() {
        return Ok(HashMap::new());
    }

    let now: chrono::DateTime<chrono::FixedOffset> = Utc::now().into();
    let rows = order.iter().map(|key| {
        let credit = latest[key];
        person::ActiveModel {
            id: Set(Uuid::new_v4()),
            external_ref: Set((*key).to_string()),
            name: Set(credit.name.clone()),
            profile_url: Set(credit.profile_url.clone()),
            created_at: Set(now),
            updated_at: Set(now),
        }
    });
    person::Entity::insert_many(rows)
        .on_conflict(
            OnConflict::column(person::Column::ExternalRef)
                .update_columns([
                    person::Column::Name,
                    person::Column::ProfileUrl,
                    person::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec(db)
        .await?;

    let stored = person::Entity::find()
        .filter(person::Column::ExternalRef.is_in(order.iter().map(|key| (*key).to_string())))
        .all(db)
        .await?;
    Ok(stored
        .into_iter()
        .map(|model| (model.external_ref, model.id))
        .collect())
}

fn credit_row(
    kind: TitleKind,
    title_id: Uuid,
    person_id: Uuid,
    role: &CreditRole,
) -> credit::ActiveModel {
    let (movie_id, show_id) = match kind {
        TitleKind::Movie => (Some(title_id), None),
        TitleKind::Show => (None, Some(title_id)),
    };
    let (kind, character, billing_order, job, department) = match role {
        CreditRole::Cast {
            character,
            billing_order,
        } => (
            credit::KIND_CAST,
            character.clone(),
            // A billing order past `i32::MAX` is no billing order anyone
            // reads; it is kept last rather than refused.
            Some(i32::try_from(*billing_order).unwrap_or(i32::MAX)),
            None,
            None,
        ),
        CreditRole::Crew { job, department } => (
            credit::KIND_CREW,
            None,
            None,
            Some(job.clone()),
            department.clone(),
        ),
    };
    credit::ActiveModel {
        id: Set(Uuid::new_v4()),
        person_id: Set(person_id),
        movie_id: Set(movie_id),
        show_id: Set(show_id),
        kind: Set(kind.to_string()),
        character: Set(character),
        billing_order: Set(billing_order),
        job: Set(job),
        department: Set(department),
    }
}

/// The role a stored credit records, or an error for a row the table's
/// `CHECK`s should have refused.
fn role_of(credit: &credit::Model) -> Result<CreditRole, DbErr> {
    match credit.kind.as_str() {
        credit::KIND_CAST => Ok(CreditRole::Cast {
            character: credit.character.clone(),
            billing_order: credit
                .billing_order
                .and_then(|order| u32::try_from(order).ok())
                .unwrap_or_default(),
        }),
        credit::KIND_CREW => Ok(CreditRole::Crew {
            job: credit.job.clone().unwrap_or_default(),
            department: credit.department.clone(),
        }),
        other => Err(DbErr::Custom(format!(
            "credit {} has unknown kind {other:?}",
            credit.id
        ))),
    }
}

/// The `ILIKE` pattern, escaped with `\`, matching any text that contains
/// `query`: a `%` or `_` in it is that character, not a wildcard.
pub(crate) fn contains_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for c in query.chars() {
        if matches!(c, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

/// The `WHERE` clause people search reads, and its binds: credited people,
/// and with a query only those whose name contains it.
fn search_filter(query: Option<&str>) -> (String, Vec<Value>) {
    let credited = "EXISTS (SELECT 1 FROM credits c WHERE c.person_id = people.id)";
    match query {
        Some(query) => (
            format!("{credited} AND people.name ILIKE $1 ESCAPE '\\'"),
            vec![contains_pattern(query).into()],
        ),
        None => (credited.to_string(), Vec::new()),
    }
}

#[async_trait]
impl PersonRepository for SqlPersonRepository {
    async fn set_movie_credits(
        &self,
        movie_id: Uuid,
        credits: &[CreditEnrichment],
    ) -> Result<(), DbErr> {
        self.set_credits((TitleKind::Movie, movie_id), credits)
            .await
    }

    async fn set_show_credits(
        &self,
        show_id: Uuid,
        credits: &[CreditEnrichment],
    ) -> Result<(), DbErr> {
        self.set_credits((TitleKind::Show, show_id), credits).await
    }

    async fn movie_credits(&self, movie_id: Uuid) -> Result<Vec<TitleCredit>, DbErr> {
        self.credits_of(credit::Column::MovieId, movie_id).await
    }

    async fn show_credits(&self, show_id: Uuid) -> Result<Vec<TitleCredit>, DbErr> {
        self.credits_of(credit::Column::ShowId, show_id).await
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<Person>, DbErr> {
        Ok(person::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .map(Person::from))
    }

    async fn filmography(&self, person_id: Uuid) -> Result<Vec<PersonCredit>, DbErr> {
        let rows = credit::Entity::find()
            .filter(credit::Column::PersonId.eq(person_id))
            .all(self.db.as_ref())
            .await?;
        rows.into_iter()
            .map(|credit| {
                let (title_kind, title_id) = match (credit.movie_id, credit.show_id) {
                    (Some(id), _) => (TitleKind::Movie, id),
                    (None, Some(id)) => (TitleKind::Show, id),
                    (None, None) => {
                        return Err(DbErr::Custom(format!(
                            "credit {} is on no title",
                            credit.id
                        )));
                    }
                };
                Ok(PersonCredit {
                    title_kind,
                    title_id,
                    role: role_of(&credit)?,
                })
            })
            .collect()
    }

    async fn search(
        &self,
        query: Option<&str>,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Person>, DbErr> {
        let (filter, mut values) = search_filter(query);
        let limit_at = values.len() + 1;
        // Applied to the name, `COLLATE "C"` makes `lower` fold ASCII
        // letters only -- whatever the database's locale -- and orders the
        // result by code point. That is exactly `str::to_ascii_lowercase`
        // compared as Rust strings, the in-memory store's order, so a page
        // boundary falls in the same place in both. Folding by the
        // database's locale would not: a `C`-locale database folds nothing
        // beyond ASCII while Rust's `to_lowercase` folds all of Unicode.
        let sql = format!(
            "SELECT people.* FROM people WHERE {filter} \
             ORDER BY lower(people.name COLLATE \"C\"), people.id \
             LIMIT ${limit_at} OFFSET ${}",
            limit_at + 1
        );
        values.push(i64::try_from(limit).unwrap_or(i64::MAX).into());
        values.push(i64::try_from(offset).unwrap_or(i64::MAX).into());
        let rows = person::Model::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(rows.into_iter().map(Person::from).collect())
    }

    async fn count(&self, query: Option<&str>) -> Result<u64, DbErr> {
        #[derive(Debug, FromQueryResult)]
        struct Counted {
            count: i64,
        }

        let (filter, values) = search_filter(query);
        let counted = Counted::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT count(*) AS count FROM people WHERE {filter}"),
            values,
        ))
        .one(self.db.as_ref())
        .await?;
        Ok(counted.map_or(0, |c| u64::try_from(c.count).unwrap_or(0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_matches_the_query_anywhere_and_its_wildcards_literally() {
        for (query, pattern) in [
            ("smith", "%smith%"),
            ("100%", "%100\\%%"),
            ("a_b", "%a\\_b%"),
            ("back\\slash", "%back\\\\slash%"),
            ("", "%%"),
        ] {
            assert_eq!(contains_pattern(query), pattern, "for {query:?}");
        }
    }
}
