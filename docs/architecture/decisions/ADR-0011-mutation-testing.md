# ADR-0011: Mutation testing as an advisory hardening loop, and an opt-in Postgres tier

## Status

Accepted.

## Context

Beam's testing strategy ([`docs/testing.md`](../../testing.md), NFR-201..207) is built around one
invariant: `cargo test --workspace` passes with zero infrastructure. That invariant is sound, and it
is what keeps the suite fast enough to run on every push. But the metric guarding it -- a 65% line
coverage floor -- measures the wrong thing, and `docs/testing.md` said so itself: *"a line-coverage
percentage says a line executed; it says nothing about assertion quality."*

An audit confirmed the gap was not hypothetical. With 368 tests and coverage comfortably above the
gate, the suite contained several classes of test that cannot fail for a reason that matters:

- **Tests whose subject was the test double.** Four tests in `beam-auth`'s user repository exercised
  `InMemoryUserRepository`'s `HashMap`; `SqlUserRepository` was never touched. Eight in `beam-domain`
  drove `InMemoryPlaybackProgressRepository`, one of them reaching into its private `rows` field.
  Others asserted that `TestClock` -- a test-only type -- keeps time, and that a method whose body is
  literally `Ok(())` returns `Ok(())`.
- **A security control tested only through its fake.** Four tests named for directory-escape
  containment each configured `InMemoryPathValidator` to return "path escapes root" and asserted that
  error came back. `OsPathValidator::validate_library_path`, which performs the real `canonicalize()`
  and `starts_with()` check, had no tests at all. The suite claimed a containment guarantee it did
  not have.
- **~1,900 lines of persistence that could not be tested.** Nine SeaORM repositories, `PgSessionStore`
  and `SqlPendingAuthStore` each hold a concrete `DatabaseConnection`, with no seam beneath the trait.
  Nothing about the SQL they generate was assertable, and the `InMemory*` fakes were free to diverge
  from Postgres silently.
- **A classifier tested against a copy of itself.** The metrics route classifier is a hand-maintained
  mirror of the route table; its test was a second hand-maintained copy of the same table.
- **Hermeticity leaking.** Six tests used real `tokio::time::sleep` to force two `Utc::now()` calls
  apart, in a workspace that already had a `Clock` seam.

Raising the coverage threshold would not have found any of these. Every one of them executes the
lines it fails to check.

## Decision

**Adopt `cargo-mutants` as an advisory hardening loop, not as a gate.**

Mutation testing changes the program in small plausible ways and reports which changes the suite
failed to notice. That is a direct measure of assertion quality, and it is the missing feedback
signal. It is deliberately *not* a merge gate: `.github/workflows/mutants.yml` runs `--in-diff` on
pull requests and a sharded full run on a schedule, both `continue-on-error`, and `ci-ok` does not
depend on either. Mutation runs cost hours and produce a tail of equivalent mutants; making them
blocking would trade a large amount of contributor friction for a signal that is most valuable when
acted on deliberately.

**When a mutant survives, harden the code before adding a test.** The resolution order is:

1. Delete the branch if it is unreachable or meaningless.
2. Make the illegal state unrepresentable with a type, if a type can carry the invariant.
3. Write the test, if the behaviour is real and load-bearing.
4. Mark `#[mutants::skip]` with a justification, if the mutant is genuinely equivalent.

This order is the point of the decision. A surviving mutant is evidence of mutable surface that
nothing depends on; the best outcome is usually less code, not more tests. Only step 4 is recorded
here, so exemptions stay countable.

**Every test-utils-gated double carries `#[mutants::skip]`, enforced by a check.** cargo-mutants
recognises only the literal `#[cfg(test)]`; it parses `#[cfg(any(test, feature = "test-utils"))]` as
an unrecognised form and mutates the contents. Left alone, this workspace's 35 gated sites would have
contributed roughly 15% of all mutants, all of them describing scaffolding, and the ones that were
killed would have inflated the caught rate with kills that prove nothing. A name-based `exclude_re`
was rejected: the doubles are interleaved with real logic in the same files, and this workspace had
un-gated production types whose names begin with `InMemory` -- silently exempting real code is the
one failure mode a mutation setup must not have. `mise run check:mutants-skip-fakes` enforces the
explicit annotation, in `mise run ci` and in `pre-push`.

**Gate regions and functions alongside lines.** `cargo-llvm-cov`'s `--branch` needs a nightly
toolchain and `rust-toolchain.toml` pins stable, so `--fail-under-regions` is the branch gate: LLVM
regions are a superset of branches, and region coverage is what correlates with mutation score.
Thresholds are calibrated a few points under measured and ratcheted, never relaxed.

**Add an opt-in `pg-integration` tier, and keep NFR-201 unconditional.** Repository behaviour is
covered by one shared contract test per trait, instantiated three times: against the `InMemory*` fake,
against the SeaORM implementation with `sea_orm::MockDatabase` (asserting the generated SQL, still
hermetic), and against a real Postgres behind the non-default `pg-integration` feature. This converts
the fake-only tests from circular to load-bearing -- the same assertions now constrain the real
implementation -- and it is the only way to cover `pg_trgm` ranking, index usage, and migration
up/down behaviour. The feature is never enabled by `rust:test`, `rust:coverage`, `mise run ci`, or
the `pre-push` hook; it runs in its own CI job with a service container.

## Consequences

**Good.** Assertion quality becomes measurable rather than assumed. The default resolution shrinks
the codebase instead of growing the test suite. `sea_orm::MockDatabase` brings ~1,900 previously
untestable lines into the hermetic suite, and the contract tests make fake-versus-Postgres divergence
a test failure rather than silent drift. The `#[mutants::skip]` convention has the side benefit of
keeping test scaffolding out of release builds -- applying it surfaced two public types
(`InMemoryNotificationService`, `NoOpAdminLogService`) that were shipping despite being used only by
tests.

**Costs.** Two more pinned tools (`cargo-mutants`, `cargo-nextest`) and a `mutants` crate dependency,
which is MIT with no dependencies. A scheduled workflow that consumes real runner time; the documented
downgrade is to move the full matrix to weekly and let the per-pull-request `--in-diff` job carry the
day-to-day signal. Contributors will see advisory failures they are not required to act on, which is
a deliberate trade against the friction of a blocking gate. `pg-integration` means one CI job that
does need infrastructure -- acceptable precisely because it is opt-in and separate, which is what
keeps NFR-201 true rather than aspirational.

**Rejected alternatives.** *Making mutation testing a blocking gate on changed code* -- rejected
because equivalent mutants are common enough that it would block pull requests on judgement calls,
and because it would incentivise `#[mutants::skip]` as an escape hatch rather than as a documented
exception. *`MockDatabase` alone, with no real-Postgres tier* -- rejected because it cannot answer
whether `pg_trgm` ranking or an index actually behaves as assumed. *A real-Postgres tier alone* --
rejected because those tests do not run in the default loop, so repository regressions would not be
caught on the developer's machine.

## What the loop found

Recorded because it is the evidence for this decision, not decoration. The first pass over the two
crates at the root of the dependency graph took **beam-domain from 21 survivors to 0** and
**beam-auth from 52 to 0**, and the resolutions split the way the mandated order predicts:

*Deleted, not tested.* `FileStatus`'s `Display`/`FromStr` became dead the moment the enum conversion
replaced them, and `parse_byte_range`'s `start >= file_size` clause is unreachable because every
branch above it already clamps `end` to `file_size - 1`. Four mutants and two branches gone, no
tests added.

*A real bug the survivor pointed at.* `oidc_delete_session` guarded "did the caller revoke the
session they are using?" with `current.value() == current_token` -- comparing the request cookie to a
token *derived from that same cookie*, so it was always true. Revoking any other device signed the
caller out of the one in their hand. The `== `→` !=` mutant surviving is what surfaced it; it now
re-reads the caller's own token after the delete.

*Structure, not assertions.* Six survivors were `session_idle_days * 24 * 60 * 60` repeated at three
call sites; naming it once (`idle_ttl_secs()`) made a single test cover all of them. A survivor on
`decode_verified_claims` was unreachable because its argument can only be built by signing and
verifying a real JWT -- splitting the string half out made it ordinary.

*A fixture that made a mutant equivalent.* `get_and_touch` computes `now - last_active`, and the
in-memory contract fixture started its `TestClock` at the Unix epoch -- where `now - 0` and
`now + 0` are the same number. The fixture now starts at a non-epoch instant.

## Decision log: accepted `#[mutants::skip]` exemptions

Two admissible reasons, and no others. Either the mutant is **equivalent** -- indistinguishable in
observable behaviour -- or the code is **unreachable from any hermetic test** *and* every part of it
that could be decided by a test has already been extracted somewhere that is tested. "Hard to test"
on its own is not a justification; it is a reason to apply step 1 or 2 above.

| Location | Mutant | Reason |
|---|---|---|
| `beam-index/src/probe/mod.rs` `init` | `-> Ok(())` | Equivalent. On FFmpeg >= 5 `av_register_all` is gone and `ffmpeg_next::init` has no effect this workspace can observe: stubbing the body out leaves the entire probe suite green, including the tests that demux real containers. The call is retained because the bindings document it as required. |
| `beam-server/src/config.rs` `ServerConfig::load_and_validate` | `-> Ok(Default::default())` | Unreachable. The body is `Self::builder().env().load()?.normalize_and_validate()`; the only uncovered half reads the *process environment*, and mutating that is `unsafe` in Rust 2024 precisely because the suite runs in parallel. The decisions it composes were extracted into `normalize_and_validate` (order of normalisation, scalar validation and path creation) and are tested directly. |

Test-double modules carrying a blanket `#[mutants::skip]` are not listed here; they are scaffolding
rather than exemptions, and `mise run check:mutants-skip-fakes` keeps them exhaustive.
