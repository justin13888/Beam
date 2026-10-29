## Outcome

beam-domain, beam-auth, beam-entity and beam-migration carry no unused dependencies, dead types or unused mocks. Shared helpers live in the crate their callers imply. The two oversized contract files and the OIDC client split along their aggregate and concern boundaries. Every repository method documents its edge cases. Behaviour is unchanged. This is layer 2, precondition step 3.

## Findings

Each is `id path:line - finding - fix`.

**Dependencies**
- **domain-IMPL-001 / -002**: `beam-domain/Cargo.toml:16, 29`. `num` and `tracing` are unused. Delete them.
- **domain-IMPL-003**: `Cargo.toml:17`. `parking_lot` is used only by test-utils doubles. Make it optional under `test-utils`, with a dev-dependency.
- **migration-IMPL-001**: `beam-migration/Cargo.toml:17` and workspace `Cargo.toml:27`. `argon2` is unused. Delete both.
- **migration-IMPL-002**: `beam-migration/Cargo.toml:20`. Its `tokio` pins its own version. Use `workspace = true`.
- **auth-IMPL-001**: `beam-auth/Cargo.toml:53`. `tokio` is test-only. Move it to dev-dependencies.
- **auth-IMPL-002 / J-auth-1**: `beam-auth/Cargo.toml:50-51`. The comment names a file that no longer exists. Whether `http` is still needed as a >=1.4 floor is settled with `cargo tree -i http`; keep it only if it is.

**Dead code**
- **entity-IMPL-001**: `beam-entity/src/lib.rs:41-76`. All 36 root aliases are unused. Delete them.
- **entity-IMPL-002 / J-entity-1**: `user_access_policy.rs`. The entity is unused because the repository uses raw SQL (`beam-index/src/repositories/access_policy.rs:93,116,123`). See the decision below.
- **domain-IMPL-004 / -005 / -007**: `XXH3Hash` (`utils/hash.rs:11-31`), `utils/file.rs` `FileType`, and `InMemoryArtworkFetcher::call_count_for` are unused. Delete them.
- **domain-IMPL-006**: five `mockall::automock` attributes generate mocks nobody uses (chapter, applied_nfo, sidecar_subtitle, watch_state, trickplay). Drop them.
- **J-entity-2**: settle whether the `Serialize`/`Deserialize` derives on entity Models are used. Remove them if the workspace builds without them.

**Placement and duplication**
- **auth-IMPL-003 / -004**: `beam-auth/src/utils/hex.rs`. Its doc gives a false reason, and its one external user is the artwork cache key (`beam-server/src/services/artwork.rs:189`). Move it to `beam-domain/src/utils/hex.rs` and repoint the 4 callers.
- **domain-IMPL-008**: `repositories/genre.rs:42,52`. `sort_genre_names` and `slugify` are model rules. Move them to `models/genre.rs`.
- **auth-IMPL-005 / -006**: opaque-token mint and hash are byte-for-byte duplicated in `session_store.rs:154-169` and `device_auth_store.rs:105-118`, and seconds-to-duration conversion is done two ways (8 wrapping `as i64` sites). Keep one helper of each.
- **domain-IMPL-009**: `utils/telemetry.rs:79, 139`. Give `FileSizeBucket` the `TABLE` form that `CountBucket` uses.
- **domain-IMPL-017**: `utils/telemetry.rs:287-357, 564-655`. The playback classes go to `utils/playback_classes.rs`.
- **domain-IMPL-018**: disc detection is split across `path_policy.rs:48-131` and `media_path.rs:600-743`. Put both in `utils/disc.rs`.
- **domain-IMPL-019**: `models/mod.rs`. Add the missing `pub use language_preferences::*;`.
- **auth-IMPL-013**: `device_hash`, `derive_display_name` and `end_tolerance` are only called inside their crate. Make them `pub(crate)`.
- **auth-IMPL-010**: `oidc_config.rs:39`. A pointless `#[allow]`. Delete it.

**Doc comments**
- **auth-IMPL-007**: `session_store.rs:73-76,84`. It describes a JWT and password flow that no longer exists. Rewrite it for the cookie and device-grant flows.
- **auth-IMPL-008**: document `set_admin` on an unknown id (a no-op, as the contract says), and the edge cases of `update_oidc_profile` and `create`.
- **domain-IMPL-010 / -011 / -012 / -013**: document the undocumented methods of `LibraryRepository`, `MediaStreamRepository`, `AdminLogRepository`, `EnrichmentProvider`'s lookups, and `mark_enriched`/`mark_unmatched`: return values, ordering, missing-id behaviour, and which error variants they produce.

**Splits**
- **domain-IMPL-016**: `repositories/contract.rs` (9457 lines). Split into `contract/mod.rs` (the fixture) and one `contract/<aggregate>.rs` per macro. `#[macro_export]` paths are unchanged.
- **auth-IMPL-011**: `beam-auth/src/utils/oidc.rs` (1875). Split into `oidc.rs` (trait, types, NotConfigured), `oidc/discovered.rs` (the adapter and its tests) and `oidc/fake.rs`.
- **auth-IMPL-012**: `beam-auth/src/utils/contract.rs` (1138). One file per store contract.
- **migration-IMPL-003**: `beam-migration/tests/pg_integration.rs` (1794). Split into `upgrade_path.rs` and `migrations.rs`.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. Cross-package move auth-IMPL-004 (hex into beam-domain) and domain-IMPL-018 (one disc module).
   Taken: move them. Every crate is `publish = false`, so all callers are in this repository.
   Reverses: move them back.
2. entity-IMPL-002.
   Taken: switch `SqlAccessPolicyRepository` to the `user_access_policy` entity where it can express the query, and delete the entity where it cannot. The README's lockstep rule asks for an entity per table that code uses.
   Rejected: keeping an unused entity silently.
   Reverses: restore the module.
3. J-domain-1 (`sea_orm::DbErr` in the domain trait signatures) is accepted debt, recorded in ADR-0012. It is out of scope here.
4. J-auth-2 (`web_url` and `cookie_secure` in beam-auth's config) stays. They are grouped on purpose as deployment knobs (`oidc_config.rs:1-8`).
5. J-auth-3: the SQL stores keep `Uuid::new_v4()` for row ids, consistent with the other repositories. `IdGenerator` is used where a test must predict an id.

## Surface

Internal. In-workspace public items move, but every crate is unpublished.

## Constraints

- Behaviour is preserved.
- No gate is weakened.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
