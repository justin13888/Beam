use super::*;

/// A scan relinks a moved video to its row (issue #180) before it judges
/// what sits beside the media: the row's subtitles are those beside its new
/// path, and its NFO's record moves with the NFO, the title keeping its pin.
#[tokio::test]
async fn a_scan_relinks_a_moved_video_with_its_subtitles_and_nfo_by_path() {
    let (h, id) = a_moved_movie_folder().await;

    h.scan().await;

    assert_eq!(
        h.file("Heat (1995)/Heat.mkv").id,
        id,
        "relinked, not re-added"
    );
    let subtitles: Vec<PathBuf> = h
        .subtitles_of("Heat (1995)/Heat.mkv")
        .await
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(subtitles, vec![h.root.join("Heat (1995)/Heat.en.srt")]);
    assert!(
        h.applied("Heat/Heat.nfo").await.is_none(),
        "the old NFO is gone"
    );
    assert!(h.applied("Heat (1995)/Heat.nfo").await.is_some());
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// A movie folder renamed whole, which the watcher reports as the directory
/// alone: its event relinks the video, records the subtitle beside it in
/// place of the one at the old path, and applies the NFO it holds, the title
/// keeping its pin.
#[tokio::test]
async fn a_renamed_movie_folders_event_carries_its_subtitles_and_nfo_along() {
    let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
    h.video("Heat/Heat.mkv");
    h.write("Heat/Heat.en.srt", "1");
    h.write("Heat/Heat.nfo", &tmdb_movie(949));
    h.scan().await;
    let id = h.file("Heat/Heat.mkv").id;

    std::fs::rename(h.root.join("Heat"), h.root.join("Heat (1995)")).unwrap();
    h.event("Heat (1995)", FsEventKind::Created).await;

    assert_eq!(
        h.file("Heat (1995)/Heat.mkv").id,
        id,
        "relinked, not re-added"
    );
    let subtitles: Vec<PathBuf> = h
        .subtitles_of("Heat (1995)/Heat.mkv")
        .await
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(subtitles, vec![h.root.join("Heat (1995)/Heat.en.srt")]);
    assert!(
        h.applied("Heat (1995)/Heat.nfo").await.is_some(),
        "the NFO inside is applied by the directory's event"
    );
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// Two copies of Heat in their own folders, indexed one scan after the other:
/// `One/`'s NFO pins the movie to tmdb:949, and `Two/`'s, naming tmdb:1, is
/// kept against it and recorded as applied (FR-219).
async fn a_kept_conflicting_nfo_in_its_own_folder() -> Harness {
    let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
    h.video("One/Heat (1995).mkv");
    h.write("One/Heat (1995).nfo", &tmdb_movie(949));
    h.scan().await;
    h.video("Two/Heat (1995).mkv");
    h.write("Two/Heat (1995).nfo", &tmdb_movie(1));
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    assert!(h.applied("Two/Heat (1995).nfo").await.is_some());
    h
}

/// A scan relinks the video of a kept NFO's moved folder (issue #180) and
/// carries the NFO's applied state with it: moved, not edited, it is not an
/// NFO added after indexing, and the title keeps its pin.
#[tokio::test]
async fn a_kept_nfos_folder_moved_keeps_the_pin_through_a_scan() {
    let h = a_kept_conflicting_nfo_in_its_own_folder().await;
    let id = h.file("Two/Heat (1995).mkv").id;

    std::fs::rename(h.root.join("Two"), h.root.join("Moved")).unwrap();
    h.scan().await;

    assert_eq!(h.file("Moved/Heat (1995).mkv").id, id, "relinked");
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    assert!(h.applied("Moved/Heat (1995).nfo").await.is_some());
    assert!(h.applied("Two/Heat (1995).nfo").await.is_none());
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// The watcher reports a renamed folder as the new directory and the old
/// one, in either order. Either way the relinked video carries its kept
/// NFO's applied state: before the old folder's removal forgets its record,
/// by that record; after, as classification applies an NFO a title never
/// had -- keeping the pin. A scan later agrees.
#[tokio::test]
async fn a_kept_nfos_folder_moved_keeps_the_pin_through_directory_events_in_either_order() {
    for created_first in [true, false] {
        let h = a_kept_conflicting_nfo_in_its_own_folder().await;
        let id = h.file("Two/Heat (1995).mkv").id;

        std::fs::rename(h.root.join("Two"), h.root.join("Moved")).unwrap();
        if created_first {
            h.event("Moved", FsEventKind::Created).await;
            h.event("Two", FsEventKind::Removed).await;
        } else {
            h.event("Two", FsEventKind::Removed).await;
            h.event("Moved", FsEventKind::Created).await;
        }

        assert_eq!(h.file("Moved/Heat (1995).mkv").id, id, "relinked");
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:949".to_string())],
            "created first: {created_first}"
        );
        assert!(h.applied("Moved/Heat (1995).nfo").await.is_some());
        assert!(h.applied("Two/Heat (1995).nfo").await.is_none());
        h.scan().await;
        assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    }
}

/// A kept NFO edited in the same move as its folder is an edited NFO: its
/// new id replaces the pin, by a scan and by the directory's event alike.
#[tokio::test]
async fn a_kept_nfo_edited_as_its_folder_moves_repins_its_title() {
    for by_scan in [true, false] {
        let h = a_kept_conflicting_nfo_in_its_own_folder().await;

        std::fs::rename(h.root.join("Two"), h.root.join("Moved")).unwrap();
        h.write("Moved/Heat (1995).nfo", &tmdb_movie(2));
        if by_scan {
            h.scan().await;
        } else {
            h.event("Moved", FsEventKind::Created).await;
            h.event("Two", FsEventKind::Removed).await;
        }

        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:2".to_string())],
            "by scan: {by_scan}"
        );
    }
}

/// A kept show NFO above a season folder moves with its show's folder: the
/// relinked episode finds the show NFO it had above its old season folder
/// by its record, and the show keeps its pin.
#[tokio::test]
async fn a_kept_show_nfos_folder_moved_keeps_the_shows_pin() {
    let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
    h.video("TV/Lost/Season 01/Lost - S01E01.mkv");
    h.write("TV/Lost/tvshow.nfo", &tmdb_show(4607));
    h.scan().await;
    h.video("Archive/Lost/Season 01/Lost - S01E02.mkv");
    h.write("Archive/Lost/tvshow.nfo", &tmdb_show(1));
    h.scan().await;
    assert_eq!(h.show_pins(), vec![Some("tmdb:4607".to_string())]);
    let id = h.file("Archive/Lost/Season 01/Lost - S01E02.mkv").id;

    std::fs::rename(h.root.join("Archive"), h.root.join("Old TV")).unwrap();
    h.scan().await;

    assert_eq!(
        h.file("Old TV/Lost/Season 01/Lost - S01E02.mkv").id,
        id,
        "relinked"
    );
    assert!(h.applied("Old TV/Lost/tvshow.nfo").await.is_some());
    assert_eq!(h.show_pins(), vec![Some("tmdb:4607".to_string())]);
}

/// A kept NFO whose folder Beam saw removed -- by the directory's own
/// removal event, or by a scan while it was missing -- is forgotten with it,
/// exactly as a removed NFO's own event forgets it: put back, it is an NFO
/// added after indexing, and re-pins its title (FR-219).
#[tokio::test]
async fn a_kept_nfos_folder_seen_removed_and_put_back_repins_its_title() {
    for by_scan in [true, false] {
        let h = a_kept_conflicting_nfo_in_its_own_folder().await;
        let away = h.root.parent().unwrap().join("Two, parked");

        std::fs::rename(h.root.join("Two"), &away).unwrap();
        if by_scan {
            h.scan().await;
        } else {
            h.event("Two", FsEventKind::Removed).await;
        }
        assert!(
            h.applied("Two/Heat (1995).nfo").await.is_none(),
            "forgotten with its folder, by scan: {by_scan}"
        );
        std::fs::rename(&away, h.root.join("Two")).unwrap();
        if by_scan {
            h.scan().await;
        } else {
            h.event("Two", FsEventKind::Created).await;
        }

        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:1".to_string())],
            "by scan: {by_scan}"
        );
    }
}

/// A video moved into a folder whose NFO it never had -- its old folder held
/// none -- is pinned by that NFO as classification would pin a new file's,
/// and the NFO recorded as applied.
#[tokio::test]
async fn a_video_moved_beside_an_nfo_it_never_had_is_pinned_by_it() {
    let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
    h.video("Incoming/Alien (1979).mkv");
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![None]);
    let id = h.file("Incoming/Alien (1979).mkv").id;

    move_file(
        &h,
        "Incoming/Alien (1979).mkv",
        "Alien (1979)/Alien (1979).mkv",
    );
    h.write("Alien (1979)/Alien (1979).nfo", &tmdb_movie(348));
    h.event("Alien (1979)/Alien (1979).mkv", FsEventKind::Created)
        .await;

    assert_eq!(h.file("Alien (1979)/Alien (1979).mkv").id, id, "relinked");
    assert_eq!(h.movie_pins(), vec![Some("tmdb:348".to_string())]);
    assert!(h.applied("Alien (1979)/Alien (1979).nfo").await.is_some());
}

/// An NFO a moved video finds beside its new path that its title never had
/// applied is a new file's NFO to that title: one naming another id than
/// the title's pin is kept against it, and the administrator told -- not
/// taken for an edit that replaces the pin.
#[tokio::test]
async fn a_video_moved_beside_a_conflicting_nfo_it_never_had_keeps_the_pin() {
    let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
    h.video("One/Heat (1995).mkv");
    h.write("One/Heat (1995).nfo", &tmdb_movie(949));
    h.video("Incoming/Heat (1995).mkv");
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

    move_file(&h, "Incoming/Heat (1995).mkv", "Two/Heat (1995).mkv");
    h.write("Two/Heat (1995).nfo", &tmdb_movie(1));
    h.scan().await;

    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    assert!(h.applied("Two/Heat (1995).nfo").await.is_some());
    assert!(
        h.warnings()
            .await
            .iter()
            .any(|w| w.contains("tmdb:1") && w.contains("tmdb:949")),
        "the administrator is told the pin was kept: {:?}",
        h.warnings().await
    );
}

/// A video moved alone, away from a folder NFO that stays where it was, did
/// not take that NFO with it: the conflicting NFO beside its new path is not
/// an edit of the one it left, and the title keeps its pin.
#[tokio::test]
async fn a_video_moved_away_from_an_nfo_that_stays_keeps_the_pin() {
    let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
    h.video("One/Heat (1995).mkv");
    h.video("One/Heat (1995) - Remux.mkv");
    h.write("One/movie.nfo", &tmdb_movie(949));
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

    move_file(
        &h,
        "One/Heat (1995) - Remux.mkv",
        "Two/Heat (1995) - Remux.mkv",
    );
    h.write("Two/movie.nfo", &tmdb_movie(1));
    h.scan().await;

    assert!(h.applied("One/movie.nfo").await.is_some(), "still there");
    assert!(h.applied("Two/movie.nfo").await.is_some());
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// A copy of Heat with its own NFO in each of `folders`, each indexed by a
/// scan of its own in turn: the first folder's NFO, naming tmdb:949, pins
/// the movie, and every later one -- naming tmdb:1, tmdb:2, ... -- is kept
/// against it and recorded as applied (FR-219). Each copy has its own size:
/// two same-size copies written within one filesystem timestamp tick would
/// stat alike, and a scan does not re-hash a file whose size and
/// modification time match its row.
async fn kept_conflicting_nfos_in(folders: &[&str]) -> Harness {
    let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
    for (i, folder) in folders.iter().enumerate() {
        let id = if i == 0 { 949 } else { i as u32 };
        h.write(&format!("{folder}/Heat (1995).mkv"), &"v".repeat(16 + i));
        h.write(&format!("{folder}/Heat (1995).nfo"), &tmdb_movie(id));
        h.scan().await;
    }
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    for folder in folders {
        assert!(
            h.applied(&format!("{folder}/Heat (1995).nfo"))
                .await
                .is_some()
        );
    }
    h
}

/// Rotate whole folders beneath the harness's root: the contents of
/// `folders[i]` end up in `folders[i + 1]`, and the last folder's in the
/// first. Two folders are swapped.
fn rotate_folders(h: &Harness, folders: &[&str]) {
    let parked = h.root.parent().unwrap().join("parked");
    let last = folders.len() - 1;
    std::fs::rename(h.root.join(folders[last]), &parked).unwrap();
    for i in (0..last).rev() {
        std::fs::rename(h.root.join(folders[i]), h.root.join(folders[i + 1])).unwrap();
    }
    std::fs::rename(&parked, h.root.join(folders[0])).unwrap();
}

/// Each folder's video row and the content hash of its NFO's record.
async fn rows_and_nfo_records(h: &Harness, folders: &[&str]) -> Vec<(Uuid, String)> {
    let mut found = Vec::new();
    for folder in folders {
        let id = h.file(&format!("{folder}/Heat (1995).mkv")).id;
        let record = h
            .applied(&format!("{folder}/Heat (1995).nfo"))
            .await
            .expect("the NFO is recorded");
        found.push((id, record.content_hash));
    }
    found
}

/// Folders swapped or rotated whole carry each kept NFO's applied state as
/// a move does (FR-219, issue #180): every row relinks with its NFO's
/// record, and the kept, conflicting NFOs stay kept -- through the scan
/// that finds the change and every scan after it -- whichever way the
/// folders' names sort.
#[tokio::test]
async fn folders_swapped_or_rotated_keep_the_pin_whichever_way_they_sort() {
    let cases: [&[&str]; 4] = [&["A", "B"], &["B", "A"], &["A", "B", "C"], &["C", "B", "A"]];
    for folders in cases {
        let h = kept_conflicting_nfos_in(folders).await;
        let before = rows_and_nfo_records(&h, folders).await;

        rotate_folders(&h, folders);
        h.scan().await;

        let after = rows_and_nfo_records(&h, folders).await;
        let mut expected = before.clone();
        expected.rotate_right(1);
        assert_eq!(
            after, expected,
            "each row and its NFO's record follow the folder, {folders:?}"
        );
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:949".to_string())],
            "{folders:?}"
        );
        h.scan().await;
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:949".to_string())],
            "{folders:?}"
        );
    }
}

/// Two folders swapped, one of whose NFOs was also edited on the way: that
/// NFO holds content no record does, so it is an edited NFO and replaces
/// the pin with the id it now names -- whether it is the pinning NFO or the
/// kept one -- while the other still carries its record.
#[tokio::test]
async fn folders_swapped_with_one_nfo_edited_repin_to_the_edit() {
    for edited in ["A", "B"] {
        let folders = ["A", "B"];
        let h = kept_conflicting_nfos_in(&folders).await;
        let kept_record = h.applied("B/Heat (1995).nfo").await.unwrap().content_hash;
        let pinning_record = h.applied("A/Heat (1995).nfo").await.unwrap().content_hash;

        rotate_folders(&h, &folders);
        h.write(&format!("{edited}/Heat (1995).nfo"), &tmdb_movie(2));
        h.scan().await;

        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:2".to_string())],
            "edited in {edited}"
        );
        let carried = if edited == "A" {
            ("B/Heat (1995).nfo", pinning_record)
        } else {
            ("A/Heat (1995).nfo", kept_record)
        };
        assert_eq!(
            h.applied(carried.0).await.unwrap().content_hash,
            carried.1,
            "the other NFO carries its record, edited in {edited}"
        );
    }
}

/// Two folders swapped through a third name, as the watcher reports it: the
/// third name's removal, then each folder's directory event, in either
/// order. Each folder's video holds another row's content and is left to
/// the scan, and so is the NFO beside it: the watcher cannot yet tell a
/// moved NFO from an edited one. So the kept NFO stays kept -- whichever
/// folder held the pinning NFO, and whichever event comes first -- and the
/// scan then carries each NFO with its video (FR-219).
#[tokio::test]
async fn folders_swapped_under_the_watcher_leave_their_nfos_to_the_scan_and_keep_the_pin() {
    for folders in [["A", "B"], ["B", "A"]] {
        for events in [["A", "B"], ["B", "A"]] {
            let h = kept_conflicting_nfos_in(&folders).await;
            let before = rows_and_nfo_records(&h, &folders).await;

            std::fs::rename(h.root.join("A"), h.root.join("T")).unwrap();
            std::fs::rename(h.root.join("B"), h.root.join("A")).unwrap();
            std::fs::rename(h.root.join("T"), h.root.join("B")).unwrap();
            h.event("T", FsEventKind::Removed).await;
            for folder in events {
                h.event(folder, FsEventKind::Created).await;
            }

            assert_eq!(
                h.movie_pins(),
                vec![Some("tmdb:949".to_string())],
                "after the events, pinned by {folders:?}, events {events:?}"
            );
            assert_eq!(
                rows_and_nfo_records(&h, &folders).await,
                before,
                "rows and NFO records left for the scan, {folders:?}, {events:?}"
            );
            h.scan().await;
            let mut expected = before.clone();
            expected.reverse();
            assert_eq!(
                rows_and_nfo_records(&h, &folders).await,
                expected,
                "each row and its NFO's record follow the folder, {folders:?}, {events:?}"
            );
            assert_eq!(
                h.movie_pins(),
                vec![Some("tmdb:949".to_string())],
                "after the scan, pinned by {folders:?}, events {events:?}"
            );
        }
    }
}

/// Two folders swapped with one NFO edited on the way, as a watcher that
/// reports each file hears it, the NFOs first: the edited NFO holds content
/// no record does, but the video it describes is no longer the file its row
/// records, so it too is left to the scan rather than applied -- which would
/// overwrite the record that shows the other NFO moved. The scan then
/// carries the other NFO and replaces the pin with the edit, as a scan alone
/// does.
#[tokio::test]
async fn folders_swapped_with_one_nfo_edited_under_per_file_events_repin_to_the_edit() {
    for edited in ["A", "B"] {
        let folders = ["A", "B"];
        let other = if edited == "A" { "B" } else { "A" };
        let h = kept_conflicting_nfos_in(&folders).await;

        rotate_folders(&h, &folders);
        h.write(&format!("{edited}/Heat (1995).nfo"), &tmdb_movie(2));
        for rel in [
            format!("{edited}/Heat (1995).nfo"),
            format!("{other}/Heat (1995).nfo"),
            format!("{edited}/Heat (1995).mkv"),
            format!("{other}/Heat (1995).mkv"),
        ] {
            h.event(&rel, FsEventKind::Created).await;
        }
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:949".to_string())],
            "left for the scan, edited in {edited}"
        );

        h.scan().await;
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:2".to_string())],
            "edited in {edited}"
        );
    }
}

/// A video moved into a new folder while another took its old name is left
/// to the scan by the new folder's event, and so is the NFO beside it -- one
/// its title never had, describing no indexed file yet. Applied then, it
/// would be recorded having pinned nothing; left, the scan relinks the
/// video and applies the NFO as a new file's, pinning the title.
#[tokio::test]
async fn an_nfo_beside_a_video_left_to_the_scan_is_applied_by_the_scan() {
    let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
    h.write("One/Heat (1995).mkv", "one");
    h.write("Two/Heat (1995).mkv", "two, a longer copy");
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![None]);
    let id = h.file("Two/Heat (1995).mkv").id;

    move_file(&h, "Two/Heat (1995).mkv", "New/Heat (1995).mkv");
    move_file(&h, "One/Heat (1995).mkv", "Two/Heat (1995).mkv");
    h.write("New/Heat (1995).nfo", &tmdb_movie(5));
    h.event("New", FsEventKind::Created).await;

    assert!(
        h.applied("New/Heat (1995).nfo").await.is_none(),
        "left to the scan with its video"
    );
    h.scan().await;
    assert_eq!(h.file("New/Heat (1995).mkv").id, id, "relinked");
    assert_eq!(h.movie_pins(), vec![Some("tmdb:5".to_string())]);
}

/// A kept NFO moved with its video file by file, its own event heard before
/// the video's: it holds what its old path's record holds, and the file
/// there is gone, so the watcher does not take it for a new NFO and replace
/// the pin with it. The video's event then relinks the video and carries
/// the NFO's record with it.
#[tokio::test]
async fn a_kept_nfos_event_heard_before_its_moved_videos_keeps_the_pin() {
    let h = a_kept_conflicting_nfo_in_its_own_folder().await;
    let id = h.file("Two/Heat (1995).mkv").id;

    move_file(&h, "Two/Heat (1995).mkv", "Moved/Heat (1995).mkv");
    move_file(&h, "Two/Heat (1995).nfo", "Moved/Heat (1995).nfo");
    h.event("Moved/Heat (1995).nfo", FsEventKind::Created).await;

    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    assert!(
        h.applied("Moved/Heat (1995).nfo").await.is_none(),
        "left for its video's relink"
    );

    h.event("Moved/Heat (1995).mkv", FsEventKind::Created).await;
    h.event("Two/Heat (1995).mkv", FsEventKind::Removed).await;
    h.event("Two/Heat (1995).nfo", FsEventKind::Removed).await;

    assert_eq!(h.file("Moved/Heat (1995).mkv").id, id, "relinked");
    assert!(h.applied("Moved/Heat (1995).nfo").await.is_some());
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    h.scan().await;
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// The same move with a link left at each old path, pointing at the moved
/// file (issue #246): a link is no file of the library, so each old path is
/// vacated as if nothing were there. The NFO's event leaves it to its video's
/// relink rather than taking it for a new NFO, and the video's event relinks
/// its row -- the link at the row's old path is no copy of it -- carrying the
/// NFO's record along.
#[cfg(unix)]
#[tokio::test]
async fn a_kept_nfo_and_its_video_moved_with_links_left_behind_keep_the_pin() {
    let h = a_kept_conflicting_nfo_in_its_own_folder().await;
    let id = h.file("Two/Heat (1995).mkv").id;

    for (from, to) in [
        ("Two/Heat (1995).mkv", "Moved/Heat (1995).mkv"),
        ("Two/Heat (1995).nfo", "Moved/Heat (1995).nfo"),
    ] {
        move_file(&h, from, to);
        std::os::unix::fs::symlink(h.root.join(to), h.root.join(from)).unwrap();
    }
    h.event("Moved/Heat (1995).nfo", FsEventKind::Created).await;

    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
    assert!(
        h.applied("Moved/Heat (1995).nfo").await.is_none(),
        "left for its video's relink"
    );

    h.event("Moved/Heat (1995).mkv", FsEventKind::Created).await;

    assert_eq!(h.file("Moved/Heat (1995).mkv").id, id, "relinked");
    assert!(h.applied("Moved/Heat (1995).nfo").await.is_some());
    assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
}

/// A kept NFO's folder moved with a link to its new place left at the old
/// one (issue #246): every old path is now reached through a link, so each
/// is vacated. The directory's event relinks the video and carries the NFO's
/// record, keeping the pin -- and an NFO edited in that move is still an
/// edited NFO, whose new id replaces the pin.
#[cfg(unix)]
#[tokio::test]
async fn a_kept_nfos_folder_moved_with_a_link_left_behind_is_judged_as_a_move() {
    for edited in [false, true] {
        let h = a_kept_conflicting_nfo_in_its_own_folder().await;
        let id = h.file("Two/Heat (1995).mkv").id;

        std::fs::rename(h.root.join("Two"), h.root.join("Moved")).unwrap();
        std::os::unix::fs::symlink(h.root.join("Moved"), h.root.join("Two")).unwrap();
        if edited {
            h.write("Moved/Heat (1995).nfo", &tmdb_movie(2));
        }
        h.event("Moved", FsEventKind::Created).await;

        assert_eq!(
            h.file("Moved/Heat (1995).mkv").id,
            id,
            "relinked, edited: {edited}"
        );
        let pin = if edited { "tmdb:2" } else { "tmdb:949" };
        assert_eq!(
            h.movie_pins(),
            vec![Some(pin.to_string())],
            "edited: {edited}"
        );
    }
}

/// Move each `(from, to)` video on disk, and return the batch a relink of
/// them hands [`LocalIndexService::carry_nfos_on_relink`]: each row's old
/// path, and the row at its new one.
fn move_videos(h: &Harness, moves: &[(&str, &str)]) -> Vec<(PathBuf, MediaFile)> {
    moves
        .iter()
        .map(|(from, to)| {
            let row = h.file(from);
            move_file(h, from, to);
            (
                h.root.join(from),
                MediaFile {
                    path: h.root.join(to),
                    ..row
                },
            )
        })
        .collect()
}

/// A `movie.nfo` two relinked videos share -- moved from one video's old
/// folder and edited on the way, beside another video that had no NFO -- is
/// judged over both of them, never for whichever the batch holds first
/// (FR-219): edited in the move in either order, so it is left for the
/// re-apply, which replaces the pin with it.
#[tokio::test]
async fn a_shared_movie_nfo_edited_in_a_move_is_judged_the_same_in_either_batch_order() {
    for reversed in [false, true] {
        let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
        h.video("P/Heat (1995).mkv");
        h.write("P/movie.nfo", &tmdb_movie(949));
        h.video("Q/Heat (1995) - Remux.mkv");
        h.scan().await;
        assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);

        let mut batch = move_videos(
            &h,
            &[
                ("P/Heat (1995).mkv", "N/Heat (1995) - X.mkv"),
                ("Q/Heat (1995) - Remux.mkv", "N/Heat (1995) - Y.mkv"),
            ],
        );
        move_file(&h, "P/movie.nfo", "N/movie.nfo");
        h.write("N/movie.nfo", &tmdb_movie(2));
        if reversed {
            batch.reverse();
        }
        h.service
            .carry_nfos_on_relink(&h.library, &batch)
            .await
            .unwrap();

        assert!(
            h.applied("N/movie.nfo").await.is_none(),
            "edited in the move, reversed: {reversed}"
        );
        assert_eq!(h.movie_pins(), vec![Some("tmdb:949".to_string())]);
        h.scan().await;
        assert_eq!(
            h.movie_pins(),
            vec![Some("tmdb:2".to_string())],
            "reversed: {reversed}"
        );
    }
}

/// The same for a `tvshow.nfo` above two relinked episodes' season folder.
#[tokio::test]
async fn a_shared_tvshow_nfo_edited_in_a_move_is_judged_the_same_in_either_batch_order() {
    for reversed in [false, true] {
        let h = Harness::build(Probe::ContentHashed, pinned_clock()).await;
        h.video("P/Lost/Season 01/Lost - S01E01.mkv");
        h.write("P/Lost/tvshow.nfo", &tmdb_show(4607));
        h.video("Q/Lost/Season 01/Lost - S01E02.mkv");
        h.scan().await;
        assert_eq!(h.show_pins(), vec![Some("tmdb:4607".to_string())]);

        let mut batch = move_videos(
            &h,
            &[
                (
                    "P/Lost/Season 01/Lost - S01E01.mkv",
                    "N/Lost/Season 01/Lost - S01E01.mkv",
                ),
                (
                    "Q/Lost/Season 01/Lost - S01E02.mkv",
                    "N/Lost/Season 01/Lost - S01E02.mkv",
                ),
            ],
        );
        move_file(&h, "P/Lost/tvshow.nfo", "N/Lost/tvshow.nfo");
        h.write("N/Lost/tvshow.nfo", &tmdb_show(2));
        if reversed {
            batch.reverse();
        }
        h.service
            .carry_nfos_on_relink(&h.library, &batch)
            .await
            .unwrap();

        assert!(
            h.applied("N/Lost/tvshow.nfo").await.is_none(),
            "edited in the move, reversed: {reversed}"
        );
        assert_eq!(h.show_pins(), vec![Some("tmdb:4607".to_string())]);
        h.scan().await;
        assert_eq!(
            h.show_pins(),
            vec![Some("tmdb:2".to_string())],
            "reversed: {reversed}"
        );
    }
}
