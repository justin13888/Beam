## Outcome

Every repository and store trait in beam-domain and beam-auth is bound to a shared behavioural contract, over its in-memory double and over Postgres. The contracts pin the edge cases where the two implementations disagree today. The real OIDC client's security checks (PKCE, nonce, audience, issuer) are proven against the wire fake IdP. Migrations are proven ordered and reversible. This is the characterization net for the beam-domain, beam-auth, beam-entity and beam-migration implementation slice (layer 3, precondition step 2).

## Findings

Each is `id path:line - finding - test`.

**Contracts**
- **DOMAIN-TEST-001 / domain-IMPL-015 / index-TEST-003 / index-TEST-007 / index-IMPL-024**: `beam-domain/src/repositories/admin_log.rs:7`, `library.rs:10`, `stream.rs:9`. No contract exists for these. The doubles diverge from SQL: an unknown-id `update_scan_progress` is `Ok` in memory but errors in SQL, and `count_files` reads a configured map. `admin_logs` uses Postgres enums, which no pg test touches. Fix: add `admin_log_repository_contract!`, `library_repository_contract!` and `media_stream_repository_contract!`, bound over the doubles and in `beam-index/tests/pg_integration`, and make the double's `count_files` derive from its files.
- **DOMAIN-TEST-003**: `enrichment.rs:22, 108`. `backfill_missing` has no caller and no test, and its in-memory version is a stub. Delete it (hardening step 1). Add contract case `count_by_status_counts_rows_per_status`.
- **DOMAIN-TEST-004**: `movie.rs:134`, `show.rs:167`. `find_entry_by_id` and `apply_season_enrichment` are outside the contracts.
- **DOMAIN-TEST-014**: `models/applied_nfo.rs:48-66`, `models/sidecar.rs:111-127`. The contracts assert through `matches()` and `same_content()`, which have no tests of their own. Add table tests, and assert fields explicitly in the contract.
- **AUTH-TEST-008 / -011 / -012**: `beam-auth`. Gaps: no duplicate-identity refusal (the unique index is at `m20260705_000001_oidc_auth.rs:27-35`; the double accepts duplicates); a malformed user id gives `InvalidId` in SQL, but the double never parses it; a pending state minted twice overwrites in memory but fails on the PK in SQL. Add one contract case each.

**OIDC client (security)**
- **AUTH-TEST-001** `beam-auth/src/utils/oidc.rs:543-563`. The real `begin_auth` is untested, and the fake IdP never verifies the PKCE challenge. Removing `.set_pkce_challenge` passes every test. Add `the_authorization_url_carries_an_s256_challenge_of_the_returned_verifier_state_nonce_and_scopes`, and make the fake IdP verify the verifier against the challenge.
- **AUTH-TEST-002** `oidc.rs:582-585`. The real nonce check is untested. The fake returns `NonceMismatch`, a variant the real client never produces. Add `a_code_exchange_whose_id_token_names_another_nonce_is_refused` and `...another_audience_or_issuer_is_refused`, and make the fake return the real variant.
- **AUTH-TEST-003 / -004** `oidc.rs:365-376, 515-539, 580-669`. Untested: discovery failures, `MissingIdToken` and `DeviceFlowUnsupported`; claim mapping beyond subject and issuer; every `DeviceAuthStart` field.
- **AUTH-TEST-005 / -006 / -007 / -014** `login.rs:82-181`, `session_store.rs:62-64`. Untested: `complete_login`'s database-failure paths (no failing double exists), profile refresh on a later login, `get_and_touch` under a failing store, and an empty-local-part email.
- **AUTH-TEST-009 / -010** `contract.rs:88-101`, `sql_shape_tests.rs:198-218`. One test restates its input, and one cannot reach the UPDATE it describes (`empty_mock`). Add `create_keeps_the_admin_flag_it_is_given`, and seed a row before the update.

**Forbidden patterns and loose assertions**
- **DOMAIN-TEST-005** `models/search.rs:13`. `title_match_score` is used only by the catalog double, its tests test scaffolding, and its doc says `(0.0, 1.0]` while it returns 3.0. Move it into the `in_memory` module and fix the doc.
- **DOMAIN-TEST-006 / -007 / -008**: `services/id.rs:84` (tests `SequentialIdGenerator`), `providers/enrichment.rs:545-580` (restates the Noop constants), `providers/artwork.rs:331-338` (assert nothing). Delete them, or replace them with a real property.
- **DOMAIN-TEST-015** `contract.rs` (15 sites). Refusals checked only with `is_err()`. Assert the shared error constructor's message.
- **DOMAIN-TEST-016** `utils/subtitle.rs:528-541`. A 5-second wall-clock bound. Assert linearity structurally instead.

**Invariants**
- **DOMAIN-TEST-009 / -010 / -011 / -012 / -018**: add proptests for these documented invariants:
  - `display_cmp` is a total order
  - `LanguageCode::parse`
  - `normalize_webvtt` is idempotent
  - `CLASSIFIER_VERSION` is bumped when the corpus classifications change (pin a digest)
  - `ExternalMediaRef` round-trips
  - `markers_from_chapters` gives unique kinds, sorted
  - `compare_title_credits` is total

**Error paths and untested behaviour**
- **DOMAIN-TEST-013 / -017**: no tests for the entity-to-domain `TryFrom` refusals, `is_resumable`/`is_finished`, `applies_to`, or a negative `runtime_mins` wrapping.

**Migrations and entities**
- **MIGRATION-TEST-001** `beam-migration/src/lib.rs:47-84`. Add hermetic `migrations_are_registered_in_strictly_ascending_name_order`.
- **MIGRATION-TEST-002** `beam-index/tests/pg_integration/schema.rs:13-42`. The up/down check is loose. Assert the exact table set; after down, assert no tables, types or indexes; then up again.
- **MIGRATION-TEST-003**: 26 of 34 migrations have no dedicated test. Start with `users_oidc_only` (data-changing) and the no-op down of `m20260704_000004`.
- **ENTITY-TEST-001**: add pg test `every_entity_reads_and_writes_against_the_migrated_schema`, `admin_log` first.

**Builders (NFR-207)**
- **DOMAIN-TEST-019 / AUTH-TEST-013 / index-IMPL J-07**: add `test_utils` builders for `MediaFile`/`CreateMediaFile`, `Library`, `EnrichmentState`, `CreateUser`, `SessionData` and `PendingAuth`. Switch the cited literals over.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. When a contract case exposes a double/SQL disagreement, which side moves.
   Taken: the SQL implementation's behaviour is the contract, and the double is corrected to match it. Postgres is what ships.
   Exception: `update_scan_progress` on an unknown id. Both are made `Ok(())` no-ops, consistent with `UserRepository::set_admin`'s documented "no-op for an unknown id" rule.
   Reverses: the opposite choice per case, recorded in the PR.
2. `backfill_missing`: delete it, as it has no caller (hardening step 1).

## Surface

Internal: tests, contracts, builders, and test doubles. Deleting `backfill_missing` touches a trait internal to the workspace with no caller.

## Constraints

- Behaviour is preserved, except the `update_scan_progress` unknown-id alignment named above, which the PR states.
- No gate is weakened.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
