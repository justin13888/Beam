use super::*;

/// [`harness`], and the store credits are written to.
fn harness_with_people(
    provider: InMemoryEnrichmentProvider,
) -> (Harness, Arc<InMemoryPersonRepository>) {
    let (harness, _notifications, people) = full_harness(provider);
    (harness, people)
}

/// [`harness`], and the store collections are written to.
fn harness_with_collections(
    provider: InMemoryEnrichmentProvider,
    collections: Arc<dyn CollectionRepository>,
) -> Harness {
    harness_storing(Arc::new(provider), |people| people, collections).0
}

fn credit(native: &str, name: &str, role: CreditRole) -> CreditEnrichment {
    CreditEnrichment {
        person_ref: ExternalMediaRef::new("tmdb", native),
        name: name.to_string(),
        profile_url: None,
        role,
    }
}

fn lead(character: &str) -> CreditRole {
    CreditRole::Cast {
        character: Some(character.to_string()),
        billing_order: 0,
    }
}

/// The names credited on a movie that held `stored` credits once a match
/// reporting `reported` is applied to it.
async fn movie_credits_after_match(
    stored: &[CreditEnrichment],
    reported: Option<Vec<CreditEnrichment>>,
) -> Vec<String> {
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "603"),
        title: "The Matrix".to_string(),
        original_title: None,
        year: Some(1999),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("The Matrix", vec![hit])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            credits: reported,
            ..Default::default()
        });
    let ((service, movie_repo, _shows, state_repo, _genres, _clock), people) =
        harness_with_people(provider);
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("The Matrix".to_string(), Some(1999), None))
        .await
        .unwrap();
    people.set_movie_credits(movie.id, stored).await.unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    let mut names: Vec<String> = people
        .movie_credits(movie.id)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.person.name)
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_match_replaces_the_cast_and_crew_whole() {
    let names = movie_credits_after_match(
        &[credit("1", "Stale Credit", lead("Old"))],
        Some(vec![
            credit("6384", "Keanu Reeves", lead("Neo")),
            credit(
                "9339",
                "Lana Wachowski",
                CreditRole::Crew {
                    job: "Director".to_string(),
                    department: Some("Directing".to_string()),
                },
            ),
        ]),
    )
    .await;
    assert_eq!(names, ["Keanu Reeves", "Lana Wachowski"]);
}

#[tokio::test]
async fn a_match_reporting_no_credits_clears_them() {
    let names =
        movie_credits_after_match(&[credit("1", "Stale Credit", lead("Old"))], Some(vec![])).await;
    assert!(names.is_empty(), "{names:?}");
}

#[tokio::test]
async fn a_match_without_credits_keeps_the_ones_stored() {
    let names =
        movie_credits_after_match(&[credit("6384", "Keanu Reeves", lead("Neo"))], None).await;
    assert_eq!(
        names,
        ["Keanu Reeves"],
        "credits a provider did not report are not credits it removed"
    );
}

#[tokio::test]
async fn a_show_match_stores_its_credits() {
    let show_ref = ExternalMediaRef::new("tmdb", "1396");
    let hit = ShowSearchHit {
        external_ref: show_ref.clone(),
        title: "Breaking Bad".to_string(),
        original_title: None,
        year: Some(2008),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_show_search("Breaking Bad", vec![hit])
        .with_show_enrichment(
            show_ref,
            ShowEnrichment {
                tmdb_id: Some(1396),
                title: "Breaking Bad".to_string(),
                year: Some(2008),
                credits: Some(vec![credit(
                    "17419",
                    "Bryan Cranston",
                    lead("Walter White"),
                )]),
                ..Default::default()
            },
        );
    let ((service, _movies, show_repo, state_repo, _genres, _clock), people) =
        harness_with_people(provider);
    let show = show_repo
        .find_or_create_by_identity(CreateShow::new("Breaking Bad".to_string(), Some(2008)))
        .await
        .unwrap();
    state_repo
        .ensure_pending(EnrichmentTargetId::Show(show.id))
        .await
        .unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    let credits = people.show_credits(show.id).await.unwrap();
    assert_eq!(credits.len(), 1);
    assert_eq!(credits[0].person.name, "Bryan Cranston");
    assert_eq!(credits[0].role, lead("Walter White"));
}

fn matrix_collection() -> beam_domain::providers::enrichment::CollectionEnrichment {
    beam_domain::providers::enrichment::CollectionEnrichment {
        collection_ref: ExternalMediaRef::new("tmdb", "2344"),
        name: "The Matrix Collection".to_string(),
        poster_url: Some("https://image.tmdb.org/collection.jpg".to_string()),
        backdrop_url: None,
    }
}

/// The collection of a movie that was in `stored` once a match reporting
/// `reported` is applied to it, through a service writing to
/// `collections`.
async fn movie_collection_after_match(
    collections: Arc<dyn CollectionRepository>,
    stored: Option<beam_domain::providers::enrichment::CollectionEnrichment>,
    reported: Option<beam_domain::providers::enrichment::CollectionEnrichment>,
) -> (
    Option<beam_domain::models::collection::Collection>,
    Arc<InMemoryEnrichmentStateRepository>,
    uuid::Uuid,
) {
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "603"),
        title: "The Matrix".to_string(),
        original_title: None,
        year: Some(1999),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("The Matrix", vec![hit])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            collection: reported,
            ..Default::default()
        });
    let (service, movie_repo, _shows, state_repo, _genres, _clock) =
        harness_with_collections(provider, collections.clone());
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("The Matrix".to_string(), Some(1999), None))
        .await
        .unwrap();
    if let Some(stored) = &stored {
        collections
            .set_movie_collection(movie.id, Some(stored))
            .await
            .unwrap();
    }
    state_repo
        .ensure_pending(EnrichmentTargetId::Movie(movie.id))
        .await
        .unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    (
        collections.movie_collection(movie.id).await.unwrap(),
        state_repo,
        movie.id,
    )
}

#[tokio::test]
async fn a_movie_match_stores_its_collection() {
    let (collection, _, _) = movie_collection_after_match(
        Arc::new(InMemoryCollectionRepository::default()),
        None,
        Some(matrix_collection()),
    )
    .await;
    let collection = collection.expect("the movie's collection");
    assert_eq!(collection.external_ref, "tmdb:2344");
    assert_eq!(collection.name, "The Matrix Collection");
}

#[tokio::test]
async fn a_match_reporting_no_collection_takes_the_movie_out_of_its_old_one() {
    let (collection, _, _) = movie_collection_after_match(
        Arc::new(InMemoryCollectionRepository::default()),
        Some(matrix_collection()),
        None,
    )
    .await;
    assert_eq!(collection, None);
}

/// A collection store whose writes fail, its reads answered by the
/// in-memory store it wraps.
#[derive(Debug)]
struct FailingCollectionWrites(InMemoryCollectionRepository);

#[async_trait::async_trait]
impl CollectionRepository for FailingCollectionWrites {
    async fn set_movie_collection(
        &self,
        _: uuid::Uuid,
        _: Option<&beam_domain::providers::enrichment::CollectionEnrichment>,
    ) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(
            "collection store unavailable".to_string(),
        ))
    }
    async fn movie_collection(
        &self,
        movie_id: uuid::Uuid,
    ) -> Result<Option<beam_domain::models::collection::Collection>, sea_orm::DbErr> {
        self.0.movie_collection(movie_id).await
    }
    async fn find_by_id(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<beam_domain::models::collection::Collection>, sea_orm::DbErr> {
        self.0.find_by_id(id).await
    }
    async fn movie_ids(
        &self,
        id: uuid::Uuid,
        access: &beam_domain::models::access::AccessPolicy,
    ) -> Result<Vec<uuid::Uuid>, sea_orm::DbErr> {
        self.0.movie_ids(id, access).await
    }
    async fn list(
        &self,
        access: &beam_domain::models::access::AccessPolicy,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<beam_domain::models::collection::CollectionSummary>, sea_orm::DbErr> {
        self.0.list(access, limit, offset).await
    }
    async fn count(
        &self,
        access: &beam_domain::models::access::AccessPolicy,
    ) -> Result<u64, sea_orm::DbErr> {
        self.0.count(access).await
    }
}

#[tokio::test]
async fn a_movie_whose_collection_fails_to_store_is_enriched_all_the_same() {
    let (collection, state_repo, movie_id) = movie_collection_after_match(
        Arc::new(FailingCollectionWrites(
            InMemoryCollectionRepository::default(),
        )),
        None,
        Some(matrix_collection()),
    )
    .await;
    assert_eq!(collection, None, "the write failed, so nothing is stored");
    assert_enriched_as(&state_repo, EnrichmentTargetId::Movie(movie_id), "tmdb:603").await;
}

/// A person store whose credit writes fail, its reads answered by the
/// in-memory store it wraps.
#[derive(Debug)]
struct FailingCreditWrites(Arc<InMemoryPersonRepository>);

#[async_trait::async_trait]
impl PersonRepository for FailingCreditWrites {
    async fn set_movie_credits(
        &self,
        _: uuid::Uuid,
        _: &[CreditEnrichment],
    ) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(
            "credits store unavailable".to_string(),
        ))
    }
    async fn set_show_credits(
        &self,
        _: uuid::Uuid,
        _: &[CreditEnrichment],
    ) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Custom(
            "credits store unavailable".to_string(),
        ))
    }
    async fn movie_credits(
        &self,
        movie_id: uuid::Uuid,
    ) -> Result<Vec<beam_domain::models::person::TitleCredit>, sea_orm::DbErr> {
        self.0.movie_credits(movie_id).await
    }
    async fn show_credits(
        &self,
        show_id: uuid::Uuid,
    ) -> Result<Vec<beam_domain::models::person::TitleCredit>, sea_orm::DbErr> {
        self.0.show_credits(show_id).await
    }
    async fn find_by_id(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<beam_domain::models::person::Person>, sea_orm::DbErr> {
        self.0.find_by_id(id).await
    }
    async fn filmography(
        &self,
        person_id: uuid::Uuid,
        access: &beam_domain::models::access::AccessPolicy,
    ) -> Result<Vec<beam_domain::models::person::PersonCredit>, sea_orm::DbErr> {
        self.0.filmography(person_id, access).await
    }
    async fn search(
        &self,
        query: Option<&str>,
        access: &beam_domain::models::access::AccessPolicy,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<beam_domain::models::person::Person>, sea_orm::DbErr> {
        self.0.search(query, access, limit, offset).await
    }
    async fn count(
        &self,
        query: Option<&str>,
        access: &beam_domain::models::access::AccessPolicy,
    ) -> Result<u64, sea_orm::DbErr> {
        self.0.count(query, access).await
    }
}

fn failing_credit_writes(people: Arc<InMemoryPersonRepository>) -> Arc<dyn PersonRepository> {
    Arc::new(FailingCreditWrites(people))
}

/// The row enrichment leaves for `target`: enriched, and matched to
/// `matched`.
async fn assert_enriched_as(
    state_repo: &InMemoryEnrichmentStateRepository,
    target: EnrichmentTargetId,
    matched: &str,
) {
    let row = state_repo.find_by_target(target).await.unwrap().unwrap();
    assert_eq!(row.status, EnrichmentStatus::Enriched, "{row:?}");
    assert_eq!(row.matched_ref.as_deref(), Some(matched));
}

#[tokio::test]
async fn a_movie_whose_credits_fail_to_store_is_enriched_and_keeps_its_old_ones() {
    let hit = MovieSearchHit {
        external_ref: ExternalMediaRef::new("tmdb", "603"),
        title: "The Matrix".to_string(),
        original_title: None,
        year: Some(1999),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("The Matrix", vec![hit])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(603),
            title: "The Matrix".to_string(),
            year: Some(1999),
            credits: Some(vec![credit("6384", "Keanu Reeves", lead("Neo"))]),
            ..Default::default()
        });
    let ((service, movie_repo, _shows, state_repo, _genres, _clock), _events, people) =
        full_harness_storing_credits(provider, failing_credit_writes);
    let movie = movie_repo
        .find_or_create_by_identity(CreateMovie::new("The Matrix".to_string(), Some(1999), None))
        .await
        .unwrap();
    people
        .set_movie_credits(movie.id, &[credit("1", "Stale Credit", lead("Old"))])
        .await
        .unwrap();
    let target = EnrichmentTargetId::Movie(movie.id);
    state_repo.ensure_pending(target).await.unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    assert_enriched_as(&state_repo, target, "tmdb:603").await;
    let names: Vec<String> = people
        .movie_credits(movie.id)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.person.name)
        .collect();
    assert_eq!(names, ["Stale Credit"]);
}

#[tokio::test]
async fn a_show_whose_credits_fail_to_store_is_enriched_and_keeps_its_old_ones() {
    let show_ref = ExternalMediaRef::new("tmdb", "1396");
    let hit = ShowSearchHit {
        external_ref: show_ref.clone(),
        title: "Breaking Bad".to_string(),
        original_title: None,
        year: Some(2008),
        popularity: None,
        vote_average: None,
    };
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_show_search("Breaking Bad", vec![hit])
        .with_show_enrichment(
            show_ref,
            ShowEnrichment {
                tmdb_id: Some(1396),
                title: "Breaking Bad".to_string(),
                year: Some(2008),
                credits: Some(vec![credit(
                    "17419",
                    "Bryan Cranston",
                    lead("Walter White"),
                )]),
                ..Default::default()
            },
        );
    let ((service, _movies, show_repo, state_repo, _genres, _clock), _events, people) =
        full_harness_storing_credits(provider, failing_credit_writes);
    let show = show_repo
        .find_or_create_by_identity(CreateShow::new("Breaking Bad".to_string(), Some(2008)))
        .await
        .unwrap();
    people
        .set_show_credits(show.id, &[credit("1", "Stale Credit", lead("Old"))])
        .await
        .unwrap();
    let target = EnrichmentTargetId::Show(show.id);
    state_repo.ensure_pending(target).await.unwrap();

    assert_eq!(service.sweep_once().await.enriched, 1);
    assert_enriched_as(&state_repo, target, "tmdb:1396").await;
    let names: Vec<String> = people
        .show_credits(show.id)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.person.name)
        .collect();
    assert_eq!(names, ["Stale Credit"]);
}
