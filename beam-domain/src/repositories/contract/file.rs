/// Behavioural contract for [`crate::repositories::FileRepository`]: the
/// soft-delete lifecycle of issue #179 -- which reads hide a missing file,
/// which see it, and that marking, restoring and purging keep the id and the
/// first stamp.
///
/// `$setup` names an `async fn() -> impl FileRepositoryFixture`.
#[macro_export]
macro_rules! file_repository_contract {
    ($setup:path) => {
        use ::chrono::{DateTime, Utc};
        use ::std::path::PathBuf;
        use ::uuid::Uuid;
        use $crate::models::file::{
            CreateMediaFile, FileClassification, FileRelink, FileStatus, MediaFile,
            MediaFileContent, ProbeUpdate, UpdateMediaFile, displaced_path,
        };
        use $crate::repositories::contract::fixture::FileRepositoryFixture;
        use $crate::utils::classification::ContainerTags;

        /// A fixed, non-epoch instant: `missing_since` is `timestamptz`, and a
        /// whole second survives Postgres's microsecond precision unchanged.
        fn at(offset_secs: i64) -> DateTime<Utc> {
            DateTime::from_timestamp(1_700_000_000 + offset_secs, 0).expect("valid instant")
        }

        /// Insert a present file under `library_id`. The path and hash are
        /// fresh per call so concurrently running Postgres tests never meet.
        async fn file_in(
            fixture: &impl FileRepositoryFixture,
            library_id: Uuid,
            content: MediaFileContent,
        ) -> MediaFile {
            let unique = Uuid::new_v4();
            fixture
                .repo()
                .create(CreateMediaFile {
                    library_id,
                    path: PathBuf::from(format!("/videos/{library_id}/{unique}.mkv")),
                    // Positive and unique: the hash is a signed BIGINT column.
                    hash: (unique.as_u128() as u64) >> 1,
                    size_bytes: 1024,
                    mtime: None,
                    identity: None,
                    mime_type: Some("video/x-matroska".to_string()),
                    duration: None,
                    container_format: Some("matroska".to_string()),
                    content: Some(content),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: Some(some_tags()),
                })
                .await
                .expect("create a file")
        }

        /// Tags a probe could have read; some left out, as most files do.
        fn some_tags() -> ContainerTags {
            ContainerTags {
                show: Some("The Office".to_string()),
                season: Some(2),
                ..ContainerTags::default()
            }
        }

        async fn movie_file(fixture: &impl FileRepositoryFixture, library_id: Uuid) -> MediaFile {
            let movie_entry_id = fixture.new_movie_entry(library_id).await;
            file_in(
                fixture,
                library_id,
                MediaFileContent::movie(movie_entry_id),
            )
            .await
        }

        fn ids(files: &[MediaFile]) -> Vec<Uuid> {
            let mut ids: Vec<Uuid> = files.iter().map(|f| f.id).collect();
            ids.sort();
            ids
        }

        fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
            ids.sort();
            ids
        }

        /// A library's files page by path, then id: each page holds at most
        /// `limit` present files of that library, the next starts strictly
        /// after the position it is given, and walking the pages visits every
        /// present file once, in order -- a missing file and another
        /// library's never.
        #[tokio::test]
        async fn a_library_s_files_page_by_path_without_gaps_or_repeats() {
            use ::std::num::NonZeroU32;
            use $crate::models::file::LibraryFilePosition;

            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let other_library = fixture.new_library().await;
            let root = PathBuf::from(format!("/videos/{library}"));
            let mut created = Vec::new();
            // Inserted out of order; lower-case ASCII names, so the database
            // collation and a byte comparison agree on the order.
            for (name, in_library) in [
                ("d/e.mkv", library),
                ("a.mkv", library),
                ("c.mkv", library),
                ("b/z.mkv", library),
                ("b/a.mkv", library),
                ("gone.mkv", library),
                ("aa.mkv", other_library),
            ] {
                let movie_entry_id = fixture.new_movie_entry(in_library).await;
                let unique = Uuid::new_v4();
                let file = fixture
                    .repo()
                    .create(CreateMediaFile {
                        library_id: in_library,
                        path: root.join(name),
                        hash: (unique.as_u128() as u64) >> 1,
                        size_bytes: 1024,
                        mtime: None,
                        identity: None,
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: Some(MediaFileContent::movie(movie_entry_id)),
                        status: FileStatus::Known,
                        classifier_version: 0,
                        container_tags: None,
                    })
                    .await
                    .expect("create a file");
                created.push((name, file));
            }
            let gone = created
                .iter()
                .find(|(name, _)| *name == "gone.mkv")
                .map(|(_, file)| file.id)
                .unwrap();
            fixture.repo().mark_missing(vec![gone], at(0)).await.unwrap();

            let two = NonZeroU32::new(2).unwrap();
            let mut walked = Vec::new();
            let mut after: Option<LibraryFilePosition> = None;
            loop {
                let page = fixture
                    .repo()
                    .find_page_by_library(library, after.clone(), two)
                    .await
                    .expect("read a page");
                assert!(page.len() <= 2, "a page holds at most the limit");
                let Some(last) = page.last() else { break };
                after = Some(LibraryFilePosition {
                    path: last.path.clone(),
                    id: last.id,
                });
                walked.extend(page.iter().map(|f| f.path.clone()));
                assert!(walked.len() <= 5, "the walk ends: {walked:?}");
            }
            let expected: Vec<PathBuf> = ["a.mkv", "b/a.mkv", "b/z.mkv", "c.mkv", "d/e.mkv"]
                .into_iter()
                .map(|name| root.join(name))
                .collect();
            assert_eq!(
                walked, expected,
                "every present file of the library, once, by path"
            );

            let other = fixture
                .repo()
                .find_page_by_library(other_library, None, NonZeroU32::new(10).unwrap())
                .await
                .unwrap();
            assert_eq!(
                other.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
                vec![root.join("aa.mkv")],
                "another library's file is its own"
            );
        }

        /// The files beneath a directory are those whose path starts with it a
        /// whole component at a time, at any depth -- the directory name is a
        /// literal, never a pattern -- and present, in that library.
        #[tokio::test]
        async fn the_files_under_a_directory_are_the_present_ones_beneath_it() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let other_library = fixture.new_library().await;
            let root = PathBuf::from(format!("/videos/{library}"));
            let mut by_name = ::std::collections::HashMap::new();
            for (name, in_library) in [
                ("Show/a.mkv", library),
                ("Show/Season 1/b.mkv", library),
                ("Show/Season 1/Extras/c.mkv", library),
                ("Show 2/d.mkv", library),
                ("Showtime/e.mkv", library),
                ("Sho_/f.mkv", library),
                ("Sh%/g.mkv", library),
                ("Show/gone.mkv", library),
                ("Show/elsewhere.mkv", other_library),
            ] {
                let movie_entry_id = fixture.new_movie_entry(in_library).await;
                let unique = Uuid::new_v4();
                let file = fixture
                    .repo()
                    .create(CreateMediaFile {
                        library_id: in_library,
                        path: root.join(name),
                        hash: (unique.as_u128() as u64) >> 1,
                        size_bytes: 1024,
                        mtime: None,
                        identity: None,
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: Some(MediaFileContent::movie(movie_entry_id)),
                        status: FileStatus::Known,
                        classifier_version: 0,
                        container_tags: None,
                    })
                    .await
                    .expect("create a file");
                by_name.insert(name, file.id);
            }
            fixture
                .repo()
                .mark_missing(vec![by_name["Show/gone.mkv"]], at(0))
                .await
                .unwrap();
            let under = |dir: &'static str| {
                let dir = root.join(dir);
                let repo = fixture.repo();
                async move { ids(&repo.find_all_under(library, &dir).await.unwrap()) }
            };
            let named = |names: &[&str]| sorted(names.iter().map(|n| by_name[n]).collect());

            assert_eq!(
                under("Show").await,
                named(&[
                    "Show/a.mkv",
                    "Show/Season 1/b.mkv",
                    "Show/Season 1/Extras/c.mkv"
                ]),
                "every present file beneath, at any depth; not `Show 2` or `Showtime`"
            );
            assert_eq!(
                under("Show/Season 1").await,
                named(&["Show/Season 1/b.mkv", "Show/Season 1/Extras/c.mkv"])
            );
            assert_eq!(under("Sho_").await, named(&["Sho_/f.mkv"]), "`_` is literal");
            assert_eq!(under("Sh%").await, named(&["Sh%/g.mkv"]), "`%` is literal");
            assert_eq!(
                under("Show/a.mkv").await,
                Vec::<Uuid>::new(),
                "a file is not beneath itself"
            );
        }

        /// One row per path (issue #181): a second file at a path is refused
        /// whatever its hash, and the first row is left as it was.
        #[tokio::test]
        async fn a_second_file_at_one_path_is_refused_whatever_its_hash() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let first = movie_file(&fixture, library).await;
            let movie_entry_id = fixture.new_movie_entry(library).await;

            let second = fixture
                .repo()
                .create(CreateMediaFile {
                    library_id: library,
                    path: first.path.clone(),
                    hash: first.hash ^ 1,
                    size_bytes: 2048,
                    mtime: None,
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(MediaFileContent::movie(movie_entry_id)),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
                })
                .await;

            assert!(second.is_err(), "a second row for a path must be refused");
            let stored = fixture
                .repo()
                .find_by_path(&first.path.to_string_lossy())
                .await
                .unwrap()
                .expect("the first row is still there");
            assert_eq!(stored.id, first.id);
            assert_eq!(stored.hash, first.hash);
        }

        /// An mtime is kept in whole microseconds, as `files.mtime`'s
        /// `TIMESTAMPTZ` is, and reads back as [`mtime_as_stored`] says --
        /// the precision the indexer brings a file's mtime to before
        /// comparing it with its row (issue #229). Whichever way it is
        /// written: created, updated, or relinked.
        #[tokio::test]
        async fn an_mtime_reads_back_in_whole_microseconds_as_mtime_as_stored_says() {
            use $crate::models::file::mtime_as_stored;
            let instant = |secs: i64, nanos: u32| {
                DateTime::from_timestamp(secs, nanos).expect("valid instant")
            };
            // (written, read back): what ext4 or btrfs reports, and what a
            // row holds for it.
            let cases = [
                // After 2000-01-01, the sub-microsecond part is dropped.
                (instant(1_790_000_341, 802_029_432), instant(1_790_000_341, 802_029_000)),
                (instant(1_790_000_341, 999_999_999), instant(1_790_000_341, 999_999_000)),
                // Before it, the driver truncates toward that epoch: up.
                (instant(946_684_799, 500), instant(946_684_799, 1_000)),
                // Before 1970 as well: the Unix epoch is not the one that counts.
                (instant(-1, 500), instant(-1, 1_000)),
                // Whole microseconds are kept as they are.
                (instant(1_790_000_341, 802_029_000), instant(1_790_000_341, 802_029_000)),
            ];
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;

            for (written, read_back) in cases {
                assert_eq!(mtime_as_stored(written), read_back, "{written}");

                let movie_entry_id = fixture.new_movie_entry(library).await;
                let created = repo
                    .create(CreateMediaFile {
                        library_id: library,
                        path: PathBuf::from(format!("/videos/{library}/{}.mkv", Uuid::new_v4())),
                        hash: (Uuid::new_v4().as_u128() as u64) >> 1,
                        size_bytes: 1024,
                        mtime: Some(written),
                        identity: None,
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: Some(MediaFileContent::movie(movie_entry_id)),
                        status: FileStatus::Known,
                        classifier_version: 0,
                        container_tags: None,
                    })
                    .await
                    .expect("create a file");
                let stored = repo.find_by_id(created.id).await.unwrap().expect("stored");
                assert_eq!(stored.mtime, Some(read_back), "created at {written}");

                let untouched = movie_file(&fixture, library).await;
                repo.update(UpdateMediaFile {
                    id: untouched.id,
                    hash: None,
                    size_bytes: None,
                    mtime: Some(written),
                    identity: None,
                    probe: ProbeUpdate::Keep,
                    content: None,
                    status: None,
                })
                .await
                .expect("update the file");
                let stored = repo.find_by_id(untouched.id).await.unwrap().expect("stored");
                assert_eq!(stored.mtime, Some(read_back), "updated to {written}");

                let moving = movie_file(&fixture, library).await;
                let moved_to =
                    PathBuf::from(format!("/videos/{library}/moved/{}.mkv", Uuid::new_v4()));
                repo.relink(vec![to(&moving, &moved_to, 1024, Some(written))], Vec::new(), at(0))
                    .await
                    .expect("relink the file");
                let stored = repo.find_by_id(moving.id).await.unwrap().expect("stored");
                assert_eq!(stored.mtime, Some(read_back), "relinked at {written}");
            }
        }

        /// A file's identity -- its inode and ctime (issue #228) -- is kept
        /// whichever way it is written: created, updated, or relinked. The
        /// inode keeps all 64 bits, past `i64::MAX` too, and the ctime reads
        /// back as [`FileIdentity::as_stored`] says, as an mtime does. An
        /// update that names none leaves the row's as it was, and a row
        /// written without one reads back without one.
        #[tokio::test]
        async fn an_identity_reads_back_as_stored_however_it_is_written() {
            use $crate::models::file::FileIdentity;
            let instant = |secs: i64, nanos: u32| {
                DateTime::from_timestamp(secs, nanos).expect("valid instant")
            };
            // (written, the ctime it reads back with): nanoseconds as ext4 or
            // btrfs reports them, and whole microseconds as a row keeps them.
            let identities = [
                (
                    FileIdentity { inode: 42, ctime: instant(1_790_000_341, 802_029_432) },
                    instant(1_790_000_341, 802_029_000),
                ),
                (
                    FileIdentity { inode: u64::MAX, ctime: instant(1_790_000_341, 999_999_999) },
                    instant(1_790_000_341, 999_999_000),
                ),
                (FileIdentity { inode: 1 << 63, ctime: at(5) }, at(5)),
            ];
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;

            let without = movie_file(&fixture, library).await;
            let stored = repo.find_by_id(without.id).await.unwrap().expect("stored");
            assert_eq!(stored.identity, None, "created without one");

            for (written, ctime) in identities {
                let read_back = Some(FileIdentity { inode: written.inode, ctime });
                assert_eq!(Some(written.as_stored()), read_back, "{written:?}");

                let movie_entry_id = fixture.new_movie_entry(library).await;
                let created = repo
                    .create(CreateMediaFile {
                        library_id: library,
                        path: PathBuf::from(format!("/videos/{library}/{}.mkv", Uuid::new_v4())),
                        hash: (Uuid::new_v4().as_u128() as u64) >> 1,
                        size_bytes: 1024,
                        mtime: None,
                        identity: Some(written),
                        mime_type: None,
                        duration: None,
                        container_format: None,
                        content: Some(MediaFileContent::movie(movie_entry_id)),
                        status: FileStatus::Known,
                        classifier_version: 0,
                        container_tags: None,
                    })
                    .await
                    .expect("create a file");
                let stored = repo.find_by_id(created.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, read_back, "created with {written:?}");

                let untouched = movie_file(&fixture, library).await;
                let update = |identity: Option<FileIdentity>| UpdateMediaFile {
                    id: untouched.id,
                    hash: None,
                    size_bytes: None,
                    mtime: None,
                    identity,
                    probe: ProbeUpdate::Keep,
                    content: None,
                    status: None,
                };
                repo.update(update(Some(written))).await.expect("update the file");
                let stored = repo.find_by_id(untouched.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, read_back, "updated to {written:?}");
                repo.update(update(None)).await.expect("update the file");
                let stored = repo.find_by_id(untouched.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, read_back, "an update naming none keeps it");

                let moving = movie_file(&fixture, library).await;
                let moved_to =
                    PathBuf::from(format!("/videos/{library}/moved/{}.mkv", Uuid::new_v4()));
                let relink = FileRelink {
                    identity: Some(written),
                    ..to(&moving, &moved_to, 1024, None)
                };
                repo.relink(vec![relink], Vec::new(), at(0))
                    .await
                    .expect("relink the file");
                let stored = repo.find_by_id(moving.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, read_back, "relinked at {written:?}");

                // A relink records what the file was found at, so one found
                // with no identity -- a platform without them -- clears the
                // identity the row had, unlike an update naming none.
                let moved_again =
                    PathBuf::from(format!("/videos/{library}/again/{}.mkv", Uuid::new_v4()));
                repo.relink(vec![to(&stored, &moved_again, 1024, None)], Vec::new(), at(1))
                    .await
                    .expect("relink the file again");
                let stored = repo.find_by_id(moving.id).await.unwrap().expect("stored");
                assert_eq!(stored.identity, None, "relinked from {written:?} with none");
            }
        }

        #[tokio::test]
        async fn a_multi_episode_range_is_stored_with_its_first_episode() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let episode_id = fixture.new_episode(library).await;
            let file = file_in(
                &fixture,
                library,
                MediaFileContent::Episode {
                    episode_id,
                    last_episode_number: Some(3),
                },
            )
            .await;

            let stored = fixture
                .repo()
                .find_by_id(file.id)
                .await
                .unwrap()
                .expect("the file is present");
            assert!(
                matches!(
                    stored.content,
                    Some(MediaFileContent::Episode {
                        episode_id: id,
                        last_episode_number: Some(3),
                    }) if id == episode_id
                ),
                "{:?}",
                stored.content
            );
        }

        /// A part of a multi-part movie (issue #233) is stored with its entry
        /// and read back with its number; a reclassification replaces the
        /// number, and one that makes the file an episode's drops it.
        #[tokio::test]
        async fn a_movie_part_is_stored_with_its_entry_and_replaced_with_it() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let movie_entry_id = fixture.new_movie_entry(library).await;
            let part = |part_number| MediaFileContent::Movie {
                movie_entry_id,
                part_number: Some(part_number),
            };
            let file = file_in(&fixture, library, part(2)).await;

            let stored = repo.find_by_id(file.id).await.unwrap().expect("present");
            assert_eq!(stored.content, Some(part(2)));
            let listed = repo.find_by_movie_entry_id(movie_entry_id).await.unwrap();
            assert_eq!(
                listed.iter().map(|f| f.content.clone()).collect::<Vec<_>>(),
                vec![Some(part(2))],
                "the entry lists the part with its number"
            );

            let renumbered = repo
                .set_classification(
                    file.id,
                    FileClassification {
                        content: Some(part(1)),
                        status: FileStatus::Known,
                        classifier_version: 3,
                    },
                )
                .await
                .unwrap();
            assert_eq!(renumbered.content, Some(part(1)));

            let episode_id = fixture.new_episode(library).await;
            repo.set_classification(
                file.id,
                FileClassification {
                    content: Some(MediaFileContent::episode(episode_id)),
                    status: FileStatus::Known,
                    classifier_version: 3,
                },
            )
            .await
            .expect("a part may become an episode's file");
            repo.set_classification(
                file.id,
                FileClassification {
                    content: Some(MediaFileContent::movie(movie_entry_id)),
                    status: FileStatus::Known,
                    classifier_version: 3,
                },
            )
            .await
            .unwrap();
            let stored = repo.find_by_id(file.id).await.unwrap().expect("present");
            assert_eq!(
                stored.content,
                Some(MediaFileContent::movie(movie_entry_id)),
                "no part survives the episode in between"
            );
        }

        #[tokio::test]
        async fn set_classification_replaces_the_classification_and_nothing_else() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            let episode_id = fixture.new_episode(library).await;

            let moved = repo
                .set_classification(
                    file.id,
                    FileClassification {
                        content: Some(MediaFileContent::Episode {
                            episode_id,
                            last_episode_number: Some(2),
                        }),
                        status: FileStatus::Known,
                        classifier_version: 7,
                    },
                )
                .await
                .unwrap();
            assert_eq!(moved.id, file.id);
            assert_eq!(moved.classifier_version, 7);
            assert_eq!((moved.hash, moved.size_bytes), (file.hash, file.size_bytes));
            assert_eq!(moved.path, file.path);
            assert_eq!(
                ids(&repo.find_by_episode_id(episode_id).await.unwrap()),
                vec![file.id],
                "the file now belongs to the episode"
            );

            let cleared = repo
                .set_classification(
                    file.id,
                    FileClassification {
                        content: None,
                        status: FileStatus::Unknown,
                        classifier_version: 8,
                    },
                )
                .await
                .unwrap();
            assert!(cleared.content.is_none(), "{:?}", cleared.content);
            assert_eq!(cleared.status, FileStatus::Unknown);
            assert_eq!(cleared.classifier_version, 8);
            assert!(repo.find_by_episode_id(episode_id).await.unwrap().is_empty());
            let stored = repo.find_by_id(file.id).await.unwrap().expect("still present");
            assert!(stored.content.is_none());
            assert_eq!(stored.classifier_version, 8);
            assert_eq!(
                stored.container_tags,
                Some(some_tags()),
                "reclassifying reads the tags; it never replaces them"
            );
        }

        /// A file with no content is `Unknown`: `Known` and `Changed` name a
        /// movie's or an episode's file. Every write that would leave a row
        /// otherwise is refused, and a refused update changes nothing.
        #[tokio::test]
        async fn a_file_without_content_can_only_be_unknown() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let create = |status: FileStatus| {
                let unique = Uuid::new_v4();
                CreateMediaFile {
                    library_id: library,
                    path: PathBuf::from(format!("/videos/{library}/{unique}.mkv")),
                    hash: (unique.as_u128() as u64) >> 1,
                    size_bytes: 1024,
                    mtime: None,
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: None,
                    status,
                    classifier_version: 0,
                    container_tags: None,
                }
            };
            for status in [FileStatus::Known, FileStatus::Changed] {
                assert!(
                    repo.create(create(status)).await.is_err(),
                    "created a {status:?} file with no content"
                );
            }
            let unknown = repo
                .create(create(FileStatus::Unknown))
                .await
                .expect("an Unknown file with no content is valid");

            for status in [FileStatus::Known, FileStatus::Changed] {
                let refused = repo
                    .update(UpdateMediaFile {
                        id: unknown.id,
                        hash: Some(unknown.hash + 1),
                        size_bytes: Some(2048),
                        mtime: None,
                        identity: None,
                        probe: ProbeUpdate::Keep,
                        content: None,
                        status: Some(status),
                    })
                    .await;
                assert!(refused.is_err(), "updated to {status:?} with no content");
                let refused = repo
                    .set_classification(
                        unknown.id,
                        FileClassification {
                            content: None,
                            status,
                            classifier_version: 1,
                        },
                    )
                    .await;
                assert!(refused.is_err(), "classified {status:?} with no content");
            }
            let stored = repo
                .find_by_id(unknown.id)
                .await
                .unwrap()
                .expect("still present");
            assert_eq!(stored.status, FileStatus::Unknown);
            assert_eq!(
                (stored.hash, stored.size_bytes, stored.classifier_version),
                (unknown.hash, unknown.size_bytes, 0),
                "a refused write changes nothing"
            );
        }

        /// An update sets, keeps or clears a file's probe results -- MIME
        /// type, duration and container format -- together, and clearing
        /// them leaves the rest of the row as it was (issue #181: a failed
        /// probe of changed content must not keep the old content's results).
        #[tokio::test]
        async fn an_update_sets_keeps_or_clears_the_probe_results_together() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            let repo = fixture.repo();
            let update = |probe: ProbeUpdate| UpdateMediaFile {
                id: file.id,
                hash: None,
                size_bytes: None,
                mtime: None,
                identity: None,
                probe,
                content: None,
                status: None,
            };
            let probe_of = |file: &MediaFile| {
                (
                    file.mime_type.clone(),
                    file.duration,
                    file.container_format.clone(),
                    file.container_tags.clone(),
                )
            };
            let tags = ContainerTags {
                title: Some("The Dundies".to_string()),
                show: Some("The Office".to_string()),
                season: Some(2),
                episode: Some(1),
                year: Some(2005),
            };

            repo.update(update(ProbeUpdate::Set {
                mime_type: "video/mp4".to_string(),
                duration: ::std::time::Duration::from_secs(90),
                container_format: "mp4".to_string(),
                container_tags: tags.clone(),
            }))
            .await
            .expect("set the probe results");
            let expected = (
                Some("video/mp4".to_string()),
                Some(::std::time::Duration::from_secs(90)),
                Some("mp4".to_string()),
                Some(tags),
            );
            let stored = || async {
                repo.find_by_id(file.id)
                    .await
                    .unwrap()
                    .expect("still present")
            };
            assert_eq!(probe_of(&stored().await), expected);

            repo.update(update(ProbeUpdate::Keep))
                .await
                .expect("keep the probe results");
            assert_eq!(probe_of(&stored().await), expected);

            repo.update(update(ProbeUpdate::Clear))
                .await
                .expect("clear the probe results");
            let cleared = stored().await;
            assert_eq!(probe_of(&cleared), (None, None, None, None));
            assert_eq!(
                (
                    cleared.hash,
                    cleared.size_bytes,
                    cleared.status,
                    cleared.content.clone()
                ),
                (file.hash, file.size_bytes, file.status, file.content.clone()),
                "clearing the probe results touches nothing else"
            );
        }

        #[tokio::test]
        async fn a_created_file_is_present() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;

            assert_eq!(file.missing_since, None);
            let found = fixture
                .repo()
                .find_by_id(file.id)
                .await
                .unwrap()
                .expect("a new file is visible");
            assert_eq!(found.missing_since, None);
            assert_eq!(
                found.container_tags,
                Some(some_tags()),
                "the tags it was created with are stored"
            );
        }

        #[tokio::test]
        async fn a_missing_file_is_hidden_from_every_visible_read() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let movie_entry_id = fixture.new_movie_entry(library).await;
            let episode_id = fixture.new_episode(library).await;
            let movie = file_in(
                &fixture,
                library,
                MediaFileContent::movie(movie_entry_id),
            )
            .await;
            let episode =
                file_in(&fixture, library, MediaFileContent::episode(episode_id)).await;
            let kept = movie_file(&fixture, library).await;

            assert_eq!(
                repo.mark_missing(vec![movie.id, episode.id], at(0))
                    .await
                    .unwrap(),
                2
            );

            assert!(repo.find_by_id(movie.id).await.unwrap().is_none());
            assert!(repo.find_by_id(episode.id).await.unwrap().is_none());
            assert!(repo.find_by_hash(movie.hash).await.unwrap().is_empty());
            assert!(
                repo.find_by_movie_entry_id(movie_entry_id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                repo.find_by_episode_id(episode_id)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                ids(&repo.find_all_by_library(library).await.unwrap()),
                vec![kept.id],
                "only the present file is listed"
            );
            // The present file is untouched by its neighbours going missing.
            assert!(repo.find_by_id(kept.id).await.unwrap().is_some());
            assert_eq!(repo.find_by_hash(kept.hash).await.unwrap().len(), 1);
        }

        #[tokio::test]
        async fn a_missing_file_is_still_seen_by_the_reconcile_reads() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let missing = movie_file(&fixture, library).await;
            let present = movie_file(&fixture, library).await;
            repo.mark_missing(vec![missing.id], at(0)).await.unwrap();

            let by_path = repo
                .find_by_path(&missing.path.to_string_lossy())
                .await
                .unwrap()
                .expect("a missing file is still found by its path");
            assert_eq!(by_path.id, missing.id);
            assert_eq!(by_path.missing_since, Some(at(0)));

            let all = repo
                .find_all_by_library_including_missing(library)
                .await
                .unwrap();
            assert_eq!(ids(&all), sorted(vec![missing.id, present.id]));
            let stamped: Vec<Option<DateTime<Utc>>> = all
                .iter()
                .filter(|f| f.id == missing.id)
                .map(|f| f.missing_since)
                .collect();
            assert_eq!(stamped, vec![Some(at(0))]);
        }

        #[tokio::test]
        async fn the_reconcile_listing_is_scoped_to_one_library() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other = fixture.new_library().await;
            let mine = movie_file(&fixture, library).await;
            let theirs = movie_file(&fixture, other).await;
            repo.mark_missing(vec![mine.id, theirs.id], at(0))
                .await
                .unwrap();

            assert_eq!(
                ids(&repo
                    .find_all_by_library_including_missing(library)
                    .await
                    .unwrap()),
                vec![mine.id]
            );
        }

        #[tokio::test]
        async fn marking_a_missing_file_again_keeps_the_first_stamp() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;

            assert_eq!(repo.mark_missing(vec![file.id], at(0)).await.unwrap(), 1);
            assert_eq!(
                repo.mark_missing(vec![file.id], at(3600)).await.unwrap(),
                0,
                "a row already missing is not newly marked"
            );

            let stored = repo
                .find_by_path(&file.path.to_string_lossy())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                stored.missing_since,
                Some(at(0)),
                "the grace period runs from when the file was first found gone"
            );
        }

        #[tokio::test]
        async fn restoring_a_missing_file_keeps_its_id_and_makes_it_visible() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            repo.mark_missing(vec![file.id], at(0)).await.unwrap();

            repo.restore(file.id).await.unwrap();

            let found = repo
                .find_by_id(file.id)
                .await
                .unwrap()
                .expect("a restored file is visible again under its old id");
            assert_eq!(found.missing_since, None);
            assert_eq!(found.path, file.path);
            assert_eq!(
                ids(&repo.find_all_by_library(library).await.unwrap()),
                vec![file.id]
            );
        }

        #[tokio::test]
        async fn a_restored_file_that_goes_missing_again_takes_a_fresh_stamp() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            repo.mark_missing(vec![file.id], at(0)).await.unwrap();
            repo.restore(file.id).await.unwrap();

            assert_eq!(repo.mark_missing(vec![file.id], at(60)).await.unwrap(), 1);

            let stored = repo
                .find_by_path(&file.path.to_string_lossy())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.missing_since, Some(at(60)));
        }

        #[tokio::test]
        async fn purge_removes_only_the_listed_rows_that_are_missing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let missing = movie_file(&fixture, library).await;
            let present = movie_file(&fixture, library).await;
            let unlisted = movie_file(&fixture, library).await;
            repo.mark_missing(vec![missing.id, unlisted.id], at(0))
                .await
                .unwrap();

            assert_eq!(
                repo.purge_missing(vec![missing.id, present.id])
                    .await
                    .unwrap(),
                1,
                "a present file is never purged, even when listed"
            );

            assert!(
                repo.find_by_path(&missing.path.to_string_lossy())
                    .await
                    .unwrap()
                    .is_none(),
                "the purged row is gone from the reconcile reads too"
            );
            assert!(repo.find_by_id(present.id).await.unwrap().is_some());
            assert_eq!(
                ids(&repo
                    .find_all_by_library_including_missing(library)
                    .await
                    .unwrap()),
                sorted(vec![present.id, unlisted.id]),
                "an unlisted missing row is left for its own grace period"
            );
        }

        /// A file of a new title under `library_id` whose content hash is
        /// `hash`, at a fresh path.
        async fn file_with_hash(
            fixture: &impl FileRepositoryFixture,
            library_id: Uuid,
            hash: u64,
        ) -> MediaFile {
            let movie_entry_id = fixture.new_movie_entry(library_id).await;
            fixture
                .repo()
                .create(CreateMediaFile {
                    library_id,
                    path: PathBuf::from(format!("/videos/{library_id}/{}.mkv", Uuid::new_v4())),
                    hash,
                    size_bytes: 1024,
                    mtime: None,
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(MediaFileContent::movie(movie_entry_id)),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
                })
                .await
                .expect("create a file")
        }

        /// Point `row` at `path`, found there at `size_bytes` bytes and `mtime`.
        fn to(
            row: &MediaFile,
            path: &::std::path::Path,
            size_bytes: u64,
            mtime: Option<DateTime<Utc>>,
        ) -> FileRelink {
            FileRelink {
                id: row.id,
                path: path.to_path_buf(),
                size_bytes,
                mtime,
                identity: None,
            }
        }

        /// The row now stored at `path`, if any.
        async fn at_path(fixture: &impl FileRepositoryFixture, path: &::std::path::Path) -> Option<MediaFile> {
            fixture
                .repo()
                .find_by_path(&path.to_string_lossy())
                .await
                .unwrap()
        }

        /// A moved file keeps its row (issue #180): the relink points it at
        /// the new path, records what was found there, and brings it back if
        /// it had been marked missing -- keeping its id, hash and title.
        #[tokio::test]
        async fn relink_moves_a_row_to_a_new_path_keeping_its_id_and_title() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            repo.mark_missing(vec![file.id], at(0)).await.unwrap();
            let moved_to = PathBuf::from(format!("/videos/{library}/moved/{}.mkv", Uuid::new_v4()));

            repo.relink(vec![to(&file, &moved_to, 4096, Some(at(30)))], Vec::new(), at(60))
                .await
                .expect("relink the row");

            let stored = repo
                .find_by_id(file.id)
                .await
                .unwrap()
                .expect("a relinked file is visible again");
            assert_eq!(stored.path, moved_to);
            assert_eq!(
                (stored.size_bytes, stored.mtime, stored.missing_since),
                (4096, Some(at(30)), None)
            );
            assert_eq!(
                (stored.hash, stored.content.clone(), stored.status),
                (file.hash, file.content.clone(), file.status),
                "the content and its title are kept"
            );
            assert!(
                at_path(&fixture, &file.path).await.is_none(),
                "nothing is left at the old path"
            );
            assert_eq!(at_path(&fixture, &moved_to).await.map(|f| f.id), Some(file.id));
        }

        /// Two files that swapped names swap paths in one relink, although
        /// neither path is free until the other row has left it: one row per
        /// path holds when the relink is done, not step by step.
        #[tokio::test]
        async fn two_rows_swap_paths_in_one_relink() {
            let fixture = $setup().await;
            let library = fixture.new_library().await;
            let heat = movie_file(&fixture, library).await;
            let ronin = movie_file(&fixture, library).await;

            fixture
                .repo()
                .relink(
                    vec![
                        to(&heat, &ronin.path, 7, Some(at(1))),
                        to(&ronin, &heat.path, 9, Some(at(2))),
                    ],
                    Vec::new(),
                    at(60),
                )
                .await
                .expect("the rows swap paths");

            let now_heat = at_path(&fixture, &ronin.path).await.unwrap();
            let now_ronin = at_path(&fixture, &heat.path).await.unwrap();
            assert_eq!((now_heat.id, now_heat.size_bytes), (heat.id, 7));
            assert_eq!((now_ronin.id, now_ronin.size_bytes), (ronin.id, 9));
            assert_eq!(now_heat.content, heat.content, "each keeps its title");
        }

        /// A rotation moves two rows along and displaces the third, whose
        /// content is nowhere: it is kept, missing as of the relink, at its
        /// displaced path -- which leaves its old path to the row moving in.
        /// A displaced row already missing keeps its first stamp.
        #[tokio::test]
        async fn a_displaced_row_is_kept_aside_as_missing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let (a, b, c) = (
                movie_file(&fixture, library).await,
                movie_file(&fixture, library).await,
                movie_file(&fixture, library).await,
            );
            let stale = movie_file(&fixture, library).await;
            let d = movie_file(&fixture, library).await;
            repo.mark_missing(vec![stale.id], at(0)).await.unwrap();

            repo.relink(
                vec![
                    to(&a, &b.path, 1, None),
                    to(&c, &a.path, 1, None),
                    to(&d, &stale.path, 1, None),
                ],
                vec![b.id, stale.id],
                at(60),
            )
            .await
            .expect("rotate");

            assert_eq!(at_path(&fixture, &b.path).await.map(|f| f.id), Some(a.id));
            assert_eq!(at_path(&fixture, &a.path).await.map(|f| f.id), Some(c.id));
            assert_eq!(at_path(&fixture, &stale.path).await.map(|f| f.id), Some(d.id));
            assert!(at_path(&fixture, &c.path).await.is_none());

            let aside = at_path(&fixture, &displaced_path(&b.path, b.id))
                .await
                .expect("the displaced row is kept");
            assert_eq!(aside.id, b.id);
            assert_eq!(aside.missing_since, Some(at(60)));
            assert_eq!(aside.content, b.content, "with its title");
            let stale_aside = at_path(&fixture, &displaced_path(&stale.path, stale.id))
                .await
                .unwrap();
            assert_eq!(stale_aside.missing_since, Some(at(0)), "the first stamp");
        }

        /// One row per path holds for a relink too: moving a row onto a path
        /// a row outside the relink holds is refused, and no row changes --
        /// not even those the same call would have moved.
        #[tokio::test]
        async fn relinking_onto_a_path_another_row_holds_changes_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let heat = movie_file(&fixture, library).await;
            let ronin = movie_file(&fixture, library).await;
            let moving = movie_file(&fixture, library).await;
            let holder = movie_file(&fixture, library).await;
            repo.mark_missing(vec![moving.id], at(0)).await.unwrap();

            let refused = repo
                .relink(
                    vec![
                        to(&heat, &ronin.path, 1, None),
                        to(&ronin, &heat.path, 1, None),
                        to(&moving, &holder.path, 1, Some(at(9))),
                    ],
                    Vec::new(),
                    at(60),
                )
                .await;

            assert!(refused.is_err(), "the path is taken");
            for row in [&heat, &ronin, &moving, &holder] {
                let stored = at_path(&fixture, &row.path)
                    .await
                    .expect("every row is where it was");
                assert_eq!(stored.id, row.id);
                assert_eq!(stored.missing_since, row.missing_since.or(
                    (row.id == moving.id).then(|| at(0))
                ));
            }
        }

        /// Two rows cannot be relinked to one path, and one row cannot be
        /// named twice.
        #[tokio::test]
        async fn a_relink_naming_a_path_or_a_row_twice_changes_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let heat = movie_file(&fixture, library).await;
            let ronin = movie_file(&fixture, library).await;
            let free = PathBuf::from(format!("/videos/{library}/{}.mkv", Uuid::new_v4()));

            let one_path = repo
                .relink(
                    vec![to(&heat, &free, 1, None), to(&ronin, &free, 1, None)],
                    Vec::new(),
                    at(60),
                )
                .await;
            let one_row = repo
                .relink(vec![to(&heat, &free, 1, None)], vec![heat.id], at(60))
                .await;

            // Two rows onto one path is the path's unique index refusing, so
            // its error is the store's own; a row named twice is refused by
            // the shared constructor before anything is written.
            assert!(one_path.is_err());
            assert_eq!(
                one_row.unwrap_err().to_string(),
                $crate::repositories::file::file_named_twice(heat.id).to_string()
            );
            assert!(at_path(&fixture, &free).await.is_none());
            assert_eq!(at_path(&fixture, &heat.path).await.map(|f| f.id), Some(heat.id));
            assert_eq!(at_path(&fixture, &ronin.path).await.map(|f| f.id), Some(ronin.id));
        }

        #[tokio::test]
        async fn relinking_a_row_that_does_not_exist_fails_and_changes_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let heat = movie_file(&fixture, library).await;
            let path = PathBuf::from(format!("/videos/{library}/{}.mkv", Uuid::new_v4()));
            let ghost = MediaFile {
                id: Uuid::new_v4(),
                ..heat.clone()
            };

            let not_found = $crate::repositories::file::file_not_found(ghost.id).to_string();
            assert_eq!(
                repo.relink(vec![to(&ghost, &path, 1, None)], Vec::new(), at(60))
                    .await
                    .unwrap_err()
                    .to_string(),
                not_found
            );
            assert_eq!(
                repo.relink(Vec::new(), vec![ghost.id, heat.id], at(60))
                    .await
                    .unwrap_err()
                    .to_string(),
                not_found
            );
            assert!(at_path(&fixture, &path).await.is_none());
            let stored = at_path(&fixture, &heat.path).await.expect("left where it was");
            assert_eq!(stored.missing_since, None);
        }

        /// The relink lookup sees a missing row -- the moved file's -- and
        /// never a row of another library, whatever its hash.
        #[tokio::test]
        async fn the_hash_lookup_includes_missing_rows_of_one_library_only() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other = fixture.new_library().await;
            // Positive and unique per run: the hash is a signed BIGINT column.
            let hash = (Uuid::new_v4().as_u128() as u64) >> 1;
            let present = file_with_hash(&fixture, library, hash).await;
            let missing = file_with_hash(&fixture, library, hash).await;
            let elsewhere = file_with_hash(&fixture, other, hash).await;
            let different = file_with_hash(&fixture, library, hash ^ 1).await;
            repo.mark_missing(vec![missing.id, elsewhere.id], at(0))
                .await
                .unwrap();

            let found = repo
                .find_by_library_and_hash_including_missing(library, hash)
                .await
                .unwrap();

            assert_eq!(ids(&found), sorted(vec![present.id, missing.id]));
            assert!(!ids(&found).contains(&different.id));
        }

        /// A file of a new title under `library_id` at `path`.
        async fn file_at(
            fixture: &impl FileRepositoryFixture,
            library_id: Uuid,
            path: PathBuf,
        ) -> MediaFile {
            let movie_entry_id = fixture.new_movie_entry(library_id).await;
            fixture
                .repo()
                .create(CreateMediaFile {
                    library_id,
                    path,
                    hash: 1,
                    size_bytes: 1024,
                    mtime: None,
                    identity: None,
                    mime_type: None,
                    duration: None,
                    container_format: None,
                    content: Some(MediaFileContent::movie(movie_entry_id)),
                    status: FileStatus::Known,
                    classifier_version: 0,
                    container_tags: None,
                })
                .await
                .expect("create a file")
        }

        /// The rows beneath a directory are those whose path continues it
        /// by whole components -- missing ones included, another library's
        /// never. A `_` or `%` in the directory's name is itself, not a
        /// wildcard.
        #[tokio::test]
        async fn the_rows_beneath_a_directory_are_matched_by_whole_components() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let other = fixture.new_library().await;
            let base = PathBuf::from(format!("/videos/{library}/{}", Uuid::new_v4()));
            let dir = base.join("Show_%1");
            let direct = file_at(&fixture, library, dir.join("a.mkv")).await;
            let nested = file_at(&fixture, library, dir.join("S01/b.mkv")).await;
            repo.mark_missing(vec![nested.id], at(0)).await.unwrap();
            // Longer by a character, and what the wildcards would match.
            file_at(&fixture, library, base.join("Show_%10/c.mkv")).await;
            file_at(&fixture, library, base.join("ShowAB1/d.mkv")).await;
            file_at(&fixture, library, base.join("Show_%1.mkv")).await;
            file_at(&fixture, other, dir.join("e.mkv")).await;

            let found = repo
                .find_beneath_including_missing(library, &dir)
                .await
                .unwrap();

            assert_eq!(ids(&found), sorted(vec![direct.id, nested.id]));
        }

        #[tokio::test]
        async fn empty_id_lists_change_nothing() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let library = fixture.new_library().await;
            let file = movie_file(&fixture, library).await;
            let gone = movie_file(&fixture, library).await;
            repo.mark_missing(vec![gone.id], at(0)).await.unwrap();

            assert_eq!(repo.mark_missing(Vec::new(), at(60)).await.unwrap(), 0);
            assert_eq!(repo.purge_missing(Vec::new()).await.unwrap(), 0);

            assert!(repo.find_by_id(file.id).await.unwrap().is_some());
            assert_eq!(
                ids(&repo
                    .find_all_by_library_including_missing(library)
                    .await
                    .unwrap()),
                sorted(vec![file.id, gone.id])
            );
        }
    };
}
