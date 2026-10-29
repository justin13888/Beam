## Outcome

beam-client-core matches ADR-0012: `ffi.rs` only marshals, and the logic sits in the modules its doc names. The 5.3k-line `ffi.rs` and the 2k-line `catalog.rs` split by API resource. Dead session states are gone. Every exported operation's `# Errors` section names the errors it actually returns. The crate joins the mutation-testing baseline. Behaviour is unchanged. This is layer 2 (step 3), with its mutation pass (step 5).

## Findings

Each is `id path:line - finding - fix`.

- **core-IMPL-001 / android-IMPL-002 / J1**: `beam-client-core/src/paging.rs:30`. `CursorPager` is dead, while Android hand-rolls paging that diverges from it: the cursor is overwritten with null (`ExploreViewModel.kt:175`), and pages are concatenated without de-duplication (`HistoryViewModel.kt:77`). See decision 1.
- **core-IMPL-002**: `session.rs:94-124`. `CertificateRejected`, `CertificateTrusted` and `StoredSessionRestored` are never produced, and `SessionState::Untrusted` is never constructed. Delete those that the Android session issue's fix does not use.
- **core-IMPL-004 / -005 / -006 / -007**: `ffi.rs:834, 524-529, 954-959, 1039-1048`, plus seven operations that call `parse_uuid` without documenting it. Their `# Errors` sections are wrong or incomplete:
  - `NoPlayableSource` does not exist;
  - `list_servers` cannot fail;
  - `NoActiveServer` and `BadRequest` are undocumented on about 30 operations.

  Correct each one.
- **core-IMPL-008 / -009 / -010**: `ffi.rs:2020-2028, 2052-2055`, `tls.rs:55-58`, `ffi.rs:2336-2345`. Three docs are stale:
  - an `install` doc names a "metadata cache" that does not exist and omits that telemetry is lost on rebuild;
  - a pool doc names Media3, which fetches over OkHttp;
  - the `map_error` doc describes a parameter the function no longer has, and narrates its history.
- **core-IMPL-011**: `ffi.rs:1-7` against `171-222, 1244-1338, 1801-1862, 2505-2619`. Logic sits in `ffi.rs`. Move `plan_parts` into `capability/select.rs`, add `MediaSourceView::from_generated` there, and add `HistoryPage`/`AdminUserPage`/`AdminLogPage::from_generated` in `catalog.rs`.
- **core-IMPL-012**: `ffi.rs` (5344 lines). Split into `ffi/mod.rs`, `ffi/playback.rs`, `ffi/catalog.rs`, `ffi/progress.rs`, `ffi/telemetry.rs` and `ffi/admin.rs`, along the existing section markers. UniFFI accepts several `#[uniffi::export] impl` blocks.
- **core-IMPL-013**: `catalog.rs` (1991). Split into `catalog/{media,library,watching,admin}.rs`.
- **core-IMPL-014 / J3**: `Cargo.toml:121-123`. The `test-utils` feature has no consumer, and the `capability/builders.rs:3-6` doc falsely says Android shares the builders. Correct the doc, and gate the doubles on `cfg(test)` unless the Android native-test issue consumes them.
- **core-IMPL-015 / -016**: the dev tokio feature `rt-multi-thread` is unused. `ProgressThrottle::with_interval` and `RebufferDetector::is_sustained` are needlessly `pub`.
- **android-IMPL-004 / -005 / -006 / -007**: Kotlin duplicates four core values: `retryable` for `RateLimited`/`Storage`, the 15s report interval, `FLUSH_AT = 50`, and the cookie name. Export each from the core, and have Kotlin read it.
- **J5**: `ffi.rs:621, 972`. Login and stream URLs are hand-built. `playback_config` should take `stream_url` from `MediaSource`.
- **J6 / J7**: Android's `PartTimeline.kt:15-43` part-offset arithmetic, and the resolution labels (Android uses height, Apple width), are platform-independent. Move both into the core.
- **core-TEST-017**: there is no `mutation-testing` feature and no baseline row in `docs/testing.md:209-214`. Add the feature, run `mise run rust:mutants:crate beam-client-core` bounded to the crate, and record the row. Squash the survivors, or record them as findings.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. J1, paging.
   Taken: export `CursorPager` as a `uniffi::Object` and adopt it in Explore and History, which also fixes the null-cursor and duplicate-row divergence. ADR-0012 puts Relay paging in the core.
   Rejected: deleting `paging.rs` and keeping hand-rolled paging, which has already diverged.
   Reverses: delete the export and restore the Kotlin loops.
2. android-IMPL-004..007, J6, J7: move shared values and logic into the core and export them. The UniFFI API grows, and Android and Apple read the same values. All callers are in this repository.
   Reverses: inline the constants per platform.
3. J5: `playback_config` uses `MediaSource.stream_url`. Login stays hand-built until spargen can expose an operation URL; that is a spargen gap, filed upstream per CLAUDE.md rule 3 if one is not already open.

## Surface

UniFFI-exported API changes that are named and decided above: `CursorPager` and constant exports, `SessionState` variant removal, `list_servers` dropping `Result` if the lane chooses. Every caller is in this repository, and Android and Apple are updated in the same PR. Everything else is internal.

## Constraints

- Behaviour is preserved, except the paging de-duplication fix in decision 1.
- No gate is weakened.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
