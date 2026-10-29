## Outcome

beam-index's tests constrain its scan, relink, enrichment, provider and runtime behaviours. Every NFR-205 error path is produced through the injected trait, the invariants its docs state are property-tested, the no-write guard on the source tree covers every scan path, and no test takes a double as its subject. This is the characterization net the beam-index implementation slice relies on (layer 3, precondition step 2).

## Findings

Each is `id path:line - finding - test`.

- **index-TEST-004** `beam-index/src/services/enrichment/mod.rs:339-347, 632-640, 744-752, 788-790, 812-815, 850-856, 618-621`. No test covers these errors: `fetch_due` Err, `apply_enrichment` Err, season or episode read/write Err, and a provider returning `NotConfigured`. Add `a_db_failure_fetching_due_rows_sweeps_nothing`, `a_failed_enrichment_write_leaves_the_title_unmatched`, `a_failing_season_read_still_enriches_the_show` and `an_unconfigured_provider_fails_the_row`.
- **index-TEST-005** `services/index.rs:5099ff`, `index_hints.rs:768,815,980,1095`, `index_sidecars.rs:51,60,70`. No scan or reconcile test makes `mark_missing`, `purge_missing`, `AppliedNfoRepository`, `SidecarSubtitleRepository`, `ChapterRepository` or `EnrichmentStateRepository` fail.
- **index-TEST-006** `beam-domain/src/repositories/library.rs:68-75` against `beam-index/src/repositories/library.rs:64-75`. The double's `count_files` is a pre-set map, so `last_scan_file_count` is unassertable. Add `a_scan_records_the_present_file_count_and_its_start_and_finish` once the library contract lands.
- **index-TEST-008** `beam-index/src/repositories/*.rs`. `Utc::now()` stamps where `Clock` should be (`grep -n Utc::now beam-index/src/repositories/*.rs`), and `mark_retrying` takes no `now`. Thread `now` through the trait, then add `a_retry_or_refresh_is_stamped_at_the_clocks_now`.
- **index-TEST-009** No shared `EnrichmentProvider` contract exists. The double returns `NotFound` for a foreign ref where TheTVDB returns `NotConfigured` (`providers/tvdb.rs:291-298`). Add a provider contract over InMemory, TheTVDB (local stand-in) and Composite.
- **index-TEST-010** `providers/tvdb.rs:393-410`. Untested: a malformed 200 body, and a transport error.
- **index-TEST-011 / -012** `providers/cameo.rs:228-290, 348-382`. The search fall-through and error-mapping arms are untested. Add a table test `every_cameo_error_maps_to_its_enrichment_error`.
- **index-TEST-013** `providers/composite.rs:246-253, 35-44`. The dispatch test cannot tell providers apart, and first-claimant-wins is untested.
- **index-TEST-014** `providers/artwork.rs:126-193`. `ReqwestArtworkFetcher::fetch` has no test of the body ceiling, the redirect cap, the https downgrade or a transport failure.
- **index-TEST-015 / -016 / -042** `services/enrichment/matcher.rs`. Missing proptests: `normalize` idempotent, similarity bounded and symmetric, scoring never panics. The fake tie test (`:429-445`) has no tie, and its helper calls `best_movie_match`. Absolute-numbered edge cases are untested.
- **index-TEST-017 / -018 / -019** `enrichment/mod.rs` tests `:2221-2315`. The rate-limit, backoff and refresh tests do not assert attempts, `next_attempt_at` or invalidation, and several use `Utc::now()` instead of the harness clock.
- **index-TEST-020** `services/index.rs:2953`. Untested: delta over threshold with the ratio under.
- **index-TEST-021** `index_hints.rs:155-205, 794-818`. An NFO that exists but does not parse is untested.
- **index-TEST-022** `index_nfo_sidecar_tests.rs:184-187, 2125`. The default harness on `RealClock` races `STAMP_SETTLE`, a wall-clock ordering dependency. Use a pinned `TestClock`.
- **index-TEST-023** `index_sidecars.rs:64, 162`. Subtitles under an unreadable folder must be kept, and no test proves it.
- **index-TEST-024 / -025 / -026 / -050 / -051** The no-write guards (FR-202) have gaps: relink, directory rename/remove events and disc folders run outside the snapshot (`index_nfo_sidecar_tests.rs:1590-1623`); trickplay (`trickplay.rs:16-59`), disc reading and probing have no tests; permission tests silently pass when run as root; and `O_RDONLY` handles are unasserted (`library_file.rs:658`).
- **index-TEST-027 / -047 / -031** Forbidden pattern, testing the double: `watcher.rs:776-825`, `notification.rs:383-395`, `admin_log.rs:159-207`. Delete them, and move the ordering assertions into the admin-log contract.
- **index-TEST-028** `watcher.rs:986-1020`. The symlink poll test stops at the marker. Drain it fully.
- **index-TEST-029** `runtime.rs:383-390, 530-535, 547, 574-575`. Untested: the recovery paths after a failing reconcile, `find_all` or `watch_library`.
- **index-TEST-030 / -032 / -043 / -045 / -046 / -048 / -053 / -055** Untested behaviours: event-log eviction and `recent_events(limit)`; `is_content_fault` over every variant; `ControlError` and `detail()`; `MetadataEnrichmentService::run` poke; the debounce boundary at 1999ms and the consumer stopping on close; worst-of-several divergence; disc size caps and IFO naming; color conversions, the `MediaInfoService` concurrency limit, `CancelOnDrop`.
- **index-TEST-034** `probe/color.rs:78-146`, `metadata.rs:1170`. 10/12/16-bit depths are unchecked, and `:1170` mirrors the implementation. Derive the expectation from `av_pix_fmt_desc_get`.
- **index-TEST-035 / -037 / -052** Missing proptests:
  - re-scan idempotence
  - relink is permutation-invariant
  - `beneath_pattern` matches only strict descendants
  - `checked_relative` never escapes
  - settle and throttle bounds
  - `plan_missing` partitions its input
  - the debouncer keeps the last kind
  - the innermost root wins
  - chapter end >= start
  - tile geometry
- **index-TEST-036 / -040 / -049 / -054 / -038 / -039** Loose assertions: limit/offset swapped in `sql_shape_tests.rs:704-715`; `is_some()` and `> 0.9` in cameo and matcher tests; relink-carry hash and size unchecked; tile-count ranges; TheTVDB edge cases (login without a token, 429 on login, the page cap, an HTTP-date `Retry-After`); `to_media_id_rejects_missing_separator` testing the wrong subject.
- **index-TEST-056 / -057 / -058** Two weaker duplicates of the missing-file tests (`index.rs:7526-7600, 8707`). Tests seed through the doubles' private maps. Wiring is copied into 33 `LocalIndexService::new(` calls and 8 `Harness` structs. Fix: one shared harness, and a `MediaFile` builder in beam-domain test-utils (NFR-207).

The tie-break, NaN and `fetch_due` ordering defects (index-TEST-001, -002, -033, -041) are delivered by the enrichment-correctness issue. The library, stream and admin-log contracts (index-TEST-003, -007) are delivered by the beam-domain test issue.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. Forbidden-pattern tests (index-TEST-027, -047, -031, -056).
   Taken: delete each, replacing it where the behaviour matters with a test of the real subject (`NotifyFsWatcher` on a `TempDir`; the admin-log contract).
   Reverses: restore them.
2. index-TEST-008 changes a trait signature internal to the workspace (`mark_retrying` gains `now`). All callers are in this repository, and nothing is published.
   Reverses: drop the parameter.
3. Root-run permission tests (index-TEST-050).
   Taken: a guard that fails when every permission test would have skipped, so CI can never pass vacuously.

## Surface

Internal: tests, test harness and builders, and the in-workspace `mark_retrying` signature.

## Constraints

- Behaviour is preserved.
- No gate is weakened.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Blocked by the beam-domain test issue, which adds the library, stream and admin-log contracts that index-TEST-006 needs.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
