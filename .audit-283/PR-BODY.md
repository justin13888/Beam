## Summary

This pull request delivers #283's own work: the post-drain `/de-slop` audit and the gap audit against `docs/requirements/`, across `beam-server`, `beam-domain`, `beam-index`, `beam-entity`, `beam-migration`, `beam-auth`, `beam-client-core` and `beam-android`.

The audit's findings are filed as issues; this PR changes documentation only. It fixes the slice of findings that sits in `docs/requirements/` and the package READMEs, layer 1 of the de-slop. Everything else is in the filed issues, listed under **Filed** below.

Changed paths:
- `docs/requirements/functional.md`
  - FR-203's data-directory example names the artwork cache and trickplay sheets.
  - FR-301 and FR-312 name the TheTVDB adapter.
  - FR-308 drops the deferral to #71; the enrichment knobs exist (`beam-server/src/config.rs:141-184`).
  - FR-311 notes per-size artwork renditions and their `ETag`s (`beam-server/src/routes/artwork.rs:255-264`).
- `docs/requirements/non-functional.md`
  - NFR-107 drops the nonexistent "session refresh" endpoint and adds `GET /v1/people` to the search budget (`routes/mod.rs:104-107`).
  - NFR-202, NFR-501 and NFR-502 name TheTVDB, whose client has no cookie store and follows no redirect (`beam-index/src/providers/tvdb.rs:141-145`).
  - NFR-206 points at the enforced, ratcheted thresholds (`mise.toml:394`, `beam-web/vitest.config.ts:51-56`).
  - NFR-208 names the `ts:check` and `ts:typecheck` tasks.
  - NFR-403 separates process logs from the `admin_logs` table that the admin viewer reads.
  - NFR-406 says why Traefik needs no in-memory double.
- `docs/requirements/product.md`: TheTVDB, and the rate-limited endpoints.
- `beam-auth/README.md`: the `/v1` route prefix, the device-login routes, and `DeviceAuthStore` / `InMemoryDeviceAuthStore`.
- `beam-domain/README.md`
  - It now says the crate does depend on `sea-orm`, but only for `DbErr` in trait signatures.
  - The current model, repository and provider lists.
  - The `services` seams.
- `beam-entity/README.md`: points at `lib.rs` instead of a stale list of 19 modules (there are 36).
- `beam-index/README.md`: the `providers`, `disc` and `library_file` modules; `Clock` belongs to beam-domain; tests run over a real `TempDir`.
- `beam-migration/README.md`: the CLI reads `DATABASE_URL`, not `BEAM_DATABASE_URL`.
- `beam-server/README.md`: FFmpeg 9.0 (`Cargo.toml` `ffmpeg-next = "9.0"`, `beam-server/Containerfile`).

## How the audit ran

The audit ran ten independent read-only readers, each in its own worktree:
- documentation: one reader over all prose;
- implementation: four readers, one per package group;
- testing: four readers, one per package group;
- requirements gap: one reader, over `docs/requirements/` against the server and Android code.

Every claim carries `path:line`. The high-severity claims were re-derived against master@670b2de3 before filing.

| Layer | Findings |
|---|---|
| documentation | 49 (DOCS) + 6 (DOCFIX) |
| implementation | 32 server, 30 index, 33 domain/auth/entity/migration, 27 core/android |
| testing | 24 server, 58 index, 40 domain/auth/migration/entity, 40 core/android |
| requirements gaps | 3 (GAP-001..003) |

## Filed

FILL: the issue numbers from FILING.md, in order, each with its title.

## Validation

- Docs-only change; no code path changed.
- Commit hooks (`convco`, `hook-git-env`) passed on each commit.

## Risks and rollout

None. Documentation only.

## Issue
Closes #283

## Decisions taken
Issue 283 - Handoff: remaining server + Android drain, re-audit, and kynos/spargen bump
1. Scope of this pull request versus the filed issues.
   Taken: this PR carries the documentation fixes that sit in its manifest, `docs/requirements/` and the package READMEs. Every other accepted finding is filed as an issue with its findings, decisions and `Blocked by` edges, and the run drains it.
   Rejected: one pull request for the whole audit. It would touch all eight packages at once and could not be reviewed.
   Reverses: fold the filed issues' work into this branch.
2. The `de-slop` label.
   Taken: the issues are filed without a label. Creating a label is a repository write outside the run's surface. The run's scope is the whole repository, so every issue returned as a remainder is drained anyway.
   Rejected: creating the `de-slop` label.
   Reverses: `gh label create de-slop --repo justin13888/Beam`, then add it to the filed issues.
3. Surface questions.
   Taken: the run had no human channel, so each observable-surface change found was decided under the autonomy contract and recorded in its issue's `## Decisions taken`, with its blast radius. These include UniFFI API additions, the CORS policy, `5xx` detail text, the default log filter, enrichment tie-breaking and cross-package moves. Every crate is `publish = false` (`Cargo.toml:22`), so every caller is in this repository. The maintainer's words for this run are: "You make all decisions without humans ever."
   Rejected: leaving them unfiled until a human answers. Nothing would be delivered.
   Reverses: close the issue that carries the surface change.
4. Defects versus slop.
   Taken: findings that are behaviour defects, not shape, are filed as their own defect issues, separate from the behaviour-preserving slices:
   - a restored Android session reads as signed out on every cold start;
   - offline progress is never flushed;
   - the session cookie reaches off-origin media requests;
   - credentialed CORS reads are allowed from any origin;
   - `5xx` detail leaks internal error text;
   - a failed logout answers `204`;
   - auth and admin events are never logged;
   - enrichment tie-breaking contradicts its docs.

   The de-slop preservation rule governs only the slop slices.
   Reverses: fold them into the per-package slices.
5. Delivery order (de-slop layer contract).
   Taken: defects and documentation first, then each package's characterization tests, then that package's restructuring, then the file-length gate. The order is enforced by the `Blocked by` lines.
6. File-length threshold.
   Taken: each language's p90 over the scope (Rust 906 lines, 42 files; Kotlin 312 lines, 14 files), introduced against a baseline that may only shrink.
   Rejected: 2000/500 (10/3 files), which leave the audit's long files unbounded; p50, which puts half the tree on the baseline.
   Reverses: edit the numbers in the gate issue.
7. Section 3 of #283, the kynos/spargen bump.
   Taken: kynos is at 0.4.0, the newest release (#223 and #200 are closed). spargen is at 0.5.0, also the newest. Its remaining gaps (getkono/spargen#268, #403) are carried by a handoff issue that waits for the maintainer's word, so closing #283 loses nothing.
8. Kotlin mutation testing.
   Taken: introduce PIT as advisory tooling, outside `ci`, mirroring ADR-0011.
   Reverses: close the PIT issue.
