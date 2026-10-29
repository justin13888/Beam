## Outcome

Once the maintainer says a spargen release carrying the open gaps is out, `beam-client-core` moves to that release as a crates.io requirement. The clients are regenerated with `mise run codegen:openapi`, the gap comments naming the fixed issues are removed, and whatever the release's correctness fixes surface is fixed.

This carries forward section 3 of #283, the handoff that this issue's parent closes. The kynos half of that section landed with the kynos 0.4.0 bump (#223, #200). Kynos 0.4.0 is still the latest kynos release, and its remaining open gaps are tracked in the kynos-gap issue filed beside this one.

## State at filing

- `beam-client-core/Cargo.toml:103` pins `spargen = "0.5.0"`, and 0.5.0 is the newest release on crates.io (`https://crates.io/api/v1/crates/spargen` reports `max_stable_version` 0.5.0).
- Open gap comments name getkono/spargen#268 (`beam-client-core/src/transport.rs:234, 375`; `ffi.rs:4714, 4830`; a `Decode` error does not keep the status) and getkono/spargen#403 (`beam-server/src/models/media/mod.rs:18`, tracked by #300).
- getkono/spargen#72, #82 and #85 are already fixed in 0.5.0. Their comments in `beam-client-core/build.rs:23, 45` and `transport.rs:151` correctly say why the omissions stay.

## Decisions taken

These are the run's decisions under the autonomy contract. Nobody has reviewed them.

1. This waits for the maintainer's word that the release is out, as #283 section 3 requires, so it is a handoff, not work for the current run.

## Constraints

- crates.io version requirements only, never a git revision (CLAUDE.md rule 3).

Blocked upstream: getkono/spargen#268, getkono/spargen#403.

Verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c. Carried forward from #283.
