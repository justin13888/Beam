//! People and the roles they are credited with on titles (issue #261).
//!
//! A person is whoever a metadata provider credits on a title -- an actor, a
//! director, a composer -- identified by the provider's own id for them, so
//! the same actor credited on two titles is one person. Credits are written by
//! enrichment, a title's whole set at a time, exactly as its genres are.

use std::cmp::Ordering;

use uuid::Uuid;

use crate::models::catalog::TitleKind;

/// Someone a provider credits on at least one title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: Uuid,
    /// The provider-qualified id the person was credited under, e.g.
    /// `"tmdb:287"`: what makes two credits one person.
    pub external_ref: String,
    pub name: String,
    /// The provider's portrait of the person, served through Beam's artwork
    /// route rather than handed to clients (ADR-0015).
    pub profile_url: Option<String>,
}

/// What a person did on a title.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CreditRole {
    /// On screen (or behind a voice): the character, when the provider names
    /// one, and the billing order, from 0 for the top-billed.
    Cast {
        character: Option<String>,
        billing_order: u32,
    },
    /// Behind the camera: the job (`Director`), and the department it belongs
    /// to (`Directing`) when the provider names one.
    Crew {
        job: String,
        department: Option<String>,
    },
}

/// One credit on a title, with the person it credits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleCredit {
    pub person: Person,
    pub role: CreditRole,
}

/// One credit in a person's filmography: the title it is on, and the role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonCredit {
    pub title_kind: TitleKind,
    pub title_id: Uuid,
    pub role: CreditRole,
}

/// The order a title's credits are listed in, cast before crew: the cast by
/// billing order, the crew by department and then job, each then by name so
/// the order is total. Done here rather than by a SQL `ORDER BY` so every
/// store lists them alike, whatever its collation.
pub fn compare_title_credits(a: &TitleCredit, b: &TitleCredit) -> Ordering {
    fn rank(role: &CreditRole) -> (u8, u32, String, String) {
        match role {
            CreditRole::Cast { billing_order, .. } => {
                (0, *billing_order, String::new(), String::new())
            }
            CreditRole::Crew { job, department } => (
                1,
                0,
                department.as_deref().unwrap_or_default().to_lowercase(),
                job.to_lowercase(),
            ),
        }
    }
    rank(&a.role)
        .cmp(&rank(&b.role))
        .then_with(|| {
            a.person
                .name
                .to_lowercase()
                .cmp(&b.person.name.to_lowercase())
        })
        .then_with(|| a.person.id.cmp(&b.person.id))
}

#[cfg(feature = "entity")]
impl From<beam_entity::person::Model> for Person {
    fn from(model: beam_entity::person::Model) -> Self {
        let beam_entity::person::Model {
            id,
            external_ref,
            name,
            profile_url,
            created_at: _,
            updated_at: _,
        } = model;
        Self {
            id,
            external_ref,
            name,
            profile_url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credit(name: &str, role: CreditRole) -> TitleCredit {
        TitleCredit {
            person: Person {
                id: Uuid::new_v4(),
                external_ref: format!("tmdb:{name}"),
                name: name.to_string(),
                profile_url: None,
            },
            role,
        }
    }

    fn cast(billing_order: u32) -> CreditRole {
        CreditRole::Cast {
            character: None,
            billing_order,
        }
    }

    fn crew(department: Option<&str>, job: &str) -> CreditRole {
        CreditRole::Crew {
            job: job.to_string(),
            department: department.map(str::to_string),
        }
    }

    /// A small domain so ties on role, name and id actually occur.
    fn any_credit() -> impl proptest::strategy::Strategy<Value = TitleCredit> {
        use proptest::prelude::*;
        let name = proptest::sample::select(vec!["ann", "Ann", "bob"]);
        let word = proptest::sample::select(vec!["Directing", "directing", "Sound"]);
        let role = prop_oneof![
            (0_u32..3, proptest::option::of(word.clone())).prop_map(|(order, character)| {
                CreditRole::Cast {
                    character: character.map(str::to_owned),
                    billing_order: order,
                }
            }),
            (word.clone(), proptest::option::of(word)).prop_map(|(job, department)| {
                CreditRole::Crew {
                    job: job.to_owned(),
                    department: department.map(str::to_owned),
                }
            }),
        ];
        (0_u128..3, name, role).prop_map(|(id, name, role)| TitleCredit {
            person: Person {
                id: Uuid::from_u128(id),
                external_ref: format!("tmdb:{id}"),
                name: name.to_owned(),
                profile_url: None,
            },
            role,
        })
    }

    proptest::proptest! {
        /// The listing order is total: swapping the operands reverses it, it
        /// is transitive, and two different people never tie -- so every
        /// store lists a title's credits in one order.
        #[test]
        fn the_credit_order_is_total(a in any_credit(), b in any_credit(), c in any_credit()) {
            use proptest::prelude::*;
            let ab = compare_title_credits(&a, &b);
            prop_assert_eq!(compare_title_credits(&a, &a), Ordering::Equal);
            prop_assert_eq!(ab, compare_title_credits(&b, &a).reverse());
            if ab == Ordering::Equal {
                prop_assert_eq!(a.person.id, b.person.id, "two people tie");
            }
            let bc = compare_title_credits(&b, &c);
            if ab != Ordering::Greater && bc != Ordering::Greater {
                prop_assert_ne!(compare_title_credits(&a, &c), Ordering::Greater);
            }
            if ab == bc {
                prop_assert_eq!(compare_title_credits(&a, &c), ab);
            }
        }
    }

    #[test]
    fn the_cast_comes_first_in_billing_order_then_the_crew_by_department_and_job() {
        let mut credits = [
            credit("Composer", crew(Some("Sound"), "Original Music Composer")),
            credit("Lead", cast(0)),
            credit("Writer", crew(Some("Writing"), "Screenplay")),
            credit("Third", cast(2)),
            credit("Director", crew(Some("Directing"), "Director")),
            credit("Second", cast(1)),
            credit("Uncategorised", crew(None, "Producer")),
        ];
        credits.sort_by(compare_title_credits);
        let names: Vec<&str> = credits.iter().map(|c| c.person.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Lead",
                "Second",
                "Third",
                // A crew credit with no department sorts as the empty one.
                "Uncategorised",
                "Director",
                "Composer",
                "Writer",
            ]
        );
    }

    #[test]
    fn equal_roles_fall_back_to_the_name_ignoring_case() {
        let mut credits = [
            credit("bob", crew(Some("Writing"), "Writer")),
            credit("Alice", crew(Some("Writing"), "Writer")),
        ];
        credits.sort_by(compare_title_credits);
        assert_eq!(credits[0].person.name, "Alice");
        assert_eq!(credits[1].person.name, "bob");
    }
}
