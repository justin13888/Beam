use super::*;
use crate::capability::builders::MediaSourceBuilder;
use crate::capability::select::{SubtitleOrigin, SubtitleTrackView};
use crate::ports::kv::{FailureMode, InMemoryKeyValueStore, KeyValueStore};
use proptest::prelude::*;
use std::sync::Arc;

use DownloadState::{Completed, Downloading, Failed, Paused, Queued, WaitingForNetwork};

const SERVER: &str = "server-a";

fn url(file_id: &str) -> Result<String, crate::error::BeamError> {
    Ok(format!("https://beam.test/v1/files/{file_id}/download"))
}

/// A plan of the given parts, each `(file_id, size)`, with sidecars named.
fn plan_of(server_id: &str, parts: &[(&str, Option<u64>)], sidecars: &[&str]) -> DownloadPlan {
    DownloadPlan {
        server_id: server_id.to_owned(),
        title_id: Some("title".to_owned()),
        file_id: parts[0].0.to_owned(),
        episode_id: None,
        parts: parts
            .iter()
            .map(|(file_id, size)| DownloadPart {
                file_id: (*file_id).to_owned(),
                part_number: None,
                download_url: url(file_id).expect("a url"),
                size_bytes: *size,
                duration_secs: None,
            })
            .collect(),
        subtitles: sidecars
            .iter()
            .map(|id| DownloadSubtitle {
                sidecar_id: (*id).to_owned(),
                part_file_id: parts[0].0.to_owned(),
                url: format!("https://beam.test/v1/files/x/subtitles/{id}"),
                codec: "subrip".to_owned(),
                language: Some("eng".to_owned()),
                title: None,
                is_default: false,
                is_forced: false,
                is_hearing_impaired: false,
            })
            .collect(),
    }
}

fn request(file_id: &str) -> DownloadRequest {
    DownloadRequest {
        media_id: "movie".to_owned(),
        episode_id: None,
        file_id: file_id.to_owned(),
        title: "Arrival".to_owned(),
        subtitle: None,
        artwork_path: Some("/data/artwork/abc".to_owned()),
    }
}

fn stored(plan: DownloadPlan, queued_at_unix: i64) -> StoredDownload {
    let file_id = plan.file_id.clone();
    StoredDownload::queued(request(&file_id), plan, queued_at_unix)
}

fn report(
    item_id: &str,
    state: DownloadState,
    downloaded: u64,
    total: Option<u64>,
) -> TransferReport {
    TransferReport {
        item_id: item_id.to_owned(),
        state,
        downloaded_bytes: downloaded,
        total_bytes: total,
        failure_message: None,
    }
}

/// Each row: the items' states, and the download's.
#[test]
fn a_downloads_state_is_decided_by_its_items() {
    let table: &[(&[DownloadState], DownloadState)] = &[
        (&[], Queued),
        (&[Queued], Queued),
        (&[Queued, Queued], Queued),
        (&[Downloading], Downloading),
        (&[Completed], Completed),
        (&[Completed, Completed], Completed),
        // Between two items of a sequential engine.
        (&[Completed, Queued], Downloading),
        (&[Completed, Downloading], Downloading),
        // What needs the viewer outranks what does not.
        (&[Completed, WaitingForNetwork], WaitingForNetwork),
        (&[Downloading, WaitingForNetwork], WaitingForNetwork),
        (&[WaitingForNetwork, Paused], Paused),
        (&[Downloading, Paused], Paused),
        (&[Paused, Failed], Failed),
        (&[Completed, Failed], Failed),
    ];
    for (items, expected) in table {
        assert_eq!(
            aggregate_state(items.iter().copied()),
            *expected,
            "items {items:?}"
        );
    }
}

/// Each row: downloaded, total, and the fraction.
#[test]
fn progress_is_unknown_until_a_total_is_and_clamped_after() {
    let table: &[(u64, Option<u64>, Option<f32>)] = &[
        (0, None, None),
        (500, None, None),
        (0, Some(0), None),
        (0, Some(1000), Some(0.0)),
        (250, Some(1000), Some(0.25)),
        (1000, Some(1000), Some(1.0)),
        (2000, Some(1000), Some(1.0)),
    ];
    for (downloaded, total, expected) in table {
        assert_eq!(
            progress_fraction(*downloaded, *total),
            *expected,
            "{downloaded} of {total:?}"
        );
    }
}

proptest! {
    /// No pair of byte counts puts a bar outside its track.
    #[test]
    fn progress_stays_within_zero_and_one(downloaded in any::<u64>(), total in any::<Option<u64>>()) {
        if let Some(fraction) = progress_fraction(downloaded, total) {
            prop_assert!((0.0..=1.0).contains(&fraction), "{fraction}");
        }
    }

    /// Nor does any sequence of reports on a record.
    #[test]
    fn a_records_progress_stays_within_zero_and_one(
        reports in proptest::collection::vec(
            (0_usize..3, 0_usize..6, any::<u64>(), any::<Option<u64>>()),
            0..12,
        ),
    ) {
        let states = [Queued, Downloading, Paused, WaitingForNetwork, Completed, Failed];
        let mut download = stored(plan_of(SERVER, &[("p1", Some(10)), ("p2", None)], &["s1"]), 0);
        for (item, state, downloaded, total) in reports {
            let item_id = ["p1", "p2", "s1"][item];
            download.apply(report(item_id, states[state], downloaded, total));
        }
        if let Some(progress) = download.to_record().progress {
            prop_assert!((0.0..=1.0).contains(&progress), "{progress}");
        }
    }
}

/// A queued record knows its size from the catalog before any byte moves,
/// so its bar is determinate from the start.
#[test]
fn a_queued_record_takes_its_total_from_the_plan() {
    let download = stored(
        plan_of(SERVER, &[("p1", Some(3000)), ("p2", Some(1000))], &[]),
        0,
    );

    let record = download.to_record();
    assert_eq!(record.state, Queued);
    assert_eq!(record.downloaded_bytes, 0);
    assert_eq!(record.total_bytes, Some(4000));
    assert_eq!(record.progress, Some(0.0));
    assert_eq!(record.server_id, SERVER);
    assert_eq!(record.artwork_path.as_deref(), Some("/data/artwork/abc"));
}

/// Parts' bytes sum; subtitles count toward completion but not toward the
/// bar, whose total they would leave unknown until they start.
#[test]
fn parts_bytes_make_the_bar_and_subtitles_only_hold_completion() {
    let mut download = stored(
        plan_of(SERVER, &[("p1", Some(3000)), ("p2", Some(1000))], &["s1"]),
        0,
    );
    assert!(download.apply(report("p1", Completed, 3000, Some(3000))));
    assert!(download.apply(report("p2", Downloading, 500, Some(1000))));
    let record = download.to_record();
    assert_eq!(record.state, Downloading);
    assert_eq!(record.downloaded_bytes, 3500);
    assert_eq!(record.progress, Some(0.875));

    download.apply(report("p2", Completed, 1000, Some(1000)));
    assert_eq!(
        download.to_record().state,
        Downloading,
        "the subtitle is still queued"
    );

    download.apply(report("s1", Completed, 40, Some(40)));
    let record = download.to_record();
    assert_eq!(record.state, Completed);
    assert_eq!(record.downloaded_bytes, 4000);
    assert_eq!(record.progress, Some(1.0));
}

/// A plan built without the catalog has no size until the engine reports
/// one, and the engine's figure replaces the catalog's once it does.
#[test]
fn an_unknown_size_is_indeterminate_until_the_engine_reports_one() {
    let mut download = stored(plan_of(SERVER, &[("p1", None)], &[]), 0);
    assert_eq!(download.to_record().progress, None);

    download.apply(report("p1", Downloading, 100, Some(400)));
    let record = download.to_record();
    assert_eq!(record.total_bytes, Some(400));
    assert_eq!(record.progress, Some(0.25));
}

/// A failure carries its item's message, or a stock one; a message sent with
/// any other state is dropped, so a retry does not keep explaining the
/// failure it recovered from.
#[test]
fn a_failure_message_belongs_to_a_failure() {
    let mut download = stored(plan_of(SERVER, &[("p1", Some(10))], &[]), 0);
    download.apply(TransferReport {
        failure_message: Some("The server refused the file.".to_owned()),
        ..report("p1", Failed, 0, None)
    });
    assert_eq!(
        download.to_record().failure_message.as_deref(),
        Some("The server refused the file.")
    );

    download.apply(TransferReport {
        failure_message: Some("stale".to_owned()),
        ..report("p1", Downloading, 1, None)
    });
    assert_eq!(download.to_record().failure_message, None);

    download.apply(report("p1", Failed, 1, None));
    assert!(
        download.to_record().failure_message.is_some(),
        "a stock message"
    );
}

/// A report repeated changes nothing, and one for an item the plan does not
/// name is ignored -- which is what lets the store skip a write.
#[test]
fn only_a_new_report_for_a_held_item_changes_a_download() {
    let mut download = stored(plan_of(SERVER, &[("p1", Some(10))], &[]), 0);
    assert!(download.apply(report("p1", Downloading, 1, Some(10))));
    assert!(!download.apply(report("p1", Downloading, 1, Some(10))));
    assert!(!download.apply(report("elsewhere", Completed, 10, Some(10))));
    assert_eq!(download.to_record().state, Downloading);
}

fn sidecar(id: &str) -> SubtitleTrackView {
    SubtitleTrackView {
        origin: SubtitleOrigin::Sidecar,
        index: None,
        sidecar_id: Some(id.to_owned()),
        codec: "subrip".to_owned(),
        language: Some("spa".to_owned()),
        title: None,
        is_default: false,
        is_forced: true,
        is_hearing_impaired: false,
        is_text: true,
        url: Some(format!("https://beam.test/v1/files/f/subtitles/{id}")),
        webvtt_url: None,
    }
}

fn embedded() -> SubtitleTrackView {
    SubtitleTrackView {
        origin: SubtitleOrigin::Embedded,
        index: Some(3),
        sidecar_id: None,
        url: None,
        ..sidecar("unused")
    }
}

/// Every part of the source holding the file, wherever in it the file is,
/// with each part's own sidecars and none of its embedded tracks.
#[test]
fn a_plan_names_every_part_and_sidecar_of_the_source_holding_the_file() {
    let mut source = MediaSourceBuilder::new("cd1")
        .parts(&[("cd1", 3000.0), ("cd2", 2800.0)])
        .build();
    source.parts[0].size_bytes = 300;
    source.parts[0].subtitle_tracks = vec![embedded()];
    source.parts[1].size_bytes = 200;
    source.parts[1].subtitle_tracks = vec![sidecar("sub2")];
    let other = MediaSourceBuilder::new("other").build();

    let plan = plan_download(
        SERVER,
        Some("title"),
        Some("ep"),
        "cd2",
        &[other, source],
        url,
    )
    .expect("every id resolves");

    let parts: Vec<(&str, Option<u32>, Option<u64>)> = plan
        .parts
        .iter()
        .map(|part| (part.file_id.as_str(), part.part_number, part.size_bytes))
        .collect();
    assert_eq!(
        parts,
        vec![("cd1", Some(1), Some(300)), ("cd2", Some(2), Some(200))]
    );
    assert_eq!(
        plan.parts[1].download_url,
        "https://beam.test/v1/files/cd2/download"
    );
    assert_eq!(plan.subtitles.len(), 1);
    assert_eq!(plan.subtitles[0].sidecar_id, "sub2");
    assert_eq!(plan.subtitles[0].part_file_id, "cd2");
    assert!(plan.subtitles[0].is_forced);
    assert_eq!(plan.file_id, "cd2");
    assert_eq!(plan.episode_id.as_deref(), Some("ep"));
    assert_eq!(
        plan.item_ids().collect::<Vec<_>>(),
        vec!["cd1", "cd2", "sub2"]
    );
}

/// A whole file is a plan of one part, sized from its source.
#[test]
fn a_whole_file_is_one_part_with_its_sidecars() {
    let mut source = MediaSourceBuilder::new("whole").size(1234).build();
    source.subtitle_tracks = vec![embedded(), sidecar("s1")];

    let plan = plan_download(SERVER, None, None, "whole", &[source], url).expect("plan");

    assert_eq!(plan.parts.len(), 1);
    assert_eq!(plan.parts[0].size_bytes, Some(1234));
    assert_eq!(plan.item_ids().collect::<Vec<_>>(), vec!["whole", "s1"]);
}

/// With no source holding the file, the plan is the file alone, unsized.
#[test]
fn a_file_no_source_holds_is_planned_alone() {
    let plan = plan_download(SERVER, Some("title"), None, "lone", &[], url).expect("plan");

    assert_eq!(plan.parts.len(), 1);
    assert_eq!(plan.parts[0].file_id, "lone");
    assert_eq!(plan.parts[0].size_bytes, None);
    assert!(plan.subtitles.is_empty());
}

/// A URL the server's origin refuses fails the plan rather than naming an
/// item the platform could not fetch.
#[test]
fn a_refused_url_fails_the_plan() {
    let refused = |_: &str| {
        Err(crate::error::BeamError::InvalidServerUrl {
            detail: "off origin".to_owned(),
        })
    };
    assert!(plan_download(SERVER, None, None, "f", &[], refused).is_err());
}

fn store_over(storage: &Arc<InMemoryKeyValueStore>) -> DownloadStore {
    DownloadStore::new(Arc::clone(storage) as Arc<dyn crate::ports::kv::KeyValueStore>)
}

/// Written through one call, read back through another -- and by a second
/// store over the same storage, which is what a restart is.
#[tokio::test]
async fn a_queued_download_reads_back_from_storage() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let store = store_over(&storage);
    store
        .insert(stored(plan_of(SERVER, &[("p1", Some(10))], &[]), 5))
        .await
        .expect("insert");

    let restarted = store_over(&storage);
    let all = restarted.all().await.expect("all");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].to_record().file_id, "p1");
    assert_eq!(all[0].to_record().title, "Arrival");
    assert!(storage.has_plain("downloads/server-a"));
}

/// Queueing a file a download already holds -- itself, or another part of
/// the same source -- answers that download and transfers nothing twice.
#[tokio::test]
async fn queueing_a_held_file_again_answers_the_download_holding_it() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let store = store_over(&storage);
    store
        .insert(stored(
            plan_of(SERVER, &[("cd1", Some(1)), ("cd2", Some(1))], &[]),
            1,
        ))
        .await
        .expect("insert");

    let again = store
        .insert(stored(plan_of(SERVER, &[("cd2", Some(1))], &[]), 2))
        .await
        .expect("insert");

    assert_eq!(again.plan.file_id, "cd1");
    assert_eq!(store.all().await.expect("all").len(), 1);
}

/// Reports land on the download holding each item, on whichever server, and
/// persist; the answer is the whole list, oldest first.
#[tokio::test]
async fn reports_persist_on_the_download_holding_each_item() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let store = store_over(&storage);
    store
        .insert(stored(plan_of("server-b", &[("b1", Some(10))], &[]), 2))
        .await
        .expect("insert");
    store
        .insert(stored(plan_of(SERVER, &[("a1", Some(10))], &[]), 1))
        .await
        .expect("insert");

    let answered = store
        .report(vec![
            report("b1", Completed, 10, Some(10)),
            report("a1", Downloading, 4, Some(10)),
            report("gone", Failed, 0, None),
        ])
        .await
        .expect("report");

    let states: Vec<(String, DownloadState)> = answered
        .iter()
        .map(|download| (download.plan.file_id.clone(), download.state()))
        .collect();
    assert_eq!(
        states,
        vec![("a1".to_owned(), Downloading), ("b1".to_owned(), Completed)]
    );
    let reread = store_over(&storage).all().await.expect("all");
    assert_eq!(reread, answered);
}

/// A report that changes nothing writes nothing, so an engine re-announcing
/// the same state does not cost a storage write.
#[tokio::test]
async fn an_unchanged_report_writes_nothing() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let store = store_over(&storage);
    store
        .insert(stored(plan_of(SERVER, &[("p1", Some(10))], &[]), 1))
        .await
        .expect("insert");
    store
        .report(vec![report("p1", Downloading, 1, Some(10))])
        .await
        .expect("report");

    storage.set_failure(FailureMode::FailWrites);
    store
        .report(vec![report("p1", Downloading, 1, Some(10))])
        .await
        .expect("a repeated report needs no write");
    assert!(
        store
            .report(vec![report("p1", Downloading, 2, Some(10))])
            .await
            .is_err(),
        "a changed one does"
    );
}

/// Removing one download answers it, with every item it held, and leaves
/// the rest; removing a server takes its downloads and no other server's.
#[tokio::test]
async fn removal_answers_what_was_removed_and_leaves_the_rest() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let store = store_over(&storage);
    for (server, file, sub, at) in [
        (SERVER, "a1", "sub1", 1),
        (SERVER, "a2", "sub2", 2),
        ("server-b", "b1", "sub3", 3),
    ] {
        store
            .insert(stored(plan_of(server, &[(file, Some(1))], &[sub]), at))
            .await
            .expect("insert");
    }

    let removed = store.remove("a2").await.expect("remove").expect("held");
    assert_eq!(
        removed.plan.item_ids().collect::<Vec<_>>(),
        vec!["a2", "sub2"]
    );
    assert_eq!(store.remove("a2").await.expect("remove"), None);

    let from_b = store
        .remove_server("server-b")
        .await
        .expect("remove_server");
    assert_eq!(from_b.len(), 1);
    assert!(!storage.has_plain("downloads/server-b"));
    let left: Vec<String> = store
        .all()
        .await
        .expect("all")
        .into_iter()
        .map(|download| download.plan.file_id)
        .collect();
    assert_eq!(left, vec!["a1".to_owned()]);

    let rest = store.remove_all().await.expect("remove_all");
    assert_eq!(rest.len(), 1);
    assert!(storage.is_empty());
}

/// Storage that yields before every call, as a platform store crossing the
/// FFI boundary does, so concurrent writers interleave between a read and
/// the write that follows it.
#[derive(Debug)]
struct YieldingStore(Arc<InMemoryKeyValueStore>);

#[async_trait::async_trait]
impl KeyValueStore for YieldingStore {
    async fn get(&self, key: String) -> Result<Option<String>, crate::error::StorageError> {
        tokio::task::yield_now().await;
        self.0.get(key).await
    }
    async fn put(&self, key: String, value: String) -> Result<(), crate::error::StorageError> {
        tokio::task::yield_now().await;
        self.0.put(key, value).await
    }
    async fn remove(&self, key: String) -> Result<(), crate::error::StorageError> {
        tokio::task::yield_now().await;
        self.0.remove(key).await
    }
    async fn list_keys(&self, prefix: String) -> Result<Vec<String>, crate::error::StorageError> {
        tokio::task::yield_now().await;
        self.0.list_keys(prefix).await
    }
    async fn get_secret(&self, key: String) -> Result<Option<String>, crate::error::StorageError> {
        self.0.get_secret(key).await
    }
    async fn put_secret(
        &self,
        key: String,
        value: String,
    ) -> Result<(), crate::error::StorageError> {
        self.0.put_secret(key, value).await
    }
    async fn remove_secret(&self, key: String) -> Result<(), crate::error::StorageError> {
        self.0.remove_secret(key).await
    }
}

/// Writes that overlap each land: two downloads queued at once on one
/// server are both kept, and a report racing a removal does not bring the
/// removed download back.
#[tokio::test]
async fn overlapping_writes_do_not_undo_each_other() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    let store = DownloadStore::new(Arc::new(YieldingStore(Arc::clone(&storage))));
    store
        .insert(stored(plan_of(SERVER, &[("gone", Some(4))], &[]), 1))
        .await
        .expect("insert");

    let (first, second, removed, reported) = tokio::join!(
        store.insert(stored(plan_of(SERVER, &[("a1", Some(1))], &[]), 2)),
        store.insert(stored(plan_of(SERVER, &[("a2", Some(1))], &[]), 3)),
        store.remove("gone"),
        store.report(vec![report("gone", Downloading, 2, Some(4))]),
    );
    first.expect("first insert");
    second.expect("second insert");
    removed.expect("remove").expect("held");
    reported.expect("report");

    let held: Vec<String> = store_over(&storage)
        .all()
        .await
        .expect("all")
        .into_iter()
        .map(|download| download.plan.file_id)
        .collect();
    assert_eq!(held, vec!["a1".to_owned(), "a2".to_owned()]);
}

/// An unreadable list reads as empty rather than failing every render.
#[tokio::test]
async fn an_unreadable_list_reads_as_empty() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    crate::ports::kv::KeyValueStore::put(
        storage.as_ref(),
        "downloads/server-a".to_owned(),
        "not json".to_owned(),
    )
    .await
    .expect("put");

    assert!(store_over(&storage).all().await.expect("all").is_empty());
}

/// A storage that cannot be read is an error, not an empty list: the
/// screen must not tell the viewer their downloads are gone.
#[tokio::test]
async fn a_storage_failure_is_an_error() {
    let storage = Arc::new(InMemoryKeyValueStore::new());
    storage.set_failure(FailureMode::FailReads);
    assert!(store_over(&storage).all().await.is_err());
}
