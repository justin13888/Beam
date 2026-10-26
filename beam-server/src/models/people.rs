//! Wire shapes for cast, crew and people (issue #261): the credits a title's
//! detail lists, people search's connection, and a person's filmography.

use kynos::Schema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::media::Title;
use crate::models::search::{PageInfo, TitleKind};

/// One cast credit on a title.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct CastCredit {
    /// The credited person: `GET /v1/people/{id}` lists their filmography.
    pub person_id: Uuid,
    /// The person's name.
    pub name: String,
    /// Path of the person's portrait, served by the artwork route; absent
    /// when the provider has none.
    pub profile_url: Option<String>,
    /// The character played, when the provider names one.
    pub character: Option<String>,
    /// Billing position, from 0 for the top-billed; the list is in this
    /// order.
    pub billing_order: u32,
}

/// One crew credit on a title.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct CrewCredit {
    /// The credited person: `GET /v1/people/{id}` lists their filmography.
    pub person_id: Uuid,
    /// The person's name.
    pub name: String,
    /// Path of the person's portrait, served by the artwork route; absent
    /// when the provider has none.
    pub profile_url: Option<String>,
    /// The job, as the provider names it: `Director`, `Screenplay`,
    /// `Original Music Composer`.
    pub job: String,
    /// The department the job belongs to (`Directing`, `Writing`), when the
    /// provider names one.
    pub department: Option<String>,
}

/// Someone credited on at least one title in the library.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct Person {
    /// Stable identifier for this person.
    pub id: Uuid,
    /// The person's name.
    pub name: String,
    /// Path of the person's portrait, served by the artwork route; absent
    /// when the provider has none.
    pub profile_url: Option<String>,
}

/// One page of people search.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct PersonConnection {
    /// The people on this page, by name.
    pub items: Vec<Person>,
    /// Where this page sits in the whole listing.
    pub page_info: PageInfo,
    /// How many people match, across every page.
    pub total: u64,
}

/// The title a filmography credit is on.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct CreditedTitle {
    /// The movie or show id: `GET /v1/media/{id}` is its detail.
    pub id: Uuid,
    /// Whether it is a movie or a show.
    pub media_type: TitleKind,
    /// The title's title.
    pub title: Title,
    /// The year it was released or first aired.
    pub year: Option<u32>,
    /// Path of the title's poster, served by the artwork route; absent when
    /// it has none.
    pub poster_url: Option<String>,
}

/// One cast credit in a person's filmography.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct FilmographyCastCredit {
    /// The title the person is credited on.
    pub title: CreditedTitle,
    /// The character played, when the provider names one.
    pub character: Option<String>,
    /// Billing position on the title, from 0 for the top-billed.
    pub billing_order: u32,
}

/// One crew credit in a person's filmography.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct FilmographyCrewCredit {
    /// The title the person is credited on.
    pub title: CreditedTitle,
    /// The job, as the provider names it.
    pub job: String,
    /// The department the job belongs to, when the provider names one.
    pub department: Option<String>,
}

/// One person, with every credit they have in the library.
///
/// Both lists are newest first -- by the title's year, a title with none
/// last -- and then by title.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct PersonDetail {
    /// Stable identifier for this person.
    pub id: Uuid,
    /// The person's name.
    pub name: String,
    /// Path of the person's portrait, served by the artwork route; absent
    /// when the provider has none.
    pub profile_url: Option<String>,
    /// The titles the person is in the cast of.
    pub cast: Vec<FilmographyCastCredit>,
    /// The titles the person is in the crew of, one entry per job.
    pub crew: Vec<FilmographyCrewCredit>,
}
