## Outcome

Every claim in `docs/architecture/`, `docs/operations/`, `docs/testing.md` and `AGENTS.md` (that is, `CLAUDE.md`) matches the code at the delivered head. Where two files state the same rule, one states it and the other links to it. Present-tense docs carry no migration history; the ADRs keep that.

This is layer 1 of the post-drain de-slop. `docs/requirements/` and the package READMEs were already reconciled in the pull request that closes #283.

## Findings

Each is `id path:line - finding - fix`. The readings are given where the documentation and the code disagree.

- **DOCS-001** `docs/operations/configuration.md:27`. It says `BEAM_ENABLE_METRICS=false` mounts neither the endpoint nor the middleware. In the code both are always mounted (`beam-server/src/routes/mod.rs:248-249`), and `/metrics` answers 503 (`metrics_mw.rs:120-157`). Fix: describe the 503 and the no-op observer.
- **DOCS-002** `configuration.md:60` (and the `beam-server/src/config.rs:285-287` doc comment). It says `BEAM_RATE_LIMIT_ENABLED=false` means "no limiter mounted". The limiter is always mounted, and disabling it is a runtime exemption (`rate_limit.rs:255-264`). Fix: describe it that way.
- **DOCS-003** `configuration.md:8-10`, `docs/operations/deployment.md:101-104`. They imply `.env` values reach the compose container. `compose.beam.yaml:18-116` forwards an explicit list without `BEAM_TRUSTED_PROXIES`, `BEAM_RATE_LIMIT_*`, `BEAM_SHUTDOWN_TIMEOUT_SECS`, `BEAM_DB_{MAX,MIN}_CONNECTIONS`, `BEAM_ENRICH_*`, `BEAM_METADATA_LANGUAGE` or `BEAM_ARTWORK_*`. See the decision below.
- **DOCS-005** The search rate-limit class also covers `GET /v1/people` (`routes/mod.rs:104-107`). `security.md:238`, `config.rs:301` and `rate_limit.rs:11-12` say `/v1/media` only. Fix: name both.
- **DOCS-006** `docs/architecture/api.md:21`. The list of routes that skip the session cookie omits `/v1/auth/device` and `/v1/auth/device/token` (`routes/auth.rs:815-820, 895-906`).
- **DOCS-007** `api.md:354`, `components.md:299`, `ADR-0012:41`, `kynos-migration-readiness.md:20,243` say spargen 0.4.0. `beam-client-core/Cargo.toml:103` pins 0.5.0.
- **DOCS-008** `api.md:54,159-165`, `ADR-0015:59-85`, `configuration.md:44`. The artwork `size` parameter and its per-size renditions and `ETag`s are undocumented (`routes/artwork.rs:52-60,255-279`).
- **DOCS-009** `data-model.md:17-27`. `users.disabled` is missing (`beam-entity/src/user.rs`, `m20260711_000002_users_disabled.rs`).
- **DOCS-010** `data-model.md:47-48,61`, `ADR-0005:41-43`. They describe a periodic sweep of sessions and pending auths, and an `expires_at` column. The code says the sweep is future work (`beam-auth/src/utils/session_store.rs:183-187`), and the columns are `idle_expires_at` and `absolute_expires_at`.
- **DOCS-011** `security.md:31`, `components.md:177`. These call admin an "allowlist". It is the ID-token claim (`beam-auth/src/utils/admin_claim.rs:4`).
- **DOCS-012** `security.md:234`. It says a handler returns 500 on missing injected state. `Inject<T>` makes that a compile error (`components.md:44-45`). Fix: delete the bullet.
- **DOCS-013** `streaming.md:147`. It says the `ETag` is size-derived. It is derived from mtime and length (`routes/stream.rs:15-17,190-205`).
- **DOCS-014** `streaming.md:141`. It says an invalid or multi-range header gets 400, but `api.md:326-327` says 416. Fix: establish the answer with a `stream_tests.rs` case and make the docs say it.
- **DOCS-015 / DOCS-041** `streaming.md:4-6`, `overview.md:115-117`, `ADR-0004:36-37`, `ADR-0014:98-100`. They say "ffmpeg only reads metadata at index time" and "no derived artifacts". Trickplay decodes frames into `BEAM_DATA_DIR/trickplay` (`beam-index/src/probe/trickplay.rs:1-11`, `data-model.md:507-513`). Fix: scope the sentences to the request path and name trickplay sheets. Annotate ADR-0004 and ADR-0014.
- **DOCS-016** `overview.md:22-23,30-31,115`, `components.md:160-161`, `decisions/README.md:16`. TheTVDB is missing from the C4 diagram and the text (`beam-index/src/providers/tvdb.rs`, `composite.rs`).
- **DOCS-017** `overview.md:91`. "user, session" are not beam-domain types (`beam-auth/src/utils/models.rs`).
- **DOCS-018** `overview.md:92-93`. It says beam-domain has no sea-orm dependency. `beam-domain/Cargo.toml:23` declares it.
- **DOCS-019** `components.md:40-42,47-49,269-273`. The route and service file lists are stale (`routes/mod.rs:3-23`, `services/mod.rs`). Fix: point to the module files instead.
- **DOCS-020** `components.md:146`. It lists a `clock.rs` in beam-index. The clock is in beam-domain.
- **DOCS-021** `components.md:164-166,203-209`. The repository and model lists are stale (21 traits). Fix: point to `repositories/mod.rs`.
- **DOCS-022** `components.md:244`. `migrate up` should be `up`.
- **DOCS-023** `components.md:323,352,385`. The hard-coded test counts are stale. Fix: delete them.
- **DOCS-030** `docs/testing.md:5-10`. It quotes AGENTS.md text that no longer exists. Fix: quote the current invariant.
- **DOCS-031** `testing.md:298-302`. It says e2e steps 3-4 are `test.fixme`. They run (`beam-web/e2e/core-loop.e2e.ts:36-116`, #320).
- **DOCS-032** `testing.md:484`. It says web coverage is "not currently enforced", while `testing.md:113-114`, `ci.yml:513-526` and `vitest.config.ts:51-56` enforce it.
- **DOCS-033** `testing.md:396-403`, `ADR-0010:122-125`, `kynos-migration-readiness.md:18,185-190`. They say the router/document conformance test is obsolete. Its successor `beam-server/src/routes/contract_tests.rs` asserts the operation set equals the committed contract. Fix: say so.
- **DOCS-034** `AGENTS.md` rule 3 example. It says tags are declared on group scopes. Routes now name their own tag (`routes/mod.rs:74-79`). Fix: reword the example.
- **DOCS-035** `components.md:249-251`. It says ids are `Uuid::new_v4()` in `create()`. Be precise about which paths use `IdGenerator`.
- **DOCS-036 / DOCS-038 / DOCS-045** `kynos-migration-readiness.md` (322 lines) duplicates ADR-0010's Outcomes and keeps migration history in a present-tense doc (`docs/README.md:3-5`). `api.md:265-267,424-425` and `testing.md:396-408` carry Salvo-era narrative. See the decision below.
- **DOCS-037** `kynos-migration-readiness.md:145-147`. It says a file outside its library root is 403. The answer is 404 `#source-file-missing` (`routes/api_error.rs:1166-1195`, `security.md:219-222`).
- **DOCS-039 / DOCS-040** `ADR-0010:5-6,85-86`, `decisions/README.md:20` ("Kynos 0.1.0"); `ADR-0009:41-43` ("six crates"; the workspace has eight). Fix the Decision-section count, and annotate the status lines with the current version.
- **DOCS-042** `ADR-0011:11` says the coverage floor is 65%. Point it at the `rust:coverage` thresholds.
- **DOCS-043 / DOCS-044** The out-of-scope statement is kept in three places (`product.md:17-21,94-100`, `overview.md:162-169`). Link-handling, NFO limits and relink pairing rules are restated across FR-212, FR-219, FR-221, `security.md:170-227`, `data-model.md:147-243,409-434` and `components.md:81-147`. Fix: state each rule once, MUSTs in the requirements and state in data-model, and link from the others.
- **DOCFIX-005** `docs/requirements/functional.md`. Shipped `/v1` surfaces have no FR: watchlist and favourites, people, collections, per-user library access (`/v1/admin/users/{id}/access`), session listing and logout-all, language preferences, trickplay, chapters and skip markers, history, artwork sizes. Fix: add an FR per surface, stating what the code does today, so the next gap audit has something to check against.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. DOCS-003: the docs or compose are wrong.
   Taken: compose is wrong. `compose.beam.yaml` forwards every documented `BEAM_*` variable with its documented default, because `deployment.md` tells compose operators to set `BEAM_TRUSTED_PROXIES`, and the reverse-proxy deployment needs it.
   Rejected: documenting that compose ignores those variables. That would make the documented reverse-proxy setup impossible under compose.
   Reverses: drop the added forwards and add a "not forwarded" note.
2. DOCS-036: what happens to `kynos-migration-readiness.md`.
   Taken: fold its still-live status table into ADR-0010's Outcomes, delete the file, and repoint its six inbound links.
   Rejected: keeping it. It duplicates ADR-0010 and contradicts `docs/README.md`'s present-tense rule.
   Reverses: restore the file from history.
3. For every other contradiction above, the code is taken as correct and the docs are fixed. No behaviour changes in this slice. DOCS-014 is settled by a test, not by choosing a reading.

## Surface

Internal: documentation only, plus the compose forwarding in decision 1, which changes a deployment configuration surface by making documented variables work as documented.

## Constraints

- Behaviour is preserved, except the compose forwarding named above.
- No gate is weakened.
- A change that would alter any other observable surface is not made; it is filed as a new issue.
- No feature is added.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
