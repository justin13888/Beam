# ADR-0001: Modular monolith; absorb the indexer in-process; rename beam-stream → beam-server

## Status

Accepted.

## Context

Previously, `beam-index` ran two ways simultaneously: linked in-process as a library into
`beam-stream` (sharing sea-orm repositories against the same Postgres database), and also built as
a standalone binary exposing a single gRPC RPC (`ScanLibrary`, over tonic/prost) that `beam-stream`
could call remotely. This meant two OS processes could hold independent database connections and
both write to the same tables — a two-writer situation with no clear ownership boundary, for a
feature (remote indexing) that nothing in the product actually requires at Beam's target deployment
scale (a single self-hosted instance on home-lab-class hardware). The gRPC layer added a protobuf
schema to keep in sync with the domain model, a second binary and Containerfile to build and
deploy, and a second process boundary to reason about for zero product benefit. `beam-auth`
similarly shipped as a standalone service/binary, embedded in-process into `beam-stream`'s router,
duplicating the same "why does this need to be its own process" question.

## Decision

We consolidated to a single deployable binary, `beam-server` (renamed from `beam-stream`, since it
now owns HTTP API + auth + indexing + streaming + enrichment, not just streaming). `beam-index` is
a library-only crate: no `main.rs`, no gRPC/tonic/prost layer, no Containerfile — `beam-server`
absorbs its scan/classify/enrich pipeline in-process via ordinary Rust function/trait calls.
`beam-auth` is likewise library-only. There is exactly one process, and exactly one writer to
Postgres. Internal crate boundaries (`beam-domain`, `beam-entity`, `beam-index`, `beam-auth` as
separate crates with trait-based interfaces) are explicitly preserved — this is a deployment/process
simplification, not a retreat from internal modularity.

## Consequences

**Positive:**
- Eliminates an entire class of dual-writer consistency bugs by construction (no more "which process
  wrote this row last" questions).
- Removes the protobuf/gRPC schema as a second contract to keep in sync with the domain model.
- Simpler operations: one container to build, deploy, monitor, and scale for the target deployment
  size.
- Faster indexing-to-serving path: no RPC round-trip between scan completion and catalog visibility.

**Negative / accepted cost:**
- `beam-server` becomes a larger binary with more total dependencies linked into one process; a bug
  in indexing can theoretically affect the availability of the HTTP API in ways it could not before
  (though standard in-process fault isolation via `Result`/panics-caught-at-task-boundary mitigates
  most of this).
- Scaling the indexing workload independently of the HTTP-serving workload (e.g. running enrichment
  on separate hardware) is no longer possible without re-introducing a process boundary later.
- This is a deliberate bet against the longer-term distributed/Kubernetes-native aspiration
  ([#76](https://github.com/justin13888/beam/issues/76)); revisiting that aspiration later means
  re-introducing some version of the boundary this ADR removes. Internal trait boundaries are kept
  specifically to make that future split easier if it ever becomes necessary, but it is not free —
  some wiring code will need to be rewritten as RPC/queue calls instead of function calls.
