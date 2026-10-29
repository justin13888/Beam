## Outcome

beam-client-core's façade is constrained by tests covering:
- every exported operation, including multi-server isolation;
- the progress-queue flush with its `Retry-After` handling;
- transport, TLS and storage failures produced through the injected backend and store;
- the invariants its docs state, through proptests (the crate has none today).

This is the characterization net the client-core implementation slice relies on (layer 3, step 2).

## Findings

Each is `id path:line - finding - test`.

- **core-TEST-002** `beam-client-core/src/ffi.rs:551`. No test registers more than one server (all 12 `add_server` calls use `https://beam.test`). Add `switching_servers_routes_calls_and_cookies_to_the_selected_one` and `removing_an_inactive_server_keeps_the_active_one`.
- **core-TEST-003** `ffi.rs:1456`. These are untested: `flush_progress` success, the wiring of `Retry-After` into `record_failure` (`ffi.rs:1485`, a bug that has happened before per `:1481-1484`), stop-at-first-failure, skipping a malformed id, and the `report_progress` Sent and Throttled outcomes. Add `a_flush_sends_every_due_sample_and_empties_the_queue`, `a_flush_stops_at_the_first_failure_and_honours_retry_after`, `a_sent_sample_retires_its_queued_entry` and `a_held_sample_is_throttled_at_the_facade`.
- **core-TEST-004** `transport.rs:509`. `CannedBackend` cannot produce a transport error or a response header. Extend it, then add `an_unreachable_server_queues_progress_and_reports_network`, `a_malformed_success_body_is_protocol` and `a_429_retry_after_reaches_the_queue`.
- **core-TEST-005** `ffi.rs:2356`. No test produces `UntrustedCertificate` through the façade. Add `a_rejected_handshake_surfaces_untrusted_certificate_with_details` (rcgen is a dev-dependency).
- **core-TEST-006** `ffi.rs:836`. `select_playback_source` is untested. Add `a_title_nothing_here_can_play_is_not_found_with_the_first_reason` and `selection_is_refused_before_the_profile_is_set`.
- **core-TEST-007** These 15 operations are never called by a test: `genres`, `libraries`, `library`, `health`, `continue_watching`, `history`, `revoke_session`, `admin_status`, `admin_users`, `set_user_disabled`, `admin_events`, `create_library`, `delete_library`, `refresh_media_metadata`, `library_files`. Add one canned-backend test each, asserting the request and the mapped result.
- **core-TEST-008** `catalog.rs:1159,1203,1254`. These conversions are untested: `AdminStatus`, `AdminUser` (the absolute avatar URL) and `AdminEvent` (category folding).
- **core-TEST-009** `ffi.rs:452`. `restore` is untested over an unreadable store, a corrupt index and a corrupt record.
- **core-TEST-010 / -011** `ports/kv.rs:195-267` and `clock.rs:79-91` test the doubles (a forbidden pattern). Replace the kv tests with a `KeyValueStore` contract that the Android test issue also runs over `DataStoreKeyValueStore`. Delete the clock tests.
- **core-TEST-013** `ffi.rs:2757, 2910`. Two assertions are loose: the secret and queue key are never checked as removed, and the logout test starts logged out. Add `logout_with_an_unreachable_server_still_signs_out_locally`.
- **core-TEST-014** `progress/mod.rs:136`. When samples coalesce, the larger position wins, not the newest, so a forced report after seeking back sends a stale position. Add `a_forced_report_after_seeking_back_sends_the_new_position`. Expect it to fail; that is a defect, and the fix makes the newest sample win.
- **core-TEST-016** No proptests exist. Add them for:
  - `MatroskaExtractor` never panics on arbitrary or truncated bytes (`demux/mkv.rs:142,195`)
  - `ByteSourceReader` returns exactly the source slice
  - `normalize_base_url` is idempotent and `server_id_for` is stable
  - `retry_after_secs` stays bounded
  - `classify` covers all u16
  - `ProblemDetail::parse` never panics
  - `ProgressThrottle` and `ProgressQueue` stay within their bounds
  - `TelemetryQueue` stays within its batch and total bounds
  - `select_source` picks one of its inputs, independent of input order
  - `progress_fraction` stays in [0, 1]

The restored-cookie 401 (core-TEST-001), the session transition invariants (core-TEST-012) and `absolute_url` (core-TEST-015) are delivered by the Android session and origin-scoping issues.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. core-TEST-014.
   Taken: fix it here. The newest sample wins a coalesce. A seek backward is the viewer's intent, and the server-side monotonic `Played` rule already protects watched state.
   Reverses: restore `held.position_secs > position_secs => held`.

## Surface

Internal: tests and proptest dev-dependencies. One behaviour fix: progress coalescing (core-TEST-014).

## Constraints

- Behaviour is preserved except core-TEST-014.
- No gate is weakened.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
