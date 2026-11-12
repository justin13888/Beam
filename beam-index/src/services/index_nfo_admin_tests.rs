use super::*;

/// The administrator's control of enrichment (issue #185) over this harness's
/// indexer, which re-pins a title by its NFO once its administrator's pin is
/// cleared.
fn enrichment_control(h: &Harness) -> crate::services::enrichment::control::EnrichmentControl {
    use crate::services::enrichment::control::{EnrichmentControl, EnrichmentControlDeps};
    use beam_domain::providers::enrichment::test_utils::InMemoryEnrichmentProvider;

    EnrichmentControl::new(EnrichmentControlDeps {
        movies: h.movie_repo.clone(),
        shows: h.show_repo.clone(),
        states: h.enrichment_repo.clone(),
        libraries: Arc::new(InMemoryLibraryRepository::default()),
        provider: Arc::new(InMemoryEnrichmentProvider::new(&["tmdb"])),
        admin_log: h.admin_log_repo.clone() as Arc<dyn AdminLogRepository>,
        nfo_pins: h.service.clone(),
        worker: Arc::new(tokio::sync::Notify::new()),
    })
}

/// An administrator's fix-match is an administrator's pin (FR-312): it
/// outranks the NFO's, and neither an edit of the NFO nor a rescan replaces
/// it.
#[tokio::test]
async fn a_fixed_match_outranks_the_nfo_and_survives_its_edit_and_a_rescan() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(movie.pin_source, Some(PinSource::Nfo));

    enrichment_control(&h)
        .fix_match(movie.id, "tmdb:624860", "admin")
        .await
        .unwrap();
    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;
    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(
        (movie.pinned_ref.as_deref(), movie.pin_source),
        (Some("tmdb:624860"), Some(PinSource::Admin)),
        "the fixed match stands"
    );
}

/// Clearing an administrator's fix-match hands the title back to its NFO:
/// pinned by what the NFO says now, and queued to be fetched by that pin.
#[tokio::test]
async fn clearing_a_fixed_match_pins_the_title_by_its_nfo_again() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(movie.id, "tmdb:624860", "admin")
        .await
        .unwrap();

    let detail = control.clear_match(movie.id, "admin").await.unwrap();

    assert_eq!(
        (detail.pinned_ref.as_deref(), detail.pin_source),
        (Some("tmdb:603"), Some(PinSource::Nfo)),
        "the NFO's pin again"
    );
    let row = h.enrichment_of(EnrichmentTargetId::Movie(movie.id)).await;
    assert!(row.force_refresh);
    assert_eq!(
        row.matched_ref, None,
        "fetched by the NFO's pin, not a match"
    );
}

/// With no NFO to fall back to, a cleared title is found by its path again:
/// unpinned, and queued to be searched for.
#[tokio::test]
async fn clearing_a_fixed_match_of_a_title_with_no_nfo_unpins_it() {
    let h = Harness::new().await;
    h.video("Shogun/Shogun.S01E01.mkv");
    h.scan().await;
    let (show, _, _) = h.show_of("Shogun/Shogun.S01E01.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(show.id, "tmdb:126308", "admin")
        .await
        .unwrap();

    let detail = control.clear_match(show.id, "admin").await.unwrap();

    assert_eq!((detail.pinned_ref, detail.pin_source), (None, None));
    assert!(h.show_pins().iter().all(Option::is_none));
}

/// Clearing a fixed match while the NFO cannot be read -- a share offline
/// -- leaves the title unpinned, and the NFO forgotten as applied: when it
/// can be read again, the next scan pins the title by it, though its content
/// never changed.
#[tokio::test]
async fn an_nfo_unreadable_when_a_match_is_cleared_is_applied_when_it_returns() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    let nfo = h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(movie.id, "tmdb:624860", "admin")
        .await
        .unwrap();
    let away = h.root.join("matrix.nfo.offline");
    std::fs::rename(&nfo, &away).unwrap();

    let detail = control.clear_match(movie.id, "admin").await.unwrap();
    assert_eq!((detail.pinned_ref, detail.pin_source), (None, None));

    std::fs::rename(&away, &nfo).unwrap();
    h.scan().await;

    let movie = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(
        (movie.pinned_ref.as_deref(), movie.pin_source),
        (Some("tmdb:603"), Some(PinSource::Nfo)),
        "the NFO is applied once it can be read"
    );
}

/// An NFO edited while an administrator's pin held the title is what the
/// title goes back to when the pin is cleared: the edited id, not the one
/// the NFO held before.
#[tokio::test]
async fn clearing_a_fixed_match_takes_the_id_the_nfo_was_edited_to_meanwhile() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    let movie = h.movie_of("Matrix/matrix.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(movie.id, "tmdb:624860", "admin")
        .await
        .unwrap();
    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;

    let detail = control.clear_match(movie.id, "admin").await.unwrap();

    assert_eq!(
        (detail.pinned_ref.as_deref(), detail.pin_source),
        (Some("tmdb:604"), Some(PinSource::Nfo))
    );
}

/// Clearing a fixed match whose NFO names an id another title holds leaves
/// the title unpinned and the NFO forgotten as applied, so once the other
/// title lets the id go, the next scan pins the title by its NFO.
#[tokio::test]
async fn an_nfo_refused_when_a_match_is_cleared_is_applied_once_its_id_is_free() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.video("Heat/heat.mkv");
    h.scan().await;
    let matrix = h.movie_of("Matrix/matrix.mkv");
    let heat = h.movie_of("Heat/heat.mkv");
    let control = enrichment_control(&h);
    control
        .fix_match(matrix.id, "tmdb:624860", "admin")
        .await
        .unwrap();
    control
        .fix_match(heat.id, "tmdb:603", "admin")
        .await
        .unwrap();

    let detail = control.clear_match(matrix.id, "admin").await.unwrap();
    assert_eq!((detail.pinned_ref, detail.pin_source), (None, None));

    control
        .fix_match(heat.id, "tmdb:949", "admin")
        .await
        .unwrap();
    h.scan().await;

    let matrix = h.movie_of("Matrix/matrix.mkv");
    assert_eq!(
        (matrix.pinned_ref.as_deref(), matrix.pin_source),
        (Some("tmdb:603"), Some(PinSource::Nfo))
    );
}

/// A show's `tvshow.nfo` is the one a cleared show goes back to.
#[tokio::test]
async fn clearing_a_shows_fixed_match_pins_it_by_its_tvshow_nfo() {
    use beam_domain::models::PinSource;

    let h = Harness::new().await;
    h.video("The Office/Season 02/The.Office.S02E01.mkv");
    h.write(
        "The Office/tvshow.nfo",
        r#"<tvshow><uniqueid type="tmdb">2316</uniqueid></tvshow>"#,
    );
    h.scan().await;
    let (show, _, _) = h.show_of("The Office/Season 02/The.Office.S02E01.mkv");
    assert_eq!(show.pinned_ref.as_deref(), Some("tmdb:2316"));
    let control = enrichment_control(&h);
    control
        .fix_match(show.id, "tmdb:9999", "admin")
        .await
        .unwrap();

    let detail = control.clear_match(show.id, "admin").await.unwrap();

    assert_eq!(
        (detail.pinned_ref.as_deref(), detail.pin_source),
        (Some("tmdb:2316"), Some(PinSource::Nfo))
    );
}

/// An NFO edited while a scan holds the library is not read under it: the
/// event is handed back to retry (issue #181), and the retry -- once the
/// library is free -- applies the edit rather than losing it.
#[tokio::test]
async fn an_nfo_event_while_the_library_is_held_is_deferred_and_applied_on_retry() {
    let h = Harness::new().await;
    h.video("Matrix/matrix.mkv");
    h.write("Matrix/movie.nfo", MATRIX_NFO);
    h.scan().await;
    h.write(
        "Matrix/movie.nfo",
        r#"<movie><uniqueid type="tmdb">604</uniqueid></movie>"#,
    );

    let held = h
        .service
        .scans
        .try_acquire_for_reconcile(h.library.id)
        .expect("the library is free");
    assert_eq!(
        h.event_outcome("Matrix/movie.nfo", FsEventKind::Modified)
            .await,
        ReconcileOutcome::Deferred {
            retry_after: LIBRARY_BUSY_RETRY
        }
    );
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:603"),
        "nothing is re-pinned while the library is held"
    );

    drop(held);
    h.event("Matrix/movie.nfo", FsEventKind::Modified).await;
    assert_eq!(
        h.movie_of("Matrix/matrix.mkv").pinned_ref.as_deref(),
        Some("tmdb:604")
    );
}

#[tokio::test]
async fn a_row_from_older_rules_is_reclassified_by_the_nfo_beside_it() {
    let h = Harness::new().await;
    h.video("Lost/Pilot Part One.mkv");
    h.scan().await;
    assert!(matches!(
        h.file("Lost/Pilot Part One.mkv").content,
        Some(MediaFileContent::Movie { .. })
    ));

    // As a build before this NFO support left it.
    let id = h.file("Lost/Pilot Part One.mkv").id;
    h.file_repo
        .files
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .classifier_version = CLASSIFIER_VERSION - 1;
    h.write(
        "Lost/Pilot Part One.nfo",
        "<episodedetails><season>1</season><episode>1</episode></episodedetails>",
    );
    h.scan().await;

    let file = h.file("Lost/Pilot Part One.mkv");
    assert_eq!(file.id, id, "reclassified in place");
    assert_eq!(file.classifier_version, CLASSIFIER_VERSION);
    assert!(matches!(
        file.content,
        Some(MediaFileContent::Episode { .. })
    ));
}
