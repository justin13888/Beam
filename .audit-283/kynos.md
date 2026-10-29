## Outcome

Every place beam-server works around a Kynos limitation names the open getkono/kynos issue that tracks it, as CLAUDE.md rule 3 requires. Gap comments that kynos 0.4.0 already resolved are corrected. No local workaround remains without an upstream issue.

## Findings

- **server-IMPL-013** `beam-server/src/routes/delivery.rs:15-19, 139-161`. `RuntimeDelivery::responses` edits Kynos's emitted response description to add `content` keys. That is a local schema patch, and no upstream issue is named. kynos-0.4.0 `served.rs:335-366` takes `Delivery<M>` with a `const MEDIA_TYPE`, so a runtime media type is impossible and the gap is open. The module doc says the gap is "Recorded in `docs/architecture/kynos-migration-readiness.md`", but that doc's "Ranged media delivery" section (`:128-148`) says "Status: met", which is false.
- **server-IMPL-014** `beam-server/src/routes/auth.rs:1184-1209`. `SessionRevoked` hand-writes `IntoResponse`/`Responses` because `Reply` cannot hold two 204 variants. No upstream issue is named.
- **server-IMPL-015** `auth.rs:869-887`. It says "Kynos 0.3's `Auth<S>` binds one scheme… NOT YET FILED". Kynos 0.4.0 ships `Auth<AnyOf<(A, B)>>` (kynos-0.4.0 `src/security/requirement.rs:11,116,124`). Cookie-only is the ADR-0017 choice, not a gap.
- **server-IMPL-016** `routes/api_error.rs:1494-1499`. A store failure becomes 401 because `AuthRejection` has no 5xx (kynos-0.4.0 `src/error/rejection.rs:560-584`). The comment points at the readiness doc, which does not record it, and no issue is named.
- **server-IMPL-017** `beam-server/Cargo.toml:29-30`. The direct `futures-core` dependency exists because kynos does not re-export `Stream`. The readiness doc (`:125`) says "Filed upstream" but names no issue.
- **J-5** `routes/api_error.rs` has 115 literal `type = "https://beam.justinchung.net/reference/errors/#…"` strings. `taxonomy_tests.rs:94-140` keeps them aligned with `ERROR_BASE`. A `#[problem]` base-URI attribute would remove them.

Existing named gaps that are still open in 0.4.0 stay as they are: getkono/kynos#1021 (`middleware.rs:64`, `auth.rs:584`) and getkono/kynos#991 (`api_error.rs`, 7 sites).

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. Filing upstream.
   Taken: file one getkono/kynos issue per open gap (IMPL-013, -014, -016, -017, and J-5 as a feature request), each with a minimal reproduction: a code snippet against kynos 0.4.0. Name each issue in the comment at the workaround. The maintainer's own words authorise filing on repositories they own when a minimal reproduction can be given.
   Rejected: removing the workarounds locally, which would break the served document.
   Reverses: drop the issue references.
2. IMPL-015.
   Taken: rewrite the comment to say Kynos 0.4.0 supports `AnyOf`, and that cookie-only device sign-in is ADR-0017's decision.
3. The rate-limiter clock (server-IMPL-018, J-3) is not filed. `RateLimitPolicy` is a supported seam, as the readiness doc argues, so it is not a gap.

## Surface

Internal: comments, and issues filed upstream. No code behaviour changes.

## Constraints

- No local workaround is added or widened.
- When a later kynos release closes a gap, the fix is taken from that release, never patched in.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
