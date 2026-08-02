# Beam Engineering Docs

This directory is the ratified, present-tense engineering documentation for Beam: requirements,
architecture, testing strategy, and operations. It describes the system as it exists, is written
for contributors and agents working in this repository, and is reviewed in pull requests like
code. Decision rationale and history live in the [ADRs](architecture/decisions/) — the other docs
state current truth and cite ADRs by number rather than restating them. Deferred and future work
is tracked in GitHub issues, not in these docs. A ratified migration may have a readiness contract
when work in another repository must satisfy explicit gates before Beam can adopt it.

It is distinct from `beam-docs/`, the Astro/Starlight site hosting public user/operator-facing
documentation — see [architecture/components.md](architecture/components.md) for the division of
responsibility.

## Reading order

1. **[`requirements/`](requirements/)** — what Beam is and must do.
   - [`product.md`](requirements/product.md) — vision, personas, delivery scenarios, scope.
   - [`functional.md`](requirements/functional.md) — numbered FRs.
   - [`non-functional.md`](requirements/non-functional.md) — numbered NFRs.
2. **[`architecture/`](architecture/)** — how the system is built.
   - [`overview.md`](architecture/overview.md) — context/container view, component list.
   - [`api.md`](architecture/api.md) — REST/OpenAPI/SSE conventions and codegen.
   - [`data-model.md`](architecture/data-model.md) — full schema and invariants.
   - [`security.md`](architecture/security.md) — OIDC/BFF auth, sessions, CSRF, threat notes.
   - [`streaming.md`](architecture/streaming.md) — direct-play delivery model (never transcode).
   - [`components.md`](architecture/components.md) — per-crate/app ownership, module layout,
     boundaries, and testing approach.
   - [`kynos-migration-readiness.md`](architecture/kynos-migration-readiness.md) — the blocking
     framework and client-tool contract for the ratified post-Salvo migration; not current state.
   - [`decisions/`](architecture/decisions/) — ADRs recording the settled, non-obvious calls
     (see its [README](architecture/decisions/README.md); ADR-0001 through ADR-0010).
3. **[`testing.md`](testing.md)** — zero-dependency unit testing, fakes over mocks, subcutaneous
   e2e, coverage tooling.
4. **[`operations/`](operations/)** — how to run the system.
   - [`configuration.md`](operations/configuration.md) — full environment variable reference.
   - [`deployment.md`](operations/deployment.md) — compose topology and production guidance.

## Conventions

- Numbered requirements (FR-*, NFR-*) and ADRs are cited by number from other docs rather than
  restated — follow the links.
- `components.md` describes ownership and module boundaries; it defers schema detail to
  `architecture/data-model.md` and decision rationale to `architecture/decisions/`.
