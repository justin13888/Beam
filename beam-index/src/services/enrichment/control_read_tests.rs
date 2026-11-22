//! [`EnrichmentControl`]'s reads over the control tests' harness: one
//! title's detail, and the paged list of the queue.

use super::*;

// ── one title's detail ──────────────────────────────────────────────────────

/// A queued title's detail is its row: status, match, attempts and the
/// position it holds in the list, under the title's own name.
#[tokio::test]
async fn a_titles_detail_is_its_row_under_its_own_name() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let movie = h.movie("Heat", Some(1995)).await;
    let show = h.show("The Office").await;
    let target = EnrichmentTargetId::Movie(movie);
    let row = h.state(target).await.id;
    h.states
        .mark_enriched(row, "tmdb:949", 0.875, Utc::now())
        .await
        .unwrap();
    let stored = h.state(target).await;

    let detail = h.control.detail(movie).await.unwrap();

    assert_eq!(detail.target, target);
    assert_eq!((detail.title.as_str(), detail.year), ("Heat", Some(1995)));
    assert_eq!(detail.status, EnrichmentStatus::Enriched);
    assert_eq!(detail.matched_ref.as_deref(), Some("tmdb:949"));
    assert_eq!(detail.match_confidence, Some(0.875));
    assert_eq!(detail.attempts, stored.attempts);
    assert_eq!(detail.enriched_at, stored.enriched_at);
    assert_eq!(detail.updated_at, Some(stored.updated_at));
    assert_eq!(detail.position, Some(stored.list_position()));

    let show_detail = h.control.detail(show).await.unwrap();
    assert_eq!(show_detail.target, EnrichmentTargetId::Show(show));
    assert_eq!(show_detail.title, "The Office");
    assert_eq!(show_detail.status, EnrichmentStatus::Pending);
}

/// A title indexed before enrichment existed has no row yet: it reads as
/// pending with nothing tried, and holds no place in the list, until the
/// startup backfill queues it.
#[tokio::test]
async fn a_title_with_no_row_reads_as_pending_and_unplaced() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    let movie = h
        .movies
        .find_or_create_by_identity(CreateMovie::new("Alien".to_string(), Some(1979), None))
        .await
        .unwrap();

    let detail = h.control.detail(movie.id).await.unwrap();

    assert_eq!(detail.target, EnrichmentTargetId::Movie(movie.id));
    assert_eq!(detail.status, EnrichmentStatus::Pending);
    assert_eq!(detail.attempts, 0);
    assert_eq!(detail.matched_ref, None);
    assert_eq!(detail.updated_at, None);
    assert_eq!(detail.position, None);
    assert_eq!(detail.locked_fields, FieldLocks::none());
}

/// An id that names no title is refused as not found, and the refusal --
/// which the API hands a client as it is -- names the id asked for.
#[tokio::test]
async fn the_detail_of_an_unknown_id_is_not_found_and_names_it() {
    let h = Harness::new(InMemoryEnrichmentProvider::new(&["tmdb"]));
    h.movie("Heat", None).await;
    let unknown = Uuid::new_v4();

    let err = h.control.detail(unknown).await.unwrap_err();

    assert!(
        matches!(err, ControlError::MediaNotFound(id) if id == unknown),
        "{err:?}"
    );
    assert_eq!(err.to_string(), format!("media {unknown} not found"));
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
