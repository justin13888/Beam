use super::*;

// ─── choose_relink_candidate ────────────────────────────────────────────────

fn candidate(id: u128, path: &str, missing_since: Option<DateTime<Utc>>) -> MediaFile {
    MediaFile {
        id: Uuid::from_u128(id),
        library_id: Uuid::nil(),
        path: PathBuf::from(path),
        hash: 77,
        size_bytes: 1000,
        mtime: None,
        identity: None,
        mime_type: None,
        duration: None,
        container_format: None,
        content: None,
        status: FileStatus::Unknown,
        classifier_version: 0,
        container_tags: None,
        scanned_at: instant(0),
        updated_at: instant(0),
        missing_since,
    }
}

// ─── plan_content_moves ──────────────────────────────────────────────────────

/// A row of hash `hash` at `path`, 1000 bytes, with no modification time.
fn row(id: u128, path: &str, hash: u64) -> MediaFile {
    MediaFile {
        hash,
        ..candidate(id, path, None)
    }
}

/// `hash` found at a path, 1000 bytes.
fn found(hash: u64) -> Fingerprint {
    Fingerprint {
        size: 1000,
        mtime: None,
        identity: None,
        inodes: Inodes::Stable,
        hash,
    }
}

/// The moves a scan plans for `rows`, given what it found at the paths it
/// hashed (`fingerprints`) and the other paths it walked: each relink as
/// `(row id, path)`, and the displaced row ids.
fn moves(
    rows: &[MediaFile],
    walked: &[&str],
    fingerprints: &[(&str, u64)],
    shielded: &[&str],
    last_played: &HashMap<Uuid, DateTime<Utc>>,
) -> (Vec<(u128, PathBuf)>, Vec<u128>) {
    let rows: HashMap<PathBuf, MediaFile> = rows
        .iter()
        .map(|row| (row.path.clone(), row.clone()))
        .collect();
    let fingerprints: HashMap<PathBuf, Fingerprint> = fingerprints
        .iter()
        .map(|(path, hash)| (PathBuf::from(path), found(*hash)))
        .collect();
    let walked: std::collections::HashSet<&Path> = walked
        .iter()
        .map(Path::new)
        .chain(fingerprints.keys().map(PathBuf::as_path))
        .collect();
    let is_shielded = |path: &Path| shielded.iter().any(|failed| path.starts_with(failed));
    let matches = content_matches(&rows, &walked, &fingerprints, is_shielded);
    let ContentMoves { relinks, displaced } = plan_content_moves(&rows, matches, last_played);
    let mut relinks: Vec<(u128, PathBuf)> = relinks
        .into_iter()
        .map(|(row, path, _)| (row.id.as_u128(), path))
        .collect();
    relinks.sort();
    (
        relinks,
        displaced.into_iter().map(|row| row.id.as_u128()).collect(),
    )
}

fn to(id: u128, path: &str) -> (u128, PathBuf) {
    (id, PathBuf::from(path))
}

/// Whether each kind of row can be the moved file's at all.
#[test]
fn only_a_gone_row_with_the_same_content_is_a_candidate() {
    let new_path = Path::new("/lib/new/Film.mkv");
    let gone = Some(instant(0));
    let absent = |path: &Path| path.starts_with("/lib/absent");

    struct Case {
        name: &'static str,
        row: MediaFile,
        hash: u64,
        matches: bool,
    }
    let with = |mut row: MediaFile, change: fn(&mut MediaFile)| {
        change(&mut row);
        row
    };
    let cases = [
        Case {
            name: "missing, same hash and size",
            row: candidate(1, "/lib/old/Film.mkv", gone),
            hash: 77,
            matches: true,
        },
        Case {
            name: "not yet marked, but gone from disk",
            row: candidate(1, "/lib/absent/Film.mkv", None),
            hash: 77,
            matches: true,
        },
        Case {
            name: "still on disk: a copy, not a move",
            row: candidate(1, "/lib/old/Film.mkv", None),
            hash: 77,
            matches: false,
        },
        Case {
            name: "another hash",
            row: candidate(1, "/lib/old/Film.mkv", gone),
            hash: 78,
            matches: false,
        },
        Case {
            name: "same hash, another size",
            row: with(candidate(1, "/lib/old/Film.mkv", gone), |row| {
                row.size_bytes = 999
            }),
            hash: 77,
            matches: false,
        },
        Case {
            name: "the unhashed sentinel matches nothing",
            row: with(candidate(1, "/lib/old/Film.mkv", gone), |row| row.hash = 0),
            hash: 0,
            matches: false,
        },
        Case {
            name: "the row already at the new path",
            row: candidate(1, "/lib/new/Film.mkv", gone),
            hash: 77,
            matches: false,
        },
    ];
    for Case {
        name,
        row,
        hash,
        matches,
    } in cases
    {
        let rows = [row];
        let chosen = choose_relink_candidate(new_path, 1000, hash, &rows, absent, &HashMap::new());
        assert_eq!(chosen.is_some(), matches, "{name}");
    }
}

/// Among several candidates the one most like the moved file wins -- the
/// same name, then the same directory, then the one played most recently,
/// then the lowest id -- and the choice does not depend on the order the
/// rows came in.
#[test]
fn the_most_alike_candidate_wins_whatever_the_order() {
    let new_path = Path::new("/lib/b/One.mkv");
    let never_absent = |_: &Path| false;
    let gone = Some(instant(0));
    let played = |plays: &[(u128, i64)]| -> HashMap<Uuid, DateTime<Utc>> {
        plays
            .iter()
            .map(|(id, secs)| (Uuid::from_u128(*id), instant(*secs)))
            .collect()
    };
    type Plays = HashMap<Uuid, DateTime<Utc>>;
    let cases: [(&str, Vec<MediaFile>, Plays, u128); 5] = [
        (
            "the same file name beats the same directory, and being played",
            vec![
                candidate(1, "/lib/b/Two.mkv", gone),
                candidate(2, "/lib/a/One.mkv", gone),
            ],
            played(&[(1, 9)]),
            2,
        ),
        (
            "then the same directory beats being played",
            vec![
                candidate(1, "/lib/a/Two.mkv", gone),
                candidate(2, "/lib/b/Two.mkv", gone),
            ],
            played(&[(1, 9)]),
            2,
        ),
        (
            "then the one played most recently",
            vec![
                candidate(1, "/lib/a/Two.mkv", gone),
                candidate(2, "/lib/a/Six.mkv", gone),
            ],
            played(&[(1, 1), (2, 9)]),
            2,
        ),
        (
            "a row played at all beats one never played",
            vec![
                candidate(1, "/lib/a/Two.mkv", gone),
                candidate(2, "/lib/a/Six.mkv", gone),
            ],
            played(&[(2, 1)]),
            2,
        ),
        (
            "then the lowest id",
            vec![
                candidate(2, "/lib/a/Two.mkv", gone),
                candidate(1, "/lib/c/Two.mkv", gone),
            ],
            played(&[(1, 5), (2, 5)]),
            1,
        ),
    ];
    for (name, rows, last_played, expected) in cases {
        let mut reversed = rows.clone();
        reversed.reverse();
        for rows in [rows, reversed] {
            let chosen =
                choose_relink_candidate(new_path, 1000, 77, &rows, never_absent, &last_played);
            assert_eq!(
                chosen.map(|row| row.id),
                Some(Uuid::from_u128(expected)),
                "{name}"
            );
        }
    }
}

mod relink_properties {
    use super::*;
    use proptest::prelude::*;

    /// A row of one of two contents, one of two sizes, at one of a few
    /// paths, gone or not, under a small id so ids collide with plays.
    fn any_candidate() -> impl Strategy<Value = MediaFile> {
        (
            0u128..6,
            prop::sample::select(vec![
                "/lib/a/One.mkv",
                "/lib/a/Two.mkv",
                "/lib/b/One.mkv",
                "/lib/b/Two.mkv",
                "/lib/c/Six.mkv",
            ]),
            prop::sample::select(vec![0u64, 77, 78]),
            prop::sample::select(vec![999u64, 1000]),
            any::<bool>(),
        )
            .prop_map(|(id, path, hash, size_bytes, gone)| MediaFile {
                hash,
                size_bytes,
                ..candidate(id, path, gone.then(|| instant(0)))
            })
    }

    proptest! {
        /// The row a moved file relinks to does not depend on the order the
        /// candidates come in: any permutation of them chooses the same row.
        #[test]
        fn the_chosen_row_is_the_same_whatever_the_order(
            (rows, shuffled) in prop::collection::vec(any_candidate(), 0..8)
                .prop_map(|rows| {
                    // Distinct ids, as a store's rows have.
                    let mut seen = std::collections::HashSet::new();
                    rows.into_iter()
                        .filter(|row| seen.insert(row.id))
                        .collect::<Vec<MediaFile>>()
                })
                .prop_flat_map(|rows| (Just(rows.clone()), Just(rows).prop_shuffle())),
            plays in prop::collection::hash_map(0u128..6, 0i64..4, 0..6),
            hash in prop::sample::select(vec![0u64, 77, 78]),
        ) {
            let last_played: HashMap<Uuid, DateTime<Utc>> = plays
                .into_iter()
                .map(|(id, secs)| (Uuid::from_u128(id), instant(secs)))
                .collect();
            let new_path = Path::new("/lib/b/One.mkv");
            let absent = |path: &Path| path.starts_with("/lib/c");

            let chosen = |rows: &[MediaFile]| {
                choose_relink_candidate(new_path, 1000, hash, rows, absent, &last_played)
                    .map(|row| row.id)
            };
            prop_assert_eq!(chosen(&rows), chosen(&shuffled));
        }
    }
}

/// Each way content moves between the paths of one scan, and which rows
/// follow it. Every case is planned from the walk as the scan sees it.
#[test]
fn rows_follow_their_content_across_the_walk() {
    let none = HashMap::new();
    struct Case {
        name: &'static str,
        rows: Vec<MediaFile>,
        walked: Vec<&'static str>,
        fingerprints: Vec<(&'static str, u64)>,
        shielded: Vec<&'static str>,
        relinks: Vec<(u128, PathBuf)>,
        displaced: Vec<u128>,
    }
    let cases = [
        Case {
            name: "a rename",
            rows: vec![row(1, "/lib/a.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/b.mkv")],
            displaced: vec![],
        },
        Case {
            name: "a swap of two names",
            rows: vec![row(1, "/lib/a.mkv", 10), row(2, "/lib/b.mkv", 20)],
            walked: vec![],
            fingerprints: vec![("/lib/a.mkv", 20), ("/lib/b.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/b.mkv"), to(2, "/lib/a.mkv")],
            displaced: vec![],
        },
        Case {
            name: "a rotation of three",
            rows: vec![
                row(1, "/lib/a.mkv", 10),
                row(2, "/lib/b.mkv", 20),
                row(3, "/lib/c.mkv", 30),
            ],
            walked: vec![],
            fingerprints: vec![("/lib/a.mkv", 30), ("/lib/b.mkv", 10), ("/lib/c.mkv", 20)],
            shielded: vec![],
            relinks: vec![
                to(1, "/lib/b.mkv"),
                to(2, "/lib/c.mkv"),
                to(3, "/lib/a.mkv"),
            ],
            displaced: vec![],
        },
        Case {
            name: "a move onto another file's path displaces it",
            rows: vec![
                row(1, "/lib/a.mkv", 10),
                row(2, "/lib/b.mkv", 20),
                row(3, "/lib/c.mkv", 30),
            ],
            walked: vec![],
            // a -> b over b's file, then c -> a.
            fingerprints: vec![("/lib/a.mkv", 30), ("/lib/b.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/b.mkv"), to(3, "/lib/a.mkv")],
            displaced: vec![2],
        },
        Case {
            name: "a rename onto the path of a row already missing",
            rows: vec![
                row(1, "/lib/heat.mkv", 10),
                MediaFile {
                    missing_since: Some(instant(0)),
                    ..row(2, "/lib/stale.mkv", 20)
                },
            ],
            walked: vec![],
            fingerprints: vec![("/lib/stale.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/stale.mkv")],
            displaced: vec![2],
        },
        Case {
            name: "a copy leaves the row whose path still holds it",
            rows: vec![row(1, "/lib/a.mkv", 10)],
            walked: vec!["/lib/a.mkv"],
            fingerprints: vec![("/lib/copy.mkv", 10)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "a missing row back at its own path is restored, not moved",
            rows: vec![MediaFile {
                missing_since: Some(instant(0)),
                ..row(1, "/lib/a.mkv", 10)
            }],
            walked: vec![],
            fingerprints: vec![("/lib/a.mkv", 10)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "new content no row records is a change, and moves nothing",
            rows: vec![row(1, "/lib/a.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/a.mkv", 99)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "a row under a path the walk could not read is not moved",
            rows: vec![row(1, "/lib/unreadable/a.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 10)],
            shielded: vec!["/lib/unreadable"],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "a walk that failed without a path moves nothing",
            rows: vec![row(1, "/lib/a.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 10)],
            shielded: vec!["/"],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "a row never hashed cannot be followed",
            rows: vec![row(1, "/lib/a.mkv", 0)],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 0)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "the same hash at another size is other content",
            rows: vec![MediaFile {
                size_bytes: 999,
                ..row(1, "/lib/a.mkv", 10)
            }],
            walked: vec![],
            fingerprints: vec![("/lib/b.mkv", 10)],
            shielded: vec![],
            relinks: vec![],
            displaced: vec![],
        },
        Case {
            name: "two identical files moved together keep their names",
            rows: vec![row(1, "/lib/x/One.mkv", 10), row(2, "/lib/x/Two.mkv", 10)],
            walked: vec![],
            fingerprints: vec![("/lib/y/Two.mkv", 10), ("/lib/y/One.mkv", 10)],
            shielded: vec![],
            relinks: vec![to(1, "/lib/y/One.mkv"), to(2, "/lib/y/Two.mkv")],
            displaced: vec![],
        },
    ];
    for Case {
        name,
        rows,
        walked,
        fingerprints,
        shielded,
        relinks,
        displaced,
    } in cases
    {
        let mut reversed = rows.clone();
        reversed.reverse();
        for rows in [rows, reversed] {
            assert_eq!(
                moves(&rows, &walked, &fingerprints, &shielded, &none),
                (relinks.clone(), displaced.clone()),
                "{name}"
            );
        }
    }
}

/// Two identical copies, one moved and one deleted: whichever the scan
/// meets, the row someone has been watching keeps the file.
#[test]
fn of_identical_copies_the_row_played_most_recently_follows_the_file() {
    let rows = [row(1, "/lib/a/Film.mkv", 10), row(2, "/lib/b/Film.mkv", 10)];
    for (watched, other) in [(1u128, 2u128), (2, 1)] {
        let last_played: HashMap<Uuid, DateTime<Utc>> =
            [(Uuid::from_u128(watched), instant(5))].into();
        assert_eq!(
            moves(&rows, &[], &[("/lib/c/Film.mkv", 10)], &[], &last_played),
            (vec![to(watched, "/lib/c/Film.mkv")], vec![]),
            "the other, {other}, is left to be marked missing"
        );
    }
}

// ─── the scan and the watcher ────────────────────────────────────────────────

/// A file renamed onto a played row's path, when its own row was never
/// played, replaces that row's content rather than taking its path; any
/// other pairing is planned as before -- including a swap with a played
/// row, which displaces nobody.
#[test]
fn a_played_rows_path_is_replaced_not_taken_by_an_unplayed_row() {
    let played = |ids: &[u128]| -> HashMap<Uuid, DateTime<Utc>> {
        ids.iter()
            .map(|id| (Uuid::from_u128(*id), instant(5)))
            .collect()
    };
    let replaced = [row(1, "/lib/M.mkv", 10), row(2, "/lib/T.mkv", 20)];
    // T renamed onto M: M's path holds T's content, T's path is gone.
    let rename_onto_m = [("/lib/M.mkv", 20)];
    /// A case's name, its rows, what the scan found where, the rows played,
    /// and the relinks and displaced rows planned.
    type Case<'a> = (
        &'a str,
        &'a [MediaFile],
        &'a [(&'a str, u64)],
        &'a [u128],
        Vec<(u128, PathBuf)>,
        Vec<u128>,
    );
    let cases: [Case<'_>; 6] = [
        (
            "the played row keeps its path",
            &replaced,
            &rename_onto_m,
            &[1],
            vec![],
            vec![],
        ),
        (
            "a played renamed row takes the path",
            &replaced,
            &rename_onto_m,
            &[1, 2],
            vec![to(2, "/lib/M.mkv")],
            vec![1],
        ),
        (
            "an unplayed path's row is displaced",
            &replaced,
            &rename_onto_m,
            &[2],
            vec![to(2, "/lib/M.mkv")],
            vec![1],
        ),
        (
            "nobody played either",
            &replaced,
            &rename_onto_m,
            &[],
            vec![to(2, "/lib/M.mkv")],
            vec![1],
        ),
        (
            "a swap with a played row still swaps",
            &replaced,
            &[("/lib/M.mkv", 20), ("/lib/T.mkv", 10)],
            &[1],
            vec![to(1, "/lib/T.mkv"), to(2, "/lib/M.mkv")],
            vec![],
        ),
        (
            "the declined row goes on to a path of its own",
            &[
                row(1, "/lib/M.mkv", 10),
                row(2, "/lib/T.mkv", 20),
                row(3, "/lib/x/Copy.mkv", 30),
            ],
            // T's content at M, and at the path of a row whose own content is gone.
            &[("/lib/M.mkv", 20), ("/lib/x/Copy.mkv", 20)],
            &[1],
            vec![to(2, "/lib/x/Copy.mkv")],
            vec![3],
        ),
    ];
    for (name, rows, fingerprints, watched, relinks, displaced) in cases {
        assert_eq!(
            moves(rows, &[], fingerprints, &[], &played(watched)),
            (relinks, displaced),
            "{name}"
        );
    }
}
