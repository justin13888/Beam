//! `AccessService` over the in-memory stores: which ids a policy hides, and
//! how a season, an episode or a file follows the title it belongs to.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

use beam_domain::models::access::{AccessPolicy, ContentRating, LibraryAccess};
use beam_domain::models::enrichment::FieldLocks;
use beam_domain::models::file::{CreateMediaFile, FileStatus, MediaFileContent};
use beam_domain::models::watch_state::TitleRef;
use beam_domain::models::{CreateEpisode, CreateMovie, CreateMovieEntry, CreateShow};
use beam_domain::providers::enrichment::{MovieEnrichment, ShowEnrichment};
use beam_domain::repositories::access_policy::in_memory::InMemoryAccessPolicyRepository;
use beam_domain::repositories::file::in_memory::InMemoryFileRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::repositories::{FileRepository, MovieRepository, ShowRepository};

use super::{AccessRepositories, AccessService};

struct Fixture {
    movies: Arc<InMemoryMovieRepository>,
    shows: Arc<InMemoryShowRepository>,
    files: Arc<InMemoryFileRepository>,
    service: AccessService,
}

/// A show the fixture made, and everything of it a route can be given.
struct MadeShow {
    show: Uuid,
    season: Uuid,
    episode: Uuid,
    file: Uuid,
}

impl Fixture {
    fn new() -> Self {
        let files = Arc::new(InMemoryFileRepository::default());
        let movies = Arc::new(InMemoryMovieRepository::with_files(files.clone()));
        let shows = Arc::new(InMemoryShowRepository::with_files(files.clone()));
        let service = AccessService::new(AccessRepositories {
            policies: Arc::new(InMemoryAccessPolicyRepository::default()),
            movies: movies.clone(),
            shows: shows.clone(),
            files: files.clone(),
        });
        Self {
            movies,
            shows,
            files,
            service,
        }
    }

    async fn file(&self, library_id: Uuid, content: Option<MediaFileContent>) -> Uuid {
        let unique = Uuid::new_v4();
        self.files
            .create(CreateMediaFile {
                library_id,
                path: PathBuf::from(format!("/media/{library_id}/{unique}.mkv")),
                hash: unique.as_u128() as u64,
                size_bytes: 1024,
                mtime: None,
                identity: None,
                mime_type: Some("video/x-matroska".to_string()),
                duration: None,
                container_format: Some("matroska".to_string()),
                // A file with no content is one the classifier could not
                // place, which is what `Unknown` records.
                status: if content.is_some() {
                    FileStatus::Known
                } else {
                    FileStatus::Unknown
                },
                content,
                classifier_version: 0,
                container_tags: None,
            })
            .await
            .unwrap()
            .id
    }

    /// A movie indexed in `library_id`, certified `certification`, with one
    /// file. Returns the movie and the file.
    async fn movie_in(&self, library_id: Uuid, certification: Option<&str>) -> (Uuid, Uuid) {
        let title = format!("movie {}", Uuid::new_v4());
        let movie = self
            .movies
            .find_or_create_by_identity(CreateMovie::new(title.clone(), None, None))
            .await
            .unwrap();
        self.movies
            .apply_enrichment(
                movie.id,
                &MovieEnrichment {
                    title,
                    content_rating: certification.map(str::to_string),
                    ..Default::default()
                },
                &FieldLocks::none(),
            )
            .await
            .unwrap();
        let entry = self
            .movies
            .find_or_create_entry(CreateMovieEntry {
                library_id,
                movie_id: movie.id,
                edition: None,
            })
            .await
            .unwrap();
        self.movies
            .ensure_library_association(library_id, movie.id)
            .await
            .unwrap();
        let file = self
            .file(library_id, Some(MediaFileContent::movie(entry.id)))
            .await;
        (movie.id, file)
    }

    /// A show indexed in `library_id`, certified `certification`, with one
    /// season of one episode that has a file.
    async fn show_in(&self, library_id: Uuid, certification: Option<&str>) -> MadeShow {
        let title = format!("show {}", Uuid::new_v4());
        let show = self
            .shows
            .find_or_create_by_identity(CreateShow::new(title.clone(), None))
            .await
            .unwrap();
        self.shows
            .apply_enrichment(
                show.id,
                &ShowEnrichment {
                    title,
                    content_rating: certification.map(str::to_string),
                    ..Default::default()
                },
                &FieldLocks::none(),
            )
            .await
            .unwrap();
        let season = self.shows.find_or_create_season(show.id, 1).await.unwrap();
        let episode = self
            .shows
            .find_or_create_episode(CreateEpisode {
                season_id: season.id,
                episode_number: 1,
                title: "Pilot".to_string(),
                runtime: None,
                air_date: None,
            })
            .await
            .unwrap();
        self.shows
            .ensure_library_association(library_id, show.id)
            .await
            .unwrap();
        let file = self
            .file(library_id, Some(MediaFileContent::episode(episode.id)))
            .await;
        MadeShow {
            show: show.id,
            season: season.id,
            episode: episode.id,
            file,
        }
    }
}

fn only(libraries: &[Uuid]) -> AccessPolicy {
    AccessPolicy {
        libraries: LibraryAccess::Only(libraries.iter().copied().collect()),
        max_rating: None,
    }
}

fn ceiling(band: ContentRating) -> AccessPolicy {
    AccessPolicy {
        libraries: LibraryAccess::All,
        max_rating: Some(band),
    }
}

#[tokio::test]
async fn a_grant_hides_every_part_of_a_title_outside_it_and_nothing_inside() {
    let fixture = Fixture::new();
    let (granted, other) = (Uuid::new_v4(), Uuid::new_v4());
    let (seen_movie, seen_file) = fixture.movie_in(granted, None).await;
    let seen_show = fixture.show_in(granted, None).await;
    let (hidden_movie, hidden_movie_file) = fixture.movie_in(other, None).await;
    let hidden_show = fixture.show_in(other, None).await;
    let policy = only(&[granted]);
    let service = &fixture.service;

    for id in [
        seen_movie,
        seen_show.show,
        seen_show.season,
        seen_show.episode,
    ] {
        assert!(!service.hides(&policy, id).await.unwrap(), "{id}");
    }
    for file in [seen_file, seen_show.file] {
        assert!(!service.hides_file(&policy, file).await.unwrap(), "{file}");
    }
    for id in [
        hidden_movie,
        hidden_show.show,
        hidden_show.season,
        hidden_show.episode,
    ] {
        assert!(service.hides(&policy, id).await.unwrap(), "{id}");
    }
    for file in [hidden_movie_file, hidden_show.file] {
        assert!(service.hides_file(&policy, file).await.unwrap(), "{file}");
    }

    // The same titles under no restriction at all.
    let open = AccessPolicy::unrestricted();
    assert!(!service.hides(&open, hidden_show.episode).await.unwrap());
    assert!(!service.hides_file(&open, hidden_movie_file).await.unwrap());
}

#[tokio::test]
async fn an_id_naming_nothing_is_not_hidden_so_the_route_answers_it_as_unknown() {
    let fixture = Fixture::new();
    let policy = only(&[]);

    assert!(
        !fixture
            .service
            .hides(&policy, Uuid::new_v4())
            .await
            .unwrap()
    );
    assert!(
        !fixture
            .service
            .hides_file(&policy, Uuid::new_v4())
            .await
            .unwrap()
    );
    assert_eq!(
        fixture.service.title_of(Uuid::new_v4()).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn a_ceiling_hides_a_title_above_it_or_with_no_certification_it_can_place() {
    let fixture = Fixture::new();
    let library = Uuid::new_v4();
    let (pg, _) = fixture.movie_in(library, Some("PG")).await;
    let (r, r_file) = fixture.movie_in(library, Some("R")).await;
    let (unrated, _) = fixture.movie_in(library, None).await;
    let (not_recognised, _) = fixture.movie_in(library, Some("NR")).await;
    let tv_pg = fixture.show_in(library, Some("TV-PG")).await;
    let tv_ma = fixture.show_in(library, Some("TV-MA")).await;
    let policy = ceiling(ContentRating::ParentalGuidance);
    let service = &fixture.service;

    assert!(!service.hides(&policy, pg).await.unwrap());
    assert!(!service.hides(&policy, tv_pg.episode).await.unwrap());
    for id in [r, unrated, not_recognised, tv_ma.show, tv_ma.season] {
        assert!(service.hides(&policy, id).await.unwrap(), "{id}");
    }
    assert!(service.hides_file(&policy, r_file).await.unwrap());
    assert!(service.hides_file(&policy, tv_ma.file).await.unwrap());
    assert!(!service.hides_file(&policy, tv_pg.file).await.unwrap());
}

#[tokio::test]
async fn visible_titles_keeps_only_the_admitted_ones_that_exist() {
    let fixture = Fixture::new();
    let (kids, grown) = (Uuid::new_v4(), Uuid::new_v4());
    let (kids_movie, _) = fixture.movie_in(kids, Some("G")).await;
    let (grown_movie, _) = fixture.movie_in(grown, Some("G")).await;
    let kids_show = fixture.show_in(kids, Some("TV-MA")).await;
    let gone = TitleRef::Movie(Uuid::new_v4());
    let titles = [
        TitleRef::Movie(kids_movie),
        TitleRef::Movie(grown_movie),
        TitleRef::Show(kids_show.show),
        gone,
    ];

    let policy = AccessPolicy {
        libraries: LibraryAccess::Only(BTreeSet::from([kids])),
        max_rating: Some(ContentRating::Teen),
    };
    let visible = fixture
        .service
        .visible_titles(&policy, &titles)
        .await
        .unwrap();
    assert_eq!(
        visible,
        [TitleRef::Movie(kids_movie)].into_iter().collect(),
        "the other library's movie, the mature show and the missing title go"
    );

    let open = fixture
        .service
        .visible_titles(&AccessPolicy::unrestricted(), &titles)
        .await
        .unwrap();
    assert_eq!(open.len(), titles.len(), "no restriction reads nothing");
}

#[tokio::test]
async fn a_file_no_title_holds_follows_its_library_and_is_hidden_under_any_ceiling() {
    let fixture = Fixture::new();
    let (granted, other) = (Uuid::new_v4(), Uuid::new_v4());
    let in_granted = fixture.file(granted, None).await;
    let in_other = fixture.file(other, None).await;
    let service = &fixture.service;

    assert!(
        !service
            .hides_file(&only(&[granted]), in_granted)
            .await
            .unwrap()
    );
    assert!(
        service
            .hides_file(&only(&[granted]), in_other)
            .await
            .unwrap()
    );
    assert!(
        service
            .hides_file(&ceiling(ContentRating::Adult), in_granted)
            .await
            .unwrap(),
        "nothing certifies an unclassified file"
    );
}

#[tokio::test]
async fn a_replaced_policy_is_the_one_read_back() {
    let fixture = Fixture::new();
    let user = Uuid::new_v4();
    let policy = AccessPolicy {
        libraries: LibraryAccess::Only(BTreeSet::from([Uuid::new_v4()])),
        max_rating: Some(ContentRating::Teen),
    };

    assert_eq!(
        fixture.service.policy(user).await.unwrap(),
        AccessPolicy::unrestricted()
    );
    fixture.service.replace(user, &policy).await.unwrap();
    assert_eq!(fixture.service.policy(user).await.unwrap(), policy);
}
