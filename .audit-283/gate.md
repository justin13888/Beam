## Outcome

No Rust or Kotlin file in the server and Android scope grows without a bound. A `check:file-size` mise task, run in `mise run ci` and by CI, fails on any file whose total length exceeds the threshold, except the files on an explicit, visible baseline list. That list may only shrink, and the check itself refuses a path being added to it.

## Findings

There is no file-length gate today: `grep -i 'file-size\|max-lines\|tokei' mise.toml hk.pkl .github/workflows/ci.yml` finds nothing.

These distributions are total lines per file over `beam-server`, `beam-domain`, `beam-index`, `beam-entity`, `beam-migration`, `beam-auth`, `beam-client-core` (Rust, 409 files) and `beam-android` (Kotlin, 119 files), measured from the de-slop size report (tokei) at master@670b2de3:

- **Rust**: p50 200, p90 906, max 9457. Over 906: 42 files. Over 1500: 17. Over 2000: 10. Over 3000: 6.
- **Kotlin**: p50 96, p90 312, max 820. Over 312: 14 files. Over 500: 3. Over 660: 1.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. The thresholds.
   Taken: each language's own p90 in this scope. That is 906 lines for Rust and 312 for Kotlin. The baseline is every violating file at the time the gate lands. The implementation slices' splits are expected to shrink that list.
   Rejected:
   - 2000 / 500 (10 and 3 files). The files the audit called too long but left under the gate, such as `services/metadata.rs` at 1022 and `config.rs` at 1880, would stay unbounded.
   - p50. Half the tree would sit on the baseline by construction.

   Reverses: change the two numbers and regenerate the baseline once, which is the only time it may grow.
2. Where the baseline lives.
   Taken: a checked-in list of paths, one per line (e.g. `scripts/file-size-baseline.txt`), read by the mise task. It is a list, not a count, so one file cannot regress while another is fixed.
3. What the gate counts.
   Taken: total lines, which is what an editor's gutter shows. Generated files are excluded by path: `beam-client-core/api/openapi.json` and anything under `target/` or `build/`, which are not Rust or Kotlin anyway.

## Surface

Internal: a new CI gate, dev tooling only.

## Constraints

- The gate lands green against its baseline.
- It is never introduced disabled.
- No threshold is raised to let a file pass.
- No path is ever added to the baseline after it lands.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Found by the post-drain audit in #283.
