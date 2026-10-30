/// Behavioural contract for [`crate::repositories::MovieRepository`]: one
/// movie per identity key whatever enrichment does to the display title
/// (issue #183), search lists only movies with a present file, and orphaned
/// movies are deleted only once no file row is left.
///
/// `$setup` names an `async fn() -> impl MovieRepositoryFixture`.
#[macro_export]
macro_rules! movie_repository_contract {
    ($setup:path) => {
        use ::uuid::Uuid;
        use $crate::models::file::{CreateMediaFile, FileStatus, MediaFile, MediaFileContent};
        use $crate::models::movie::{CreateMovie, CreateMovieEntry, Movie};
        use $crate::providers::enrichment::MovieEnrichment;
        use $crate::repositories::MovieRepository;
        use $crate::repositories::contract::fixture::MovieRepositoryFixture;
        use $crate::utils::media_path::CLASSIFIER_VERSION;

        /// A movie parsed as a title of its own -- a fresh UUID keeps tests
        /// apart -- released in `year`.
        fn parsed(name: &str, year: Option<u32>) -> CreateMovie {
            CreateMovie::new(format!("{name} {}", Uuid::new_v4()), year, None)
        }

        /// [`parsed`] with no year.
        fn new_movie(name: &str) -> CreateMovie {
            parsed(name, None)
        }

        /// Give `movie` an entry, and -- when `with_file` -- a present file
        /// behind it, returning the file.
        async fn entry_for(
            fixture: &impl MovieRepositoryFixture,
            movie: &Movie,
            with_file: bool,
        ) -> Option<MediaFile> {
            let library_id = fixture.new_library().await;
            let entry = fixture
                .repo()
                .find_or_create_entry(CreateMovieEntry {
                    library_id,
                    movie_id: movie.id,
                    edition: None,
                })
                .await
                .expect("create an entry");
            if !with_file {
                return None;
            }
            let unique = Uuid::new_v4();
            Some(
                fixture
                    .files()
                    .create(CreateMediaFile {
                        library_id,
                        path: ::std::path::PathBuf::from(format!(
                            "/videos/{library_id}/{unique}.mkv"
                        )),
                        hash: (unique.as_u128() as u64) >> 1,
                        size_bytes: 1024,
                        mtime: None,
                        identity: None,
                        mime_type: Some("video/x-matroska".to_string()),
                        duration: None,
                        container_format: Some("matroska".to_string()),
                        content: Some(MediaFileContent::movie(entry.id)),
                        status: FileStatus::Known,
                        classifier_version: 0,
                        container_tags: None,
                    })
                    .await
                    .expect("create a movie file"),
            )
        }

        fn after_everything() -> ::chrono::DateTime<::chrono::Utc> {
            ::chrono::Utc::now() + ::chrono::Duration::minutes(1)
        }

        #[tokio::test]
        async fn find_or_create_entry_returns_one_entry_per_library_movie_and_edition() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = repo
                .find_or_create_by_identity(parsed("Editions", Some(2019)))
                .await
                .unwrap();
            let library = fixture.new_library().await;
            let other_library = fixture.new_library().await;
            let entry = |library_id: Uuid, edition: Option<&str>| CreateMovieEntry {
                library_id,
                movie_id: movie.id,
                edition: edition.map(str::to_string),
            };

            let default = repo
                .find_or_create_entry(entry(library, None))
                .await
                .unwrap();
            let again = repo
                .find_or_create_entry(entry(library, None))
                .await
                .unwrap();
            assert_eq!(
                again.id, default.id,
                "a second copy of the default edition shares its entry"
            );

            let cut = repo
                .find_or_create_entry(entry(library, Some("Director's Cut")))
                .await
                .unwrap();
            let cut_again = repo
                .find_or_create_entry(entry(library, Some("Director's Cut")))
                .await
                .unwrap();
            assert_eq!(cut_again.id, cut.id);
            assert_ne!(cut.id, default.id, "an edition is its own entry");
            assert_eq!(cut.edition.as_deref(), Some("Director's Cut"));

            let elsewhere = repo
                .find_or_create_entry(entry(other_library, None))
                .await
                .unwrap();
            assert_ne!(elsewhere.id, default.id, "another library is another entry");

            assert_eq!(
                repo.find_entries_by_movie_id(movie.id).await.unwrap().len(),
                3
            );
        }

        /// The reverse lookup continue-watching resolves a file's entry by:
        /// the entry reads back whole, and an id no entry has is `None`.
        #[tokio::test]
        async fn an_entry_is_found_by_its_id_whole_and_only_by_its_id() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = repo
                .find_or_create_by_identity(parsed("Entries", Some(2003)))
                .await
                .unwrap();
            let other = repo
                .find_or_create_by_identity(parsed("Other entries", Some(2004)))
                .await
                .unwrap();
            let library = fixture.new_library().await;
            let cut = repo
                .find_or_create_entry(CreateMovieEntry {
                    library_id: library,
                    movie_id: movie.id,
                    edition: Some("Extended".to_string()),
                })
                .await
                .unwrap();
            let theirs = repo
                .find_or_create_entry(CreateMovieEntry {
                    library_id: library,
                    movie_id: other.id,
                    edition: None,
                })
                .await
                .unwrap();

            let found = repo.find_entry_by_id(cut.id).await.unwrap().unwrap();
            assert_eq!(
                (
                    found.id,
                    found.library_id,
                    found.movie_id,
                    found.edition.as_deref(),
                    found.created_at
                ),
                (cut.id, library, movie.id, Some("Extended"), cut.created_at)
            );
            let found = repo.find_entry_by_id(theirs.id).await.unwrap().unwrap();
            assert_eq!((found.movie_id, found.edition), (other.id, None));
            assert!(
                repo.find_entry_by_id(movie.id).await.unwrap().is_none(),
                "a movie's id is no entry's"
            );
            assert!(
                repo.find_entry_by_id(Uuid::new_v4())
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn find_or_create_by_identity_returns_one_unchanged_row_per_key() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let create = parsed("Amelie", Some(2001));

            let first = repo
                .find_or_create_by_identity(create.clone())
                .await
                .unwrap();
            let mut later = create.clone();
            later.title = "Amélie.2001.1080p".to_string();
            later.runtime = Some(::std::time::Duration::from_secs(90 * 60));
            let again = repo.find_or_create_by_identity(later).await.unwrap();

            assert_eq!(again.id, first.id, "one movie per identity key");
            assert_eq!(
                again.title, create.title,
                "an existing movie is returned unchanged"
            );
            assert_eq!(again.runtime, None);
            assert_eq!(
                first.identity_key.as_deref(),
                Some(create.identity_key.as_str())
            );
        }

        #[tokio::test]
        async fn movies_of_one_title_and_different_years_are_different_movies() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let dune = format!("Dune {}", Uuid::new_v4());

            let lynch = repo
                .find_or_create_by_identity(CreateMovie::new(dune.clone(), Some(1984), None))
                .await
                .unwrap();
            let villeneuve = repo
                .find_or_create_by_identity(CreateMovie::new(dune.clone(), Some(2021), None))
                .await
                .unwrap();

            assert_ne!(lynch.id, villeneuve.id);
        }

        #[tokio::test]
        async fn an_enriched_movie_is_still_found_by_its_original_key() {
            // Issue #183: enrichment used to rewrite the column the indexer
            // matched on, so the renamed movie's next file made a duplicate
            // that then failed enrichment on the unique `tmdb_id`.
            let fixture = $setup().await;
            let repo = fixture.repo();
            let create = parsed("Leon", Some(1994));
            let movie = repo
                .find_or_create_by_identity(create.clone())
                .await
                .unwrap();

            repo.apply_enrichment(
                movie.id,
                &MovieEnrichment {
                    title: "Léon: The Professional".to_string(),
                    year: Some(1995),
                    ..Default::default()
                },
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();

            let found = repo.find_or_create_by_identity(create).await.unwrap();
            assert_eq!(found.id, movie.id, "the renamed movie is still found");
            assert_eq!(
                found.title, "Léon: The Professional",
                "enrichment's title stands"
            );
            assert_eq!(found.year, Some(1995));
            assert_eq!(
                found.identity_key, movie.identity_key,
                "the key is untouched"
            );
        }

        #[tokio::test]
        async fn find_by_ids_reads_every_named_movie_in_one_call() {
            let fixture = $setup().await;
            let repo: &dyn MovieRepository = fixture.repo();
            let one = repo
                .find_or_create_by_identity(new_movie("One"))
                .await
                .unwrap();
            let two = repo
                .find_or_create_by_identity(new_movie("Two"))
                .await
                .unwrap();

            let mut found: Vec<Uuid> = repo
                .find_by_ids(&[two.id, Uuid::new_v4(), one.id])
                .await
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect();
            found.sort();
            let mut expected = vec![one.id, two.id];
            expected.sort();
            assert_eq!(found, expected, "an unknown id is skipped");
            assert!(repo.find_by_ids(&[]).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn enrichment_replaces_the_runtime_and_rating() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = repo
                .find_or_create_by_identity(CreateMovie::new(
                    format!("Timed {}", Uuid::new_v4()),
                    None,
                    Some(::std::time::Duration::from_secs(90 * 60)),
                ))
                .await
                .unwrap();

            repo.apply_enrichment(
                movie.id,
                &MovieEnrichment {
                    title: movie.title.clone(),
                    runtime_mins: Some(121),
                    rating: Some(7.5),
                    ..Default::default()
                },
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();

            let stored = repo.find_by_id(movie.id).await.unwrap().unwrap();
            assert_eq!(
                stored.runtime,
                Some(::std::time::Duration::from_secs(121 * 60))
            );
            assert_eq!(stored.rating_tmdb, Some(7.5));
        }

        #[tokio::test]
        async fn enrichment_replaces_the_certification_and_clears_it_when_none_is_reported() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = repo
                .find_or_create_by_identity(new_movie("Certified"))
                .await
                .unwrap();
            assert_eq!(
                movie.content_rating, None,
                "none until enrichment reports one"
            );
            let enrich = |content_rating: Option<&str>| MovieEnrichment {
                title: movie.title.clone(),
                content_rating: content_rating.map(str::to_string),
                ..Default::default()
            };
            // Locking every field leaves the certification to the provider:
            // it is not a field an administrator edits.
            let every_lock: $crate::models::enrichment::FieldLocks =
                $crate::models::enrichment::MetadataField::ALL
                    .into_iter()
                    .collect();

            repo.apply_enrichment(movie.id, &enrich(Some("PG-13")), &every_lock)
                .await
                .unwrap();
            assert_eq!(
                repo.find_by_id(movie.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .content_rating
                    .as_deref(),
                Some("PG-13")
            );

            repo.apply_enrichment(
                movie.id,
                &enrich(None),
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
            assert_eq!(
                repo.find_by_id(movie.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .content_rating,
                None
            );
        }

        #[tokio::test]
        async fn library_ids_are_the_libraries_each_movie_is_associated_with() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let (one, two) = (fixture.new_library().await, fixture.new_library().await);
            let both = repo
                .find_or_create_by_identity(new_movie("In both"))
                .await
                .unwrap();
            let single = repo
                .find_or_create_by_identity(new_movie("In one"))
                .await
                .unwrap();
            let unassociated = repo
                .find_or_create_by_identity(new_movie("In none"))
                .await
                .unwrap();
            for (library, movie) in [(one, both.id), (two, both.id), (two, single.id)] {
                repo.ensure_library_association(library, movie)
                    .await
                    .unwrap();
                // Associating twice is one association.
                repo.ensure_library_association(library, movie)
                    .await
                    .unwrap();
            }

            let libraries = repo
                .library_ids(&[both.id, single.id, unassociated.id, Uuid::new_v4()])
                .await
                .unwrap();
            assert_eq!(libraries.len(), 2, "{libraries:?}");
            assert_eq!(libraries[&both.id], [one, two].into_iter().collect());
            assert_eq!(libraries[&single.id], [two].into_iter().collect());
            assert!(
                !repo
                    .library_ids(&[single.id])
                    .await
                    .unwrap()
                    .contains_key(&both.id),
                "only the movies asked about"
            );
            assert!(repo.library_ids(&[]).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn delete_orphaned_removes_only_fileless_movies_created_before_the_cutoff() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let bare = repo
                .find_or_create_by_identity(parsed("No entry", None))
                .await
                .unwrap();
            let entry_only = repo
                .find_or_create_by_identity(parsed("Entry without file", None))
                .await
                .unwrap();
            entry_for(&fixture, &entry_only, false).await;
            let kept = repo
                .find_or_create_by_identity(parsed("With file", None))
                .await
                .unwrap();
            entry_for(&fixture, &kept, true).await;
            // A fileless entry beside a present one goes; the movie stays.
            entry_for(&fixture, &kept, false).await;

            assert_eq!(
                repo.delete_orphaned(bare.created_at - ::chrono::Duration::seconds(1))
                    .await
                    .unwrap(),
                0,
                "nothing was created before the cutoff"
            );
            assert!(repo.find_by_id(bare.id).await.unwrap().is_some());

            assert_eq!(repo.delete_orphaned(after_everything()).await.unwrap(), 2);
            assert!(repo.find_by_id(bare.id).await.unwrap().is_none());
            assert!(repo.find_by_id(entry_only.id).await.unwrap().is_none());
            assert!(repo.find_by_id(kept.id).await.unwrap().is_some());
            assert_eq!(
                repo.find_entries_by_movie_id(kept.id).await.unwrap().len(),
                1,
                "only the entry a file references survives"
            );
        }

        #[tokio::test]
        async fn a_soft_deleted_file_keeps_its_movie_until_it_is_purged() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = repo
                .find_or_create_by_identity(parsed("Away", None))
                .await
                .unwrap();
            let file = entry_for(&fixture, &movie, true).await.unwrap();
            fixture
                .files()
                .mark_missing(vec![file.id], ::chrono::Utc::now())
                .await
                .unwrap();

            assert_eq!(repo.delete_orphaned(after_everything()).await.unwrap(), 0);
            assert!(repo.find_by_id(movie.id).await.unwrap().is_some());

            fixture.files().purge_missing(vec![file.id]).await.unwrap();
            assert_eq!(repo.delete_orphaned(after_everything()).await.unwrap(), 1);
            assert!(repo.find_by_id(movie.id).await.unwrap().is_none());
        }

        #[tokio::test]
        async fn an_unkeyed_movie_is_never_matched_and_can_be_keyed_once() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let create = parsed("Legacy", Some(1999));
            let legacy = fixture
                .new_unkeyed_movie(&create.title, ::chrono::Utc::now())
                .await;

            assert!(
                repo.find_unkeyed()
                    .await
                    .unwrap()
                    .iter()
                    .any(|m| m.id == legacy)
            );
            assert!(
                repo.assign_identity_key(legacy, &create.identity_key, CLASSIFIER_VERSION)
                    .await
                    .unwrap()
            );
            assert!(
                !repo
                    .find_unkeyed()
                    .await
                    .unwrap()
                    .iter()
                    .any(|m| m.id == legacy),
                "a keyed movie is no longer unkeyed"
            );
            assert_eq!(
                repo.find_or_create_by_identity(create).await.unwrap().id,
                legacy,
                "once keyed, the legacy movie is found by its key"
            );
            assert!(
                !repo
                    .assign_identity_key(legacy, "another key|", CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "a key is assigned once, never replaced"
            );
        }

        #[tokio::test]
        async fn unkeyed_movies_are_listed_oldest_first() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            // Whole seconds: Postgres keeps microseconds, the double keeps
            // nanoseconds, and the contract must not depend on either.
            let base = ::chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap();
            let title = format!("Legacy Movie {}", Uuid::new_v4());
            // Inserted newest first, so insertion order is not the answer.
            let newest = fixture
                .new_unkeyed_movie(&title, base + ::chrono::Duration::days(2))
                .await;
            let oldest = fixture.new_unkeyed_movie(&title, base).await;
            let middle = fixture
                .new_unkeyed_movie(&title, base + ::chrono::Duration::days(1))
                .await;

            let listed: Vec<Uuid> = repo
                .find_unkeyed()
                .await
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .filter(|id| [newest, oldest, middle].contains(id))
                .collect();
            assert_eq!(
                listed,
                vec![oldest, middle, newest],
                "of legacy duplicates, the original comes first"
            );
        }

        #[tokio::test]
        async fn an_unkeyed_movie_is_not_matched_and_cannot_take_a_held_key() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let create = parsed("Duplicate", Some(2001));
            let legacy = fixture
                .new_unkeyed_movie(&create.title, ::chrono::Utc::now())
                .await;

            let keyed = repo
                .find_or_create_by_identity(create.clone())
                .await
                .unwrap();
            assert_ne!(keyed.id, legacy, "a keyless row is never matched");

            assert!(
                !repo
                    .assign_identity_key(legacy, &create.identity_key, CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "a key another movie holds is refused"
            );
            assert_eq!(
                repo.find_by_id(legacy).await.unwrap().unwrap().identity_key,
                None
            );
            assert!(
                !repo
                    .assign_identity_key(Uuid::new_v4(), "nobody|", CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "an unknown movie is not keyed"
            );
        }

        #[tokio::test]
        async fn a_movie_key_records_the_rules_version_that_derived_it() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let current = repo
                .find_or_create_by_identity(new_movie("Current"))
                .await
                .unwrap();
            let stale = repo
                .find_or_create_by_identity(CreateMovie {
                    identity_key_version: 0,
                    ..new_movie("Stale")
                })
                .await
                .unwrap();
            // Whole seconds, as in the oldest-first test above.
            let base = ::chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap();
            let legacy = new_movie("Legacy");
            let older = fixture.new_unkeyed_movie(&legacy.title, base).await;
            assert!(
                repo.assign_identity_key(older, &legacy.identity_key, 0)
                    .await
                    .unwrap()
            );
            let fresh = new_movie("Fresh");
            let fresh_legacy = fixture.new_unkeyed_movie(&fresh.title, base).await;
            assert!(
                repo.assign_identity_key(fresh_legacy, &fresh.identity_key, CLASSIFIER_VERSION)
                    .await
                    .unwrap()
            );
            let unkeyed = fixture
                .new_unkeyed_movie(&format!("Unkeyed {}", Uuid::new_v4()), base)
                .await;
            let ours = [current.id, stale.id, older, fresh_legacy, unkeyed];

            let listed = |version: u16| async move {
                repo.find_keyed_before_version(version)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|t| t.id)
                    .filter(|id| ours.contains(id))
                    .collect::<Vec<Uuid>>()
            };
            assert_eq!(
                listed(CLASSIFIER_VERSION).await,
                vec![older, stale.id],
                "keys older rules derived, oldest first; never a keyless row"
            );
            let mut next = listed(CLASSIFIER_VERSION + 1).await;
            next.sort();
            let mut keyed = vec![current.id, stale.id, older, fresh_legacy];
            keyed.sort();
            assert_eq!(next, keyed, "every key is older than rules not yet written");
        }

        #[tokio::test]
        async fn a_movie_is_found_by_its_key() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let created = repo
                .find_or_create_by_identity(new_movie("Keyed"))
                .await
                .unwrap();
            let key = created.identity_key.clone().unwrap();

            assert_eq!(
                repo.find_by_identity_key(&key).await.unwrap().map(|t| t.id),
                Some(created.id)
            );
            assert!(
                repo.find_by_identity_key(&format!("nobody {}|", Uuid::new_v4()))
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn rekeying_a_movie_replaces_its_key_and_keeps_the_movie() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let old = CreateMovie {
                identity_key_version: 0,
                ..new_movie("Old Spelling")
            };
            let created = repo.find_or_create_by_identity(old.clone()).await.unwrap();
            repo.apply_enrichment(
                created.id,
                &MovieEnrichment {
                    title: "Provider Title".to_string(),
                    tmdb_id: Some(42),
                    ..Default::default()
                },
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
            let new = new_movie("New Spelling");

            assert!(
                repo.rekey(
                    created.id,
                    Some(new.identity_key.clone()),
                    CLASSIFIER_VERSION
                )
                .await
                .unwrap()
            );
            let stored = repo
                .find_by_identity_key(&new.identity_key)
                .await
                .unwrap()
                .expect("found by its new key");
            assert_eq!(stored.id, created.id, "the same row, not a new one");
            assert_eq!(stored.tmdb_id, Some(42), "with its enrichment");
            assert!(
                repo.find_by_identity_key(&old.identity_key)
                    .await
                    .unwrap()
                    .is_none(),
                "the old key is free"
            );
            assert!(
                !repo
                    .find_keyed_before_version(CLASSIFIER_VERSION)
                    .await
                    .unwrap()
                    .iter()
                    .any(|t| t.id == created.id),
                "the new key carries the version that derived it"
            );
            assert_eq!(
                repo.find_or_create_by_identity(new.clone())
                    .await
                    .unwrap()
                    .id,
                created.id,
                "the next file of the new spelling finds it"
            );
            assert!(
                repo.rekey(
                    created.id,
                    Some(new.identity_key.clone()),
                    CLASSIFIER_VERSION
                )
                .await
                .unwrap(),
                "a key it already holds is no clash"
            );
        }

        #[tokio::test]
        async fn rekeying_a_movie_refuses_a_held_key_and_can_release_its_own() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let a = repo
                .find_or_create_by_identity(new_movie("A"))
                .await
                .unwrap();
            let b = repo
                .find_or_create_by_identity(new_movie("B"))
                .await
                .unwrap();
            let b_key = b.identity_key.clone().unwrap();

            assert!(
                !repo
                    .rekey(a.id, Some(b_key.clone()), CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "another title holds it"
            );
            assert_eq!(
                repo.find_by_id(a.id).await.unwrap().unwrap().identity_key,
                a.identity_key,
                "a refused rekey changes nothing"
            );

            assert!(repo.rekey(b.id, None, CLASSIFIER_VERSION).await.unwrap());
            assert_eq!(
                repo.find_by_id(b.id).await.unwrap().unwrap().identity_key,
                None,
                "released: never matched again"
            );
            assert!(
                repo.rekey(a.id, Some(b_key.clone()), CLASSIFIER_VERSION)
                    .await
                    .unwrap(),
                "a released key is free"
            );
            assert!(
                !repo
                    .rekey(
                        Uuid::new_v4(),
                        Some("nobody|".to_string()),
                        CLASSIFIER_VERSION
                    )
                    .await
                    .unwrap(),
                "an unknown title is not rekeyed"
            );
        }

        /// A retitle replaces the title only while it is the one expected,
        /// and nothing else about the movie.
        #[tokio::test]
        async fn retitling_a_movie_replaces_only_the_title_it_expects() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let movie = repo
                .find_or_create_by_identity(new_movie("Movie - CD1"))
                .await
                .unwrap();

            assert!(
                !repo
                    .retitle_from(movie.id, "Another Title", "Movie")
                    .await
                    .unwrap(),
                "a title set since it was read is kept"
            );
            assert_eq!(
                repo.find_by_id(movie.id).await.unwrap().unwrap().title,
                movie.title
            );

            assert!(
                repo.retitle_from(movie.id, &movie.title, "Movie")
                    .await
                    .unwrap()
            );
            let stored = repo.find_by_id(movie.id).await.unwrap().unwrap();
            assert_eq!(stored.title, "Movie");
            assert_eq!(
                (stored.identity_key, stored.year),
                (movie.identity_key.clone(), movie.year),
                "only the title changes"
            );
            assert_eq!(
                repo.find_by_identity_key(movie.identity_key.as_deref().unwrap())
                    .await
                    .unwrap()
                    .map(|found| found.id),
                Some(movie.id),
                "the movie is still found by its key"
            );

            assert!(
                !repo
                    .retitle_from(Uuid::new_v4(), "Movie", "Other")
                    .await
                    .unwrap(),
                "an unknown movie is not retitled"
            );
        }

        #[tokio::test]
        async fn a_pin_finds_the_movie_pinned_to_it_before_one_matched_to_its_id() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let pin = ProviderPin::Tmdb(603);
            assert!(repo.find_by_pin(&pin).await.unwrap().is_none());

            let matched = repo
                .find_or_create_by_identity(new_movie("Matched"))
                .await
                .unwrap();
            repo.apply_enrichment(
                matched.id,
                &MovieEnrichment {
                    title: "Matched".to_string(),
                    tmdb_id: Some(603),
                    imdb_id: Some("tt0133093".to_string()),
                    anilist_id: Some(5114),
                    ..Default::default()
                },
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
            for by_id in [
                ProviderPin::Tmdb(603),
                ProviderPin::Imdb("tt0133093".to_string()),
                ProviderPin::Anilist(5114),
            ] {
                assert_eq!(
                    repo.find_by_pin(&by_id).await.unwrap().map(|t| t.id),
                    Some(matched.id),
                    "the movie enrichment matched to {by_id}"
                );
            }

            let pinned = repo
                .find_or_create_by_identity(new_movie("Pinned"))
                .await
                .unwrap();
            assert!(
                repo.set_pinned_ref(pinned.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert_eq!(
                repo.find_by_pin(&pin).await.unwrap().map(|t| t.id),
                Some(pinned.id),
                "a pin before a match"
            );
            assert_eq!(
                repo.find_by_id(pinned.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pinned_ref
                    .as_deref(),
                Some("tmdb:603")
            );
            assert!(
                repo.find_by_pin(&ProviderPin::Tvdb(81189))
                    .await
                    .unwrap()
                    .is_none(),
                "an id nothing carries finds nothing"
            );
        }

        #[tokio::test]
        async fn one_pin_pins_one_movie_and_a_movie_can_be_repinned() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let a = repo
                .find_or_create_by_identity(new_movie("A"))
                .await
                .unwrap();
            let b = repo
                .find_or_create_by_identity(new_movie("B"))
                .await
                .unwrap();
            let pin = ProviderPin::Anilist(5114);

            assert!(
                repo.set_pinned_ref(a.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert!(
                !repo
                    .set_pinned_ref(b.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "another movie holds the pin"
            );
            assert_eq!(
                repo.find_by_id(b.id).await.unwrap().unwrap().pinned_ref,
                None
            );
            assert!(
                repo.set_pinned_ref(a.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "pinning a movie to its own pin again is no clash"
            );

            assert!(
                repo.set_pinned_ref(a.id, &ProviderPin::Tmdb(1), PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert_eq!(
                repo.find_by_id(a.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pinned_ref
                    .as_deref(),
                Some("tmdb:1"),
                "a new pin replaces the old"
            );
            assert!(
                repo.set_pinned_ref(b.id, &pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "a released pin is free"
            );
            assert!(
                !repo
                    .set_pinned_ref(Uuid::new_v4(), &ProviderPin::Tvdb(7), PinSource::Nfo)
                    .await
                    .unwrap(),
                "no movie, no pin"
            );
        }

        #[tokio::test]
        async fn an_nfo_pin_never_replaces_an_administrators_pin_of_a_movie() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let title = repo
                .find_or_create_by_identity(new_movie("Pinned"))
                .await
                .unwrap();
            let stored = |title: Option<_>| {
                title.map(|t: $crate::models::Movie| (t.pinned_ref, t.pin_source))
            };

            assert!(
                repo.set_pinned_ref(title.id, &ProviderPin::Tmdb(1), PinSource::Nfo)
                    .await
                    .unwrap()
            );
            assert_eq!(
                stored(repo.find_by_id(title.id).await.unwrap()),
                Some((Some("tmdb:1".to_string()), Some(PinSource::Nfo))),
                "the pin is recorded with who set it"
            );
            assert!(
                repo.set_pinned_ref(title.id, &ProviderPin::Tmdb(2), PinSource::Admin)
                    .await
                    .unwrap(),
                "an administrator replaces an NFO's pin"
            );
            assert!(
                !repo
                    .set_pinned_ref(title.id, &ProviderPin::Tmdb(3), PinSource::Nfo)
                    .await
                    .unwrap(),
                "an NFO never replaces an administrator's pin"
            );
            assert_eq!(
                stored(repo.find_by_id(title.id).await.unwrap()),
                Some((Some("tmdb:2".to_string()), Some(PinSource::Admin)))
            );
            assert!(
                repo.set_pinned_ref(title.id, &ProviderPin::Tmdb(4), PinSource::Admin)
                    .await
                    .unwrap(),
                "an administrator replaces their own pin"
            );
            assert_eq!(
                stored(repo.find_by_id(title.id).await.unwrap()),
                Some((Some("tmdb:4".to_string()), Some(PinSource::Admin)))
            );
        }

        #[tokio::test]
        async fn enrichment_never_rewrites_a_movies_pin() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let title = repo
                .find_or_create_by_identity(new_movie("Pinned"))
                .await
                .unwrap();
            assert!(
                repo.set_pinned_ref(
                    title.id,
                    &ProviderPin::Imdb("tt0113277".to_string()),
                    PinSource::Nfo
                )
                .await
                .unwrap()
            );
            repo.apply_enrichment(
                title.id,
                &MovieEnrichment {
                    title: "Provider".to_string(),
                    tmdb_id: Some(949),
                    ..Default::default()
                },
                &$crate::models::enrichment::FieldLocks::none(),
            )
            .await
            .unwrap();
            assert_eq!(
                repo.find_by_id(title.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .pinned_ref
                    .as_deref(),
                Some("imdb:tt0113277")
            );
        }

        #[tokio::test]
        async fn enrichment_leaves_every_locked_field_and_writes_the_rest() {
            use $crate::models::enrichment::{FieldLocks, MetadataField};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let enrichment = MovieEnrichment {
                tmdb_id: Some(603),
                imdb_id: Some("tt0133093".to_string()),
                anilist_id: None,
                title: "The Matrix".to_string(),
                original_title: Some("Matrix".to_string()),
                description: Some("A hacker learns the truth.".to_string()),
                year: Some(1999),
                release_date: ::chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
                runtime_mins: Some(136),
                poster_url: Some("https://img/poster.jpg".to_string()),
                backdrop_url: Some("https://img/backdrop.jpg".to_string()),
                rating: Some(8.2),
                content_rating: Some("R".to_string()),
                genres: Vec::new(),
                credits: None,
                collection: None,
            };
            // Each field locked alone: that one keeps the value it had before
            // enrichment, and every other takes the provider's.
            for field in MetadataField::ALL
                .into_iter()
                .filter(|f| *f != MetadataField::Genres)
            {
                let title = repo
                    .find_or_create_by_identity(parsed("Locked", Some(1998)))
                    .await
                    .unwrap();
                let before = repo.find_by_id(title.id).await.unwrap().unwrap();
                let locks: FieldLocks = [field].into_iter().collect();
                // A provider id matches one movie: each gets its own.
                let tmdb_id = 900_000 + field as u32;
                let enrichment = MovieEnrichment {
                    tmdb_id: Some(tmdb_id),
                    imdb_id: Some(format!("tt{:07}", tmdb_id)),
                    ..enrichment.clone()
                };
                repo.apply_enrichment(title.id, &enrichment, &locks)
                    .await
                    .unwrap();
                let after = repo.find_by_id(title.id).await.unwrap().unwrap();

                let pick = |m: &Movie| -> String {
                    match field {
                        MetadataField::Title => m.title.clone(),
                        MetadataField::OriginalTitle => format!("{:?}", m.title_localized),
                        MetadataField::Description => format!("{:?}", m.description),
                        MetadataField::Year => format!("{:?}", m.year),
                        MetadataField::ReleaseDate => format!("{:?}", m.release_date),
                        MetadataField::Runtime => format!("{:?}", m.runtime),
                        MetadataField::Poster => format!("{:?}", m.poster_url),
                        MetadataField::Backdrop => format!("{:?}", m.backdrop_url),
                        MetadataField::Rating => format!("{:?}", m.rating_tmdb),
                        MetadataField::Genres => unreachable!("genres are not a movie column"),
                    }
                };
                assert_eq!(pick(&after), pick(&before), "{field:?} is locked");
                let written = [
                    (MetadataField::Title, after.title == "The Matrix"),
                    (
                        MetadataField::OriginalTitle,
                        after.title_localized.as_deref() == Some("Matrix"),
                    ),
                    (
                        MetadataField::Description,
                        after.description.as_deref() == Some("A hacker learns the truth."),
                    ),
                    (MetadataField::Year, after.year == Some(1999)),
                    (
                        MetadataField::ReleaseDate,
                        after.release_date == ::chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
                    ),
                    (
                        MetadataField::Runtime,
                        after.runtime == Some(::std::time::Duration::from_secs(136 * 60)),
                    ),
                    (
                        MetadataField::Poster,
                        after.poster_url.as_deref() == Some("https://img/poster.jpg"),
                    ),
                    (
                        MetadataField::Backdrop,
                        after.backdrop_url.as_deref() == Some("https://img/backdrop.jpg"),
                    ),
                    (MetadataField::Rating, after.rating_tmdb == Some(8.2)),
                ];
                for (other, was_written) in written {
                    if other != field {
                        assert!(
                            was_written,
                            "{other:?} is written while {field:?} is locked"
                        );
                    }
                }
                assert_eq!(after.tmdb_id, Some(tmdb_id), "the match is always written");
                assert_eq!(after.imdb_id, enrichment.imdb_id);
            }
        }

        #[tokio::test]
        async fn only_an_administrators_pin_is_cleared() {
            use $crate::models::pin::{PinSource, ProviderPin};
            let fixture = $setup().await;
            let repo = fixture.repo();
            let admin = repo
                .find_or_create_by_identity(new_movie("Admin pinned"))
                .await
                .unwrap();
            let nfo = repo
                .find_or_create_by_identity(new_movie("NFO pinned"))
                .await
                .unwrap();
            let bare = repo
                .find_or_create_by_identity(new_movie("Unpinned"))
                .await
                .unwrap();
            let admin_pin = ProviderPin::Tmdb(900_000 + (admin.id.as_u128() % 90_000) as u32);
            let nfo_pin = ProviderPin::Tmdb(800_000 + (nfo.id.as_u128() % 90_000) as u32);
            assert!(
                repo.set_pinned_ref(admin.id, &admin_pin, PinSource::Admin)
                    .await
                    .unwrap()
            );
            assert!(
                repo.set_pinned_ref(nfo.id, &nfo_pin, PinSource::Nfo)
                    .await
                    .unwrap()
            );

            assert!(repo.clear_admin_pin(admin.id).await.unwrap());
            let cleared = repo.find_by_id(admin.id).await.unwrap().unwrap();
            assert_eq!((cleared.pinned_ref, cleared.pin_source), (None, None));
            assert!(
                !repo.clear_admin_pin(admin.id).await.unwrap(),
                "nothing left to clear"
            );
            assert!(!repo.clear_admin_pin(nfo.id).await.unwrap());
            let kept = repo.find_by_id(nfo.id).await.unwrap().unwrap();
            assert_eq!(
                (kept.pinned_ref, kept.pin_source),
                (Some(nfo_pin.to_ref_string()), Some(PinSource::Nfo)),
                "an NFO's pin is the NFO's to change"
            );
            assert!(!repo.clear_admin_pin(bare.id).await.unwrap());
            assert!(!repo.clear_admin_pin(Uuid::new_v4()).await.unwrap());
            assert!(
                repo.set_pinned_ref(nfo.id, &admin_pin, PinSource::Nfo)
                    .await
                    .unwrap(),
                "the cleared id is free for another title"
            );
        }
    };
}
