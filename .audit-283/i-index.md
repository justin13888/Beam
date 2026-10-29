## Outcome

In beam-index:
- the 9.3k-line `services/index.rs` splits along its identity, relink, walk and divergence boundaries;
- movie and show enrichment share one generic path, and candidate scoring exists once;
- dead seams, mocks and dependencies are gone;
- every documented function names its edge cases.

Behaviour is unchanged. This is layer 2, precondition step 3.

## Findings

Each is `id path:line - finding - fix`.

**Dependencies and dead code**
- **index-IMPL-004 / -005 / -006** `beam-index/Cargo.toml:30, 25, 18`. `regex` is unused, and the `serde` features on `num` and `chrono` are unused or redundant. Remove them.
- **index-IMPL-008 / -009** `services/index.rs:1364-1368, 1399-1405`. `with_id_generator` and `with_divergence_policy` have no callers. Decision J-02 keeps `with_id_generator` and uses it in tests; delete `with_divergence_policy` and make `DivergencePolicy` `pub(crate)`.
- **index-IMPL-010** `services/hash.rs:33, 80-89`. `HashService::hash_sync` has no production caller and forces 5 doubles to implement it. Remove it from the trait.
- **index-IMPL-011 / -012** `watcher.rs:50`, `runtime.rs:37`. The `MockFsWatcher` and `MockBackgroundIndexer` automocks are unused. Drop them.
- **index-IMPL-013** `services/index.rs:1133-1134`. `IndexError::InvalidId` is built only by a test helper, and beam-server matches on it (`beam-server/src/services/library.rs:678`). Make the helper take a `Uuid` and remove the variant and the arm.
- **index-IMPL-014** `services/mod.rs:14-18`. The `Clock` re-export shim has no users. Delete it.
- **index-IMPL-015** `probe/format.rs:9, 21-27`, `probe/metadata.rs:91, 205`, `probe/color.rs:147`. These are test-only or dead: the unused `Resolution` serde derives, and the methods called only from tests. Delete them with their tests.
- **index-IMPL-030** `services/index.rs:9`. The redundant `use serde_json;` is unused. Delete it.

**Duplication and placement**
- **index-IMPL-016** `services/enrichment/mod.rs:305-389, 418-632, 849-887`. Three movie/show pairs duplicate each other: search, resolve (about 110 lines each), and log-unmatched. Replace them with one generic `resolve<K: TitleKind>` over a private trait.
- **index-IMPL-017** `matcher.rs:136-287`, `control.rs:371-427`. Candidate scoring exists three times. Replace it with one `score_hits`. The NaN fix lands first, in the enrichment-correctness issue.
- **index-IMPL-018** `providers/cameo.rs:228-290`. Cameo's search fan-out loop re-implements `composite::search_all`. Share one helper.
- **index-IMPL-019** `enrichment/mod.rs:904-905, 951-952`. The delay-or-60s expression is written twice. Extract it into a private `after()`.
- **index-IMPL-020** `enrichment/mod.rs:50-64`, `index.rs:862`. Defaults are copied in beam-server `config.rs:109,147,153`. Keep one source: the server's `#[config(default)]` references the beam-index constants.
- **index-IMPL-007** `repositories/movie.rs:13-19`. `is_unique_violation` is shared, so move it to `repositories/mod.rs`.
- **index-IMPL-021 / -023** `services/notification.rs:82-100`, `repositories/enrichment.rs:79,288,338`. `AdminEvent::new` and three repository methods stamp `Utc::now()`/`Uuid::new_v4()` and bypass the `Clock` and `IdGenerator` seams. Inject them. index-TEST-008 in the beam-index test issue threads `now` through the trait.
- **index-IMPL-022 / J-01** `services/admin_log.rs:25-98`. `AdminLogService` is a 1:1 pass-through over `AdminLogRepository`: a second seam with its own NoOp double. See the decision below.

**Doc comments and edge cases**
- **index-IMPL-002** `enrichment/mod.rs:1-5`. It says production uses `NoopEnrichmentProvider` "for this commit". Rewrite it to name the composite, cameo and TheTVDB wiring (`beam-server/src/state.rs:19-22, 679`).
- **index-IMPL-026** `services/media_info.rs:21-33`. It speaks of "FFmpeg processes" where probing is in-process, and `new(0)` blocks forever. Reword it, and take a `NonZeroUsize`.
- **index-IMPL-027** `runtime.rs:104-117`. A zero interval spins `clock.sleep(0)`. Make the fields `NonZeroU64` (hardening step 2).
- **index-IMPL-028 / -029** `index.rs:4233-4242, 1136-1145`. `scan_all_libraries` does not document its saturating count and error contract, and the `PathNotFound` doc omits `begin_scan`.

**Splits**
- `services/index.rs` (9329): split it into `index_identity.rs` (874-1090 plus 3122-4200), `index_relink.rs` (523-860 plus 2276-2453), `index_walk.rs` (242-520) and `index_divergence.rs` (1100-1125 plus 2781-3015). Move the inline tests (5614-9329) out by behaviour into `index_streams_tests.rs`, `index_classify_tests.rs`, `index_process_tests.rs`, `index_scan_tests.rs` and `index_divergence_tests.rs` (index-IMPL-025). Share one harness.
- `enrichment/mod.rs` (2859): tests go to `service_tests.rs` via `#[path]`.
- `watcher.rs` (1283): tests and `in_memory` move out.
- `probe/metadata.rs`: tests move out.
- `index_identity_tests.rs`: split into backfill and rekey.
- `index_nfo_sidecar_tests.rs`: split into NFO, sidecar and container tests.
- `sql_shape_tests.rs`: one file per repository module.
- `index_relink_tests.rs`: split into planner tables and integration.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. J-01 (`AdminLogService`).
   Taken: delete it. Callers take `Arc<dyn AdminLogRepository>`, the error maps once at the beam-server route, and tests assert through `InMemoryAdminLogRepository`. CLAUDE.md asks for one seam per concern.
   Rejected: keeping a pass-through service. It is a second seam, and its NoOp double hides what was logged.
   Reverses: restore the service.
2. J-02: keep `with_id_generator` and use it in the scan tests so job ids are deterministic. `IdGenerator` is the canonical seam.
3. J-03: remove `IndexService::subscribe_scan`, which only tests call, if the scan tests can observe jobs through `NotificationService`. If they cannot, keep it and say why.
4. J-04: the telemetry and user-state SQL repositories stay in beam-index. The README declares `repositories` to be the sea-orm implementations of beam-domain's traits in general.
5. J-05: `BackgroundIndexer::library_repo()` gives way to passing the repository into `spawn_background_indexing_with`.
6. All of these are in-workspace trait or API changes. Every crate is `publish = false`.

## Surface

Internal. The OpenAPI document is unchanged.

## Constraints

- Behaviour is preserved.
- No gate is weakened.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
