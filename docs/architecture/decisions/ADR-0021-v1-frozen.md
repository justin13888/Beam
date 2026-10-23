# ADR-0021: The /v1 wire contract is frozen; a breaking change goes to /v2

## Status

Accepted. Closes the freeze [#190](https://github.com/justin13888/beam/issues/190) set out to make,
with one item still waiting on a spargen release ([#300](https://github.com/justin13888/beam/issues/300)).
Builds on [ADR-0010](ADR-0010-openapi-3-2-kynos.md) and [ADR-0012](ADR-0012-native-client-rust-core.md).

## Context

Every client Beam has -- `beam-web`, `beam-client-core` and the Android and Apple apps above it --
is generated from one OpenAPI document. Until #190 that document was consistent by habit: lists
were bare arrays in some places and connections in others, offsets in some and cursors in others,
`H264` beside `next_up`, epoch integers beside RFC 3339 strings, a `duration` in seconds beside a
`runtime` in minutes, and `info.version` a literal `1.0.0` that named no release. Each shape a
document gets wrong is a shape every generated client inherits, and a native app on a phone cannot
be upgraded in step with the server it talks to.

## Decision

**`/v1` is frozen.** A change that would break a client generated from the current document -- a
removed or renamed operation, field or enum value, a type narrowed, a status or problem type a
client relies on changed -- is made under `/v2`, never by editing `/v1` in place. Adding is not
breaking: a new operation, a new optional field, a new list's connection. A new problem type on an
existing status is additive on the wire but not to a spargen client, which decodes a closed set
([getkono/spargen#268](https://github.com/getkono/spargen/issues/268)); it is made knowing that.

**The conventions are executable.** `beam-server/src/routes/conventions_tests.rs` walks the exported
document and fails on any violation, with no exception list -- a schema that cannot comply is
changed, not waived: R1 (`snake_case` enum values and consts), R2 (no `Dto`/`Response` names), R2b
(no two enums with one value set), R3 (no bare-array response; every list is a `*Connection`), R4
(identifiers are UUIDs), R5 (`*_at` instants, `format: date` days, no epochs), R6 (measured numbers
name their unit), R8 (`info.version` is the release) and R10 (a `409` or `422` names its problem
type, Kynos's own body rejection aside). `docs/architecture/api.md` describes them; the test is the
contract. R9 (no single-key `oneOf` wrapper) waits on #300.

**`info.version` is the release, not the contract.** It is `CARGO_PKG_VERSION`, so the document a
release ships names that release, and the release workflow regenerates both committed copies on the
release pull request. The contract's version is the path prefix.

## Consequences

- A `/v1` client keeps working against every later server: the freeze is what makes an installed
  Android build safe to leave alone.
- A breaking need costs a `/v2` route (or tree), served beside `/v1` until no supported client
  calls the old one. That cost is the point; it is what keeps "small" breaks from accumulating.
- A new list starts life as a connection, and may start unpaged (`items` is everything,
  `page_info` one complete page) and page later without a break.
- The rules guard the document Beam emits, not Kynos or spargen: a gap in either is fixed upstream
  (CLAUDE.md rule 3), as the `MediaMetadata` tagging waits on spargen today.
