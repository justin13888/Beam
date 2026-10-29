## Outcome

beam-server's test suite constrains every authorization rule in both directions. It produces every NFR-205 error path through the injected trait, asserts the state mutation as well as the response, and contains none of CLAUDE.md's forbidden patterns. This is the characterization net that the beam-server implementation slice (moves, splits, dedupes) relies on. It is layer 3 of the post-drain de-slop, step 2 of the precondition chain.

## Findings

Each is `id path:line - finding - test to add or tighten`.

- **server-TEST-003** `beam-server/src/routes/admin.rs:463-469, 528-531`. No test calls `DELETE /v1/admin/libraries/{id}` or the admin event snapshot as a non-admin. Add `deleting_a_library_is_closed_to_a_regular_user_and_keeps_it` and `the_admin_event_snapshot_is_admin_only`.
- **server-TEST-004** `admin_tests.rs:877, 1688, 1829-1865`. Non-admin refusals check the status only. Line 1688 uses a malformed id. Tighten to `..._is_403_and_stores_nothing`, `..._is_403_and_queues_nothing` (real UUID) and `..._leaves_the_target_enabled_with_its_sessions`.
- **server-TEST-005** `routes/contract_tests.rs`. Nothing derives from the document that every `/v1/admin/*` operation demands the admin scope. Add `every_admin_path_demands_the_admin_scope` plus a runtime non-admin sweep.
- **server-TEST-006** `routes/api_error.rs:1500-1560`, `routes/auth.rs:1068-1072`. The authenticator's error mapping is untested: a session-store failure gives 401, a deleted admin gives 401, and `/v1/me` for a removed account gives `AccountRemoved`. Add `a_session_store_outage_is_401_not_500`, `an_admin_whose_row_is_gone_is_401_on_admin_routes` and `me_for_a_removed_account_is_its_problem_type`.
- **server-TEST-007** No database-failure (NFR-205) test exists for these modules: access, admin, auth, device_login, enrichment, genres, playback, stream, subtitle, trickplay and the `GET /v1/me/language-preferences` read (`grep -c 'INTERNAL_SERVER_ERROR\|#internal'` is 0 per file). Add one `a_<store>_failure_is_a_500_never_<wrong-answer>` per module, following `lists_tests.rs:596`.
- **server-TEST-008** `auth_tests.rs:619, 638, 656, 671, 690`. Callback failures assert 400 only. Also assert no session cookie, no user and no session stored.
- **server-TEST-009** `routes/auth.rs:719-770`. Untested: an expired login attempt (`STATE_TTL_SECS` via `TestClock`), a missing `state`, a missing `code`, and `NonceMismatch`. Add one test each.
- **server-TEST-011** `routes/mod.rs:79-87, 226-249`, `routes/rate_limit.rs`. Through `create_router`, nothing proves that login, callback and device start share one budget, or that browse and people search share one. The fail-open at `MAX_ENTRIES` and the request with no client address are untested.
- **server-TEST-012** `access_tests.rs:796-852, 702-726, 730-757`. Refused writes on hidden titles do not assert that nothing was written. HEAD on download, subtitle and trickplay is untested, as are season and episode artwork of a hidden show. A restricted administrator held to their access (`admin.rs:752-756`) is untested.
- **server-TEST-013** `services/library_tests.rs:399, 424, 449, 472`. These test the double (`InMemoryPathValidator` returns the configured result). Replace them with `a_root_the_validator_refuses_stores_nothing_and_publishes_nothing`.
- **server-TEST-014** `library_tests.rs:547-585, 646-678, 384-388`. These seed and assert through the double's private maps. Go through the trait instead.
- **server-TEST-015** `routes/media_tests.rs:347`. The sort parameters are asserted against a stub that ignores them. Add `sort_parameters_reach_the_browse_order` over the real service.
- **server-TEST-016** `models/search.rs:246-285`. This mirrors the implementation with a hand-listed set of variants. Iterate over `ALL`, as `models/enrichment.rs:441` does.
- **server-TEST-017** `models/admin.rs:758`. Destructure the wire `AdminEvent` fully.
- **server-TEST-018** `proptest-regressions/routes/stream_tests.txt`. This seed has no proptest. Add `a_ranged_stream_never_answers_outside_the_file` over `TestClient` and keep the seed.
- **server-TEST-019** `services/trickplay.rs:12-14`, the artwork cache (`state.rs:499`), and the WebVTT rendition. Nothing hermetic proves the library tree is byte-identical after trickplay or WebVTT generation (FR-202). Add `a_trickplay_pass_and_a_webvtt_rendition_leave_the_library_tree_unchanged`.
- **server-TEST-020** `tests/e2e_server/*`. These seams have no e2e coverage: per-user access over SQL, lists and collections, people search, artwork, trickplay, `/metrics`, rate limiting, the admin SSE stream, language preferences, download. Add `a_restricted_viewer_sees_only_their_libraries_over_postgres` and `lists_and_collections_round_trip_over_postgres` first.
- **server-TEST-021** `tests/e2e_server/scan.rs:86`, `catalog.rs:221`. These e2e cases re-prove unit-tested 404 and 422 cases. Cut them to what only e2e can show.
- **server-TEST-022** `config.rs:712, 867-875`. Two cases are untested: an unparseable env value (child-process pattern, `config.rs:1105`) and `DirCreationError`.
- **server-TEST-023** `routes/me.rs:68-80`. The `Err(too_many)` branch looks unreachable over HTTP (Kynos `maxItems` refuses first). Run hardening-loop step 1: delete the branch if it is unreachable, otherwise test it.
- **server-TEST-024** `routes/admin.rs:604-620`. The SSE `Lagged` skip policy is untested. Add `a_lagging_event_stream_skips_and_keeps_delivering`.

Cookie attributes, CORS and the redirect guard (server-TEST-001, -002, -010) are delivered by the CORS/cookie issue, and 5xx detail by the error-detail issue. They are not repeated here.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. Hardening-loop order for server-TEST-023.
   Taken: delete the branch if it is unreachable (CLAUDE.md step 1). Write a test only if it is reachable.
   Reverses: keep the branch and add a test that constructs the domain error directly.
2. server-TEST-021: shrink the duplicating e2e cases rather than delete the files. The real-`BEAM_VIDEO_DIR` wiring and the Dex-group non-admin are kept, because only e2e covers them.

## Surface

Internal: tests only, plus deletion of an unreachable branch if step 1 shows it is unreachable.

## Constraints

- Behaviour is preserved.
- No gate is weakened: no test deleted except the forbidden-pattern tests named above, each replaced by a test of the real subject.
- A change that would alter any observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
