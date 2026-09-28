//! [`EnrichmentControl`] over in-memory stores and the in-memory provider:
//! what each administrator action changes, what it refuses, and that the
//! sweep then does what the action asked for.

use std::sync::Mutex;

use futures::FutureExt;

use super::*;
use crate::services::admin_log::LocalAdminLogService;
use crate::services::enrichment::MetadataEnrichmentService;
use crate::services::notification::InMemoryNotificationService;
use beam_domain::models::catalog::TitleKind;
use beam_domain::models::{AdminLog, CreateLibrary, CreateMovie, CreateShow};
use beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider;
use beam_domain::providers::enrichment::{
    ExternalMediaRef, MovieEnrichment, MovieSearchHit, ShowSearchHit,
};
use beam_domain::repositories::AdminLogRepository;
use beam_domain::repositories::admin_log::in_memory::InMemoryAdminLogRepository;
use beam_domain::repositories::enrichment::in_memory::InMemoryEnrichmentStateRepository;
use beam_domain::repositories::genre::in_memory::InMemoryGenreRepository;
use beam_domain::repositories::library::in_memory::InMemoryLibraryRepository;
use beam_domain::repositories::movie::in_memory::InMemoryMovieRepository;
use beam_domain::repositories::show::in_memory::InMemoryShowRepository;
use beam_domain::services::TestClock;

/// Records which titles the control asked to re-pin by their NFOs; the
/// re-pin itself is the indexer's, tested with it.
#[derive(Debug, Default)]
struct RecordingNfoPins {
    asked: Mutex<Vec<EnrichmentTargetId>>,
}

#[async_trait]
impl TitleNfoPins for RecordingNfoPins {
    async fn repin_from_nfos(&self, target: EnrichmentTargetId) -> Result<(), IndexError> {
        self.asked.lock().unwrap().push(target);
        Ok(())
    }
}

const ADMIN: &str = "admin-user-7";

struct Harness {
    control: EnrichmentControl,
    movies: Arc<InMemoryMovieRepository>,
    shows: Arc<InMemoryShowRepository>,
    states: Arc<InMemoryEnrichmentStateRepository>,
    libraries: Arc<InMemoryLibraryRepository>,
    admin_log: Arc<InMemoryAdminLogRepository>,
    nfo_pins: Arc<RecordingNfoPins>,
    worker: Arc<Notify>,
    provider: Arc<InMemoryEnrichmentProvider>,
}

impl Harness {
    fn new(provider: InMemoryEnrichmentProvider) -> Self {
        let movies = Arc::new(InMemoryMovieRepository::default());
        let shows = Arc::new(InMemoryShowRepository::default());
        let states = Arc::new(InMemoryEnrichmentStateRepository::over_titles(
            movies.clone(),
            shows.clone(),
        ));
        let libraries = Arc::new(InMemoryLibraryRepository::default());
        let admin_log = Arc::new(InMemoryAdminLogRepository::default());
        let nfo_pins = Arc::new(RecordingNfoPins::default());
        let worker = Arc::new(Notify::new());
        let provider = Arc::new(provider);
        let control = EnrichmentControl::new(EnrichmentControlDeps {
            movies: movies.clone(),
            shows: shows.clone(),
            states: states.clone(),
            libraries: libraries.clone(),
            provider: provider.clone(),
            admin_log: Arc::new(LocalAdminLogService::new(admin_log.clone())),
            nfo_pins: nfo_pins.clone(),
            worker: worker.clone(),
        });
        Self {
            control,
            movies,
            shows,
            states,
            libraries,
            admin_log,
            nfo_pins,
            worker,
            provider,
        }
    }

    async fn movie(&self, title: &str, year: Option<u32>) -> Uuid {
        let movie = self
            .movies
            .find_or_create_by_identity(CreateMovie::new(title.to_string(), year, None))
            .await
            .unwrap();
        self.states
            .ensure_pending(EnrichmentTargetId::Movie(movie.id))
            .await
            .unwrap();
        movie.id
    }

    async fn show(&self, title: &str) -> Uuid {
        let show = self
            .shows
            .find_or_create_by_identity(CreateShow::new(title.to_string(), None))
            .await
            .unwrap();
        self.states
            .ensure_pending(EnrichmentTargetId::Show(show.id))
            .await
            .unwrap();
        show.id
    }

    async fn state(&self, target: EnrichmentTargetId) -> EnrichmentState {
        self.states.find_by_target(target).await.unwrap().unwrap()
    }

    /// Whether the worker was poked since the last call.
    fn worker_poked(&self) -> bool {
        self.worker.notified().now_or_never().is_some()
    }

    async fn audit(&self) -> Vec<AdminLog> {
        self.admin_log.list(100, 0).await.unwrap()
    }

    /// The sweep, over the same stores and provider.
    fn sweep(&self) -> MetadataEnrichmentService {
        MetadataEnrichmentService::new(
            self.states.clone(),
            self.movies.clone(),
            self.shows.clone(),
            Arc::new(InMemoryGenreRepository::default()),
            self.provider.clone(),
            Arc::new(LocalAdminLogService::new(Arc::new(
                InMemoryAdminLogRepository::default(),
            ))),
            Arc::new(InMemoryNotificationService::new()),
            Arc::new(TestClock::new()),
        )
    }
}

fn movie_hit(provider_ref: &str, title: &str, year: Option<u32>) -> MovieSearchHit {
    let (provider, native) = provider_ref.split_once(':').unwrap();
    MovieSearchHit {
        external_ref: ExternalMediaRef::new(provider, native),
        title: title.to_string(),
        original_title: None,
        year,
        popularity: None,
        vote_average: None,
    }
}

// ── fix-match ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn fixing_a_match_pins_the_title_by_the_administrator_and_queues_it() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let id = h.movie("Heat", Some(1995)).await;
    let target = EnrichmentTargetId::Movie(id);
    let row = h.state(target).await.id;
    h.states
        .mark_enriched(row, "tmdb:1", 0.71, Utc::now())
        .await
        .unwrap();

    let detail = h
        .control
        .fix_match(id, Some("tmdb:949"), ADMIN)
        .await
        .unwrap();

    assert_eq!(detail.pinned_ref.as_deref(), Some("tmdb:949"));
    assert_eq!(detail.pin_source, Some(PinSource::Admin));
    assert_eq!(detail.status, EnrichmentStatus::Pending);
    assert_eq!(detail.matched_ref, None, "the wrong match is cleared");
    let movie = h.movies.find_by_id(id).await.unwrap().unwrap();
    assert_eq!(
        (movie.pinned_ref.as_deref(), movie.pin_source),
        (Some("tmdb:949"), Some(PinSource::Admin))
    );
    assert!(h.worker_poked(), "the sweep starts at once");
    let audit = h.audit().await;
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].category, AdminLogCategory::Enrichment);
    let details = audit[0].details.as_ref().unwrap();
    assert_eq!(details["admin_user_id"], ADMIN);
    assert_eq!(details["external_ref"], "tmdb:949");
}

#[tokio::test]
async fn the_sweep_fetches_a_fixed_title_by_the_chosen_id_at_full_confidence() {
    // The search would find another film: a fixed match is never searched.
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("Heat", vec![movie_hit("tmdb:1", "Heat", Some(1995))])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(949),
            title: "Heat".to_string(),
            year: Some(1995),
            ..Default::default()
        });
    let h = Harness::new(provider);
    let id = h.movie("Heat", Some(1995)).await;
    h.control
        .fix_match(id, Some("tmdb:949"), ADMIN)
        .await
        .unwrap();

    let report = h.sweep().sweep_once().await;

    assert_eq!(report.enriched, 1);
    let row = h.state(EnrichmentTargetId::Movie(id)).await;
    assert_eq!(row.matched_ref.as_deref(), Some("tmdb:949"));
    assert_eq!(row.match_confidence, Some(1.0));
    assert_eq!(
        h.movies.find_by_id(id).await.unwrap().unwrap().tmdb_id,
        Some(949)
    );
}

#[tokio::test]
async fn a_fixed_id_the_provider_does_not_have_is_left_unmatched_at_once() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let id = h.movie("Heat", Some(1995)).await;
    h.control
        .fix_match(id, Some("tmdb:999999"), ADMIN)
        .await
        .unwrap();

    let report = h.sweep().sweep_once().await;

    assert_eq!((report.unmatched, report.retrying), (1, 0));
    let row = h.state(EnrichmentTargetId::Movie(id)).await;
    assert_eq!(row.status, EnrichmentStatus::Unmatched);
    assert_eq!(row.attempts, 0, "a missing id is not retried");
    assert!(
        row.last_error.as_deref().unwrap().contains("tmdb:999999"),
        "{:?}",
        row.last_error
    );
}

#[tokio::test]
async fn a_fix_match_is_refused_for_what_cannot_be_pinned() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["anilist"]));
    let id = h.movie("Heat", Some(1995)).await;
    let other = h.movie("Other", None).await;
    h.control
        .fix_match(other, Some("anilist:5"), ADMIN)
        .await
        .unwrap();
    let _ = h.worker_poked();

    for (external_ref, refused) in [
        ("603", "no provider"),
        ("tmdb:0", "an id no provider issues"),
        ("trakt:12", "a provider Beam does not pin by"),
    ] {
        assert!(
            matches!(
                h.control.fix_match(id, Some(external_ref), ADMIN).await,
                Err(ControlError::InvalidExternalRef(_))
            ),
            "{refused}"
        );
    }
    assert!(matches!(
        h.control.fix_match(id, Some("tmdb:603"), ADMIN).await,
        Err(ControlError::ProviderNotConfigured(_))
    ));
    assert!(matches!(
        h.control.fix_match(id, Some("anilist:5"), ADMIN).await,
        Err(ControlError::ExternalRefTaken(_))
    ));
    assert!(matches!(
        h.control
            .fix_match(Uuid::new_v4(), Some("anilist:6"), ADMIN)
            .await,
        Err(ControlError::MediaNotFound(_))
    ));

    let movie = h.movies.find_by_id(id).await.unwrap().unwrap();
    assert_eq!(movie.pinned_ref, None, "a refusal changes nothing");
    assert!(!h.worker_poked());
    assert_eq!(h.audit().await.len(), 1, "only the fix that happened");
}

#[tokio::test]
async fn clearing_a_match_drops_the_administrators_pin_and_asks_the_nfo_again() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let id = h.show("The Office").await;
    let target = EnrichmentTargetId::Show(id);
    h.control
        .fix_match(id, Some("tmdb:2316"), ADMIN)
        .await
        .unwrap();

    let detail = h.control.fix_match(id, None, ADMIN).await.unwrap();

    assert_eq!((detail.pinned_ref, detail.pin_source), (None, None));
    assert_eq!(detail.status, EnrichmentStatus::Pending);
    assert_eq!(detail.matched_ref, None, "matched afresh");
    assert_eq!(*h.nfo_pins.asked.lock().unwrap(), vec![target]);
    assert!(h.worker_poked());
}

#[tokio::test]
async fn clearing_leaves_an_nfos_pin_to_the_nfo() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let id = h.movie("Heat", Some(1995)).await;
    h.movies
        .set_pinned_ref(id, &ProviderPin::Tmdb(949), PinSource::Nfo)
        .await
        .unwrap();

    let detail = h.control.fix_match(id, None, ADMIN).await.unwrap();

    assert_eq!(
        (detail.pinned_ref.as_deref(), detail.pin_source),
        (Some("tmdb:949"), Some(PinSource::Nfo))
    );
    assert!(
        h.nfo_pins.asked.lock().unwrap().is_empty(),
        "the NFO's pin never went"
    );
    assert_eq!(
        h.state(EnrichmentTargetId::Movie(id)).await.status,
        EnrichmentStatus::Pending,
        "still queued to be matched again"
    );
}

// ── candidates ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn candidates_come_best_first_from_the_titles_own_search() {
    let mut hits = vec![
        movie_hit("tmdb:1", "Heat Wave", Some(1990)),
        movie_hit("tmdb:949", "Heat", Some(1995)),
        movie_hit("tmdb:2", "Heat", Some(1972)),
    ];
    hits.extend((10..30).map(|n| movie_hit(&format!("tmdb:{n}"), "Unrelated", None)));
    let h =
        Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]).with_movie_search("Heat", hits));
    let id = h.movie("Heat", Some(1995)).await;

    let candidates = h.control.candidates(id, None, None).await.unwrap();

    assert_eq!(candidates.len(), MAX_CANDIDATES);
    assert_eq!(candidates[0].external_ref, "tmdb:949", "the year decides");
    assert_eq!(candidates[1].external_ref, "tmdb:2");
    assert!(
        candidates
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score),
        "best first"
    );
    assert!(candidates.iter().all(|c| (0.0..=1.0).contains(&c.score)));
}

#[tokio::test]
async fn a_query_the_administrator_types_replaces_the_titles_own() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"]).with_show_search(
        "Kaamelott",
        vec![ShowSearchHit {
            external_ref: ExternalMediaRef::new("tmdb", "1719"),
            title: "Kaamelott".to_string(),
            original_title: None,
            year: Some(2005),
            popularity: None,
            vote_average: None,
        }],
    );
    let h = Harness::new(provider);
    let id = h.show("kmlt s01").await;

    assert!(
        h.control
            .candidates(id, None, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        h.control
            .candidates(id, Some("  "), None)
            .await
            .unwrap()
            .is_empty(),
        "a blank query is the title's own"
    );
    let found = h
        .control
        .candidates(id, Some(" Kaamelott "), Some(2005))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].external_ref, "tmdb:1719");
}

#[tokio::test]
async fn a_search_no_provider_can_answer_is_refused_and_a_failing_one_reported() {
    let none = Harness::new(InMemoryEnrichmentProvider::new(&[]));
    let id = none.movie("Heat", None).await;
    assert!(matches!(
        none.control.candidates(id, None, None).await,
        Err(ControlError::ProviderNotConfigured(_))
    ));

    let failing = Harness::new(
        InMemoryEnrichmentProvider::new(&["tmdb"])
            .with_search_error(|| EnrichmentError::Transport("timed out".to_string())),
    );
    let id = failing.movie("Heat", None).await;
    assert!(matches!(
        failing.control.candidates(id, None, None).await,
        Err(ControlError::Provider(message)) if message.contains("timed out")
    ));
    assert!(matches!(
        failing.control.candidates(Uuid::new_v4(), None, None).await,
        Err(ControlError::MediaNotFound(_))
    ));
}

// ── locks ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn locks_are_set_whole_and_the_sweep_leaves_them_alone() {
    let provider = InMemoryEnrichmentProvider::new(&["tmdb"])
        .with_movie_search("Heat", vec![movie_hit("tmdb:949", "Heat", Some(1995))])
        .with_movie_enrichment(MovieEnrichment {
            tmdb_id: Some(949),
            title: "Heat (Provider)".to_string(),
            year: Some(1995),
            description: Some("A heist.".to_string()),
            genres: vec!["Crime".to_string()],
            ..Default::default()
        });
    let h = Harness::new(provider);
    let id = h.movie("Heat", Some(1995)).await;

    let detail = h
        .control
        .set_locks(id, &[MetadataField::Title, MetadataField::Genres], ADMIN)
        .await
        .unwrap();
    assert_eq!(
        detail.locked_fields,
        [MetadataField::Title, MetadataField::Genres]
            .into_iter()
            .collect::<FieldLocks>()
    );
    assert_eq!(
        h.audit().await[0].details.as_ref().unwrap()["admin_user_id"],
        ADMIN
    );

    let genres = Arc::new(InMemoryGenreRepository::default());
    let sweep = MetadataEnrichmentService::new(
        h.states.clone(),
        h.movies.clone(),
        h.shows.clone(),
        genres.clone(),
        h.provider.clone(),
        Arc::new(LocalAdminLogService::new(Arc::new(
            InMemoryAdminLogRepository::default(),
        ))),
        Arc::new(InMemoryNotificationService::new()),
        Arc::new(TestClock::new()),
    );
    assert_eq!(sweep.sweep_once().await.enriched, 1);

    let movie = h.movies.find_by_id(id).await.unwrap().unwrap();
    assert_eq!(movie.title, "Heat", "the locked title stands");
    assert_eq!(
        movie.description.as_deref(),
        Some("A heist."),
        "the rest is written"
    );
    assert!(
        genres.genres_for_movie(id).is_empty(),
        "the locked genres stand"
    );
}

#[tokio::test]
async fn a_field_a_show_does_not_have_cannot_be_locked_on_one() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let id = h.show("The Office").await;

    let refused = h
        .control
        .set_locks(
            id,
            &[
                MetadataField::ReleaseDate,
                MetadataField::Poster,
                MetadataField::Runtime,
            ],
            ADMIN,
        )
        .await;
    let Err(ControlError::FieldNotLockable(unlockable)) = refused else {
        panic!("a show has no release date or runtime to lock: {refused:?}");
    };
    assert_eq!(
        unlockable
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        vec![0, 2],
        "each refused field is named by its position"
    );
    assert!(unlockable[0].1.contains("release_date"));
    assert!(
        h.state(EnrichmentTargetId::Show(id))
            .await
            .locked_fields
            .is_empty(),
        "a refusal locks nothing"
    );
    assert!(matches!(
        h.control
            .set_locks(Uuid::new_v4(), &[MetadataField::Poster], ADMIN)
            .await,
        Err(ControlError::MediaNotFound(_))
    ));
}

// ── refresh ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_library_refresh_queues_that_librarys_titles_and_no_others() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let library = h
        .libraries
        .create(CreateLibrary {
            name: "Films".to_string(),
            root_path: "/films".into(),
            description: None,
        })
        .await
        .unwrap();
    let inside = h.movie("Heat", None).await;
    let show_inside = h.show("The Office").await;
    let outside = h.movie("Alien", None).await;
    h.movies
        .ensure_library_association(library.id, inside)
        .await
        .unwrap();
    h.shows
        .ensure_library_association(library.id, show_inside)
        .await
        .unwrap();
    for target in [
        EnrichmentTargetId::Movie(inside),
        EnrichmentTargetId::Show(show_inside),
        EnrichmentTargetId::Movie(outside),
    ] {
        let row = h.state(target).await.id;
        h.states.mark_failed(row, "boom", Utc::now()).await.unwrap();
    }

    let queued = h
        .control
        .refresh(RefreshScope::Library(library.id), false, ADMIN)
        .await
        .unwrap();

    assert_eq!(queued, 2);
    for target in [
        EnrichmentTargetId::Movie(inside),
        EnrichmentTargetId::Show(show_inside),
    ] {
        assert_eq!(h.state(target).await.status, EnrichmentStatus::Pending);
    }
    assert_eq!(
        h.state(EnrichmentTargetId::Movie(outside)).await.status,
        EnrichmentStatus::Failed
    );
    assert!(h.worker_poked());
    assert!(matches!(
        h.control
            .refresh(RefreshScope::Library(Uuid::new_v4()), false, ADMIN)
            .await,
        Err(ControlError::LibraryNotFound(_))
    ));
}

#[tokio::test]
async fn refreshing_all_or_one_queues_them_whatever_their_status() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let first = h.movie("Heat", None).await;
    let second = h.show("The Office").await;
    let row = h.state(EnrichmentTargetId::Movie(first)).await.id;
    h.states
        .mark_enriched(row, "tmdb:949", 0.9, Utc::now())
        .await
        .unwrap();

    assert_eq!(
        h.control
            .refresh(RefreshScope::Title(first), false, ADMIN)
            .await
            .unwrap(),
        1
    );
    let refreshed = h.state(EnrichmentTargetId::Movie(first)).await;
    assert_eq!(refreshed.status, EnrichmentStatus::Pending);
    assert!(refreshed.force_refresh);
    assert_eq!(
        refreshed.matched_ref.as_deref(),
        Some("tmdb:949"),
        "no rematch"
    );
    assert!(h.worker_poked());

    assert_eq!(
        h.control
            .refresh(RefreshScope::All, false, ADMIN)
            .await
            .unwrap(),
        2
    );
    assert!(
        h.state(EnrichmentTargetId::Show(second))
            .await
            .force_refresh
    );
    assert!(matches!(
        h.control
            .refresh(RefreshScope::Title(Uuid::new_v4()), false, ADMIN)
            .await,
        Err(ControlError::MediaNotFound(_))
    ));
    assert_eq!(h.audit().await.len(), 2);
}

// ── the list ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_list_names_each_title_and_says_whether_more_follow() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let movie = h.movie("Heat", Some(1995)).await;
    let show = h.show("The Office").await;
    let enriched = h.movie("Alien", Some(1979)).await;
    let base = Utc::now();
    let movie_row = h.state(EnrichmentTargetId::Movie(movie)).await.id;
    let show_row = h.state(EnrichmentTargetId::Show(show)).await.id;
    let enriched_row = h.state(EnrichmentTargetId::Movie(enriched)).await.id;
    h.states
        .mark_unmatched(movie_row, "no candidate", base)
        .await
        .unwrap();
    h.states
        .mark_unmatched(
            show_row,
            "no candidate",
            base + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    h.states
        .mark_enriched(enriched_row, "tmdb:348", 0.9, base)
        .await
        .unwrap();
    let unmatched = EnrichmentListFilter {
        status: Some(EnrichmentStatus::Unmatched),
        kind: None,
    };

    let first = h
        .control
        .list(unmatched, None, NonZeroU32::MIN)
        .await
        .unwrap();
    assert_eq!(first.total, 2);
    assert!(first.has_next_page);
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].title, "The Office");
    assert_eq!(first.items[0].target, EnrichmentTargetId::Show(show));
    assert_eq!(first.items[0].last_error.as_deref(), Some("no candidate"));

    let second = h
        .control
        .list(unmatched, first.items[0].position, NonZeroU32::MIN)
        .await
        .unwrap();
    assert!(!second.has_next_page);
    assert_eq!(second.items.len(), 1);
    assert_eq!(
        (second.items[0].title.as_str(), second.items[0].year),
        ("Heat", Some(1995))
    );

    let movies = h
        .control
        .list(
            EnrichmentListFilter {
                status: None,
                kind: Some(TitleKind::Movie),
            },
            None,
            NonZeroU32::new(10).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(movies.total, 2);
    assert!(!movies.has_next_page);
}
