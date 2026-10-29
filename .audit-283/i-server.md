## Outcome

In beam-server, every symbol sits in the module its callers imply, the duplicated paging, cursor, error-widening and test-wiring code exists once, dead code and unused dependencies are gone, business rules live in services rather than route handlers, and every file over the agreed threshold is split along a named cohesion boundary. Behaviour is unchanged.

This is layer 2 of the post-drain de-slop, precondition step 3. Doc comments that travel with moved symbols are corrected in the same change (step 4).

## Findings

Each is `id path:line - finding - fix`.

**Dependencies and dead code**
- **server-IMPL-001 / -002** `beam-server/Cargo.toml:40, 53`: `rand` and dev `futures` are unused (grep printed nothing). Delete both.
- **server-IMPL-003 / -004** `Cargo.toml:45, 12`: `tempfile` and `beam-entity` are used only in tests. Move them to `[dev-dependencies]`.
- **server-IMPL-005** `Cargo.toml:17`: `cameo` is pinned separately for one type (`state.rs:695`). Make it a `[workspace.dependencies]` entry, or re-export the type from beam-index.
- **server-IMPL-006 / -007 / -008 / -009**: `src/utils/mod.rs` has no caller; `models/file.rs` `FileID` is dead; `AppServices.hash` is never read in production, and three test stubs exist only to fill it; `InMemoryDependencyProbe::set_database_healthy` has no caller. Delete them.
- **server-IMPL-030** `services/{hash,media_info,admin_log,notification}.rs`: re-export shims give beam-index symbols two paths. Import the beam-index paths and delete the shims.

**Misplaced symbols and duplication**
- **server-IMPL-019** `routes/playback.rs:160-245`: `caller_access`, `hides`, `media_not_found`, `page_size` and `parse_user_id` are imported by 9 other route modules. Move them to `routes/access.rs`.
- **server-IMPL-020**: page-size validation is copied 6 times (`playback.rs:240`, `enrichment.rs:98-103`, `services/{metadata,people,lists,collections}.rs`), and the default/max constants 4 times. Replace with one `services::paging::page_size(..)`; message strings stay byte-identical.
- **server-IMPL-021** `services/{cursor,enrichment_cursor,history_cursor,library_files_cursor,user_list_cursor,offset_cursor}.rs`: six copies of the version, error and base64url-JSON seal/open code. Move to `services/cursor/` with a shared generic envelope; cursor tokens stay byte-identical.
- **server-IMPL-022 / -026** `routes/admin.rs:672-716, 756-826`: "cannot disable self", "revoke sessions on disable" and access-policy validation live in handlers, and the `find_by_id → UserNotFound` lookup is repeated three times. Move them to `services/admin_users.rs` and map the domain error once.
- **server-IMPL-023** `routes/auth.rs:895-1058`: the RFC 8628 poll state machine is about 160 lines in a handler. Move it to `beam_auth::utils::login::poll_device_login`, beside `complete_login`.
- **server-IMPL-024** `routes/api_error.rs:1564-1597`: DI wiring sits in the error module. Move it to `state.rs`.
- **server-IMPL-025** `api_error.rs:1350-1404`: eight identical `From<InternalError>` impls. Replace with a `widen_internal!` macro.
- **server-IMPL-027** `api_error.rs:1410`: `AuthenticatedUser.user_id: String` is re-parsed in four places. Parse once; the field becomes `Uuid`, and `parse_user_id` goes.
- **server-IMPL-028** `services/sources.rs:54-66, 629-635`: route paths are hard-coded in the service and restated in tests. Tests follow the URL through `TestClient` instead.
- **server-IMPL-029** `services/library.rs:406`: `#[allow(clippy::too_many_arguments)]` with no reason. Add a `LibraryServiceDependencies` struct, as the sibling services have.
- **server-IMPL-031**: `StubHashService` is defined 3 times and `StubMetadataService` 4 times, and the 31-field `AppServices` literal is repeated in about 15 test files. Add one `AppServicesBuilder` in `routes/test_support.rs`.
- **server-IMPL-032** `routes/auth.rs:66-248`: response types live in a route file. Move them to `models/auth.rs`; schema names are unchanged.
- **J-1** `Cargo.toml:72`: no crate depends on beam-server. Make its doubles plain `#[cfg(test)]` if no `test-utils` path is production code, and remove the `mutation-testing` dependency on the feature accordingly.

**Doc comments**
- **server-IMPL-010** `services/library.rs:342-344, 432`: the doc says "by user ID / None if not found", but the function returns every library and ignores the id. Drop the parameter and rewrite the doc.
- **server-IMPL-011 / -012**: `get_library_by_id` and `delete_library` don't document their `None`/`false` meanings; `get_media_sources` doesn't document `Unsupported` or `MediaNotFound`. Document them.

**File splits** (threshold decided in the size-gate issue; boundaries from the audit)
- `config.rs` (1880): move tests 881-1880 to `config_tests.rs`.
- `routes/api_error.rs` (1597): authentication (1406-1563) goes to `routes/session_auth.rs`; DI goes to `state.rs`.
- `routes/auth.rs` (1246): wire types go to `models/auth.rs`; the device flow goes to beam-auth.
- `services/metadata.rs` (1022): paging types go to `services/paging.rs`; mapping helpers (182-412) go to `services/metadata/mapping.rs`.
- `routes/admin_tests.rs` (2755): split by its own section headers into libraries, logs/events/refresh, and users/status/access.
- `services/metadata_tests.rs` (2607): source ranking goes to `services/sources_tests.rs`; `mod browse` goes to `metadata_browse_tests.rs`.
- `media_tests.rs`: browse goes to `media_browse_tests.rs`.
- `auth_tests.rs`: sessions go to `session_tests.rs`.
- `playback_tests.rs`: continue-watching moves to its own file.
- `library_tests.rs`: root-conflict and startup-audit move to their own file.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. Cross-package move server-IMPL-023 (device poll flow to beam-auth).
   Taken: move it. `complete_login` already lives there, and every crate is `publish = false` (`Cargo.toml:22`), so all callers are in this repository.
   Reverses: move it back into `routes/auth.rs`.
2. server-IMPL-010 drops an ignored parameter from an in-workspace service method; it is internal for the same reason.
3. J-4: the services keep returning `crate::models` DTOs. CLAUDE.md forbids HTTP requests, responses and extractors in services, not serde DTOs that derive `kynos::Schema`.
   Reverses: introduce domain result types and map them in routes.

## Surface

Internal. The served OpenAPI document must be byte-identical before and after (`mise run codegen:openapi:check`), as must cursor tokens and error message strings.

## Constraints

- Behaviour is preserved.
- No gate is weakened.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
