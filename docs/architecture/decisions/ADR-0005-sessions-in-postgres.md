# ADR-0005: Sessions in Postgres, not Redis/Valkey

## Status

Accepted.

## Context

Sessions were previously backed by Redis/Valkey, adding a second stateful datastore to the
deployment (alongside Postgres) purely to hold session records — a small, low-throughput,
relational-shaped piece of state (a row per session, looked up by key, with an expiry). Running a
second datastore means a second thing to provision, back up, monitor, and reason about for failure
modes, for a workload that doesn't need Redis's specific strengths (sub-millisecond in-memory access at very high
throughput, pub/sub, specialized data structures). At Beam's target deployment scale — a single
self-hosted instance serving a household or small organization — session lookup volume is nowhere
near the point where Postgres's latency profile would matter.

## Decision

We moved sessions into Postgres, in a `sessions` table (see `data-model.md`), accessed through a
`SessionStore` trait with a Postgres-backed production implementation and an in-memory fake for
tests. Redis/Valkey was dropped from the stack entirely — it was not used for anything else, so
removing sessions from it removed the only reason it was present.

## Consequences

**Positive:**
- One fewer stateful service to deploy, back up, and operate; `compose.dependencies.yaml` shrank by
  one entry.
- Sessions get transactional consistency with the rest of the application's state for free (e.g. a
  user deletion cascading to their sessions is a normal FK cascade, not a manual two-store
  synchronization step).
- One fewer external-boundary trait to fake for tests, since the `SessionStore` in-memory fake
  replaces what would otherwise be a Redis-client mock.

**Negative / accepted cost:**
- Every authenticated request now costs a Postgres round-trip (or a cache hit in front of it) for
  session lookup, instead of an in-memory-speed Redis lookup. At Beam's target scale this is not
  expected to be a measurable bottleneck, but it is a real trade against a design that could handle
  much higher request throughput.
- Loses Redis's native key expiry (`TTL`) as a mechanism; expiry is instead enforced by an
  `expires_at` column check on lookup plus a periodic sweep of expired rows — slightly more
  application-level bookkeeping than "the store expires it for you."
- If Beam later needs a fast shared cache for an unrelated purpose (e.g. a hot read-through cache
  in front of catalog queries), that would be a new decision to reintroduce a cache layer — this ADR
  does not preclude that, but it does mean Redis isn't already sitting there half-used for it.
