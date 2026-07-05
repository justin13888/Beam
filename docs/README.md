# Beam Engineering Docs

This directory is the canonical, top-down engineering documentation for Beam: requirements,
architecture, component ownership, testing strategy, and operations. It is written for
contributors and agents working in this repository and is meant to be reviewed in pull requests
like code. It is distinct from `beam-docs/`, the Astro/Starlight site that will host
user/operator-facing documentation (install guides, feature tours) — see
[`components/docs-site.md`](components/docs-site.md) for the division of responsibility.

Status: these docs describe the **target state** of the current repository-wide push (see the
project's git history around this commit), not just a snapshot of what existed before it. Gaps
found during the review that produced these docs are resolved here as requirements and decisions,
not left as open questions — the accompanying code changes bring the repository into alignment
with what's written here.

## Reading order

1. **[`requirements/`](requirements/)** — what Beam is and must do.
   - [`product.md`](requirements/product.md) — vision, personas, delivery scenarios, scope.
   - [`functional.md`](requirements/functional.md) — numbered FRs.
   - [`non-functional.md`](requirements/non-functional.md) — numbered NFRs.
2. **[`architecture/`](architecture/)** — how the system is built.
   - [`overview.md`](architecture/overview.md) — context/container view, component list.
   - [`data-model.md`](architecture/data-model.md) — full schema and invariants.
   - [`streaming.md`](architecture/streaming.md) — delivery model (never transcode).
   - [`security.md`](architecture/security.md) — OIDC/BFF auth, sessions, CSRF, threat notes.
   - [`api.md`](architecture/api.md) — REST/OpenAPI/SSE conventions and codegen.
   - [`decisions/`](architecture/decisions/) — ADRs recording the settled, non-obvious calls.
3. **[`components/`](components/)** — per-crate/app ownership and module boundaries:
   [`server.md`](components/server.md), [`indexer.md`](components/indexer.md),
   [`domain.md`](components/domain.md), [`persistence.md`](components/persistence.md),
   [`web.md`](components/web.md), [`docs-site.md`](components/docs-site.md).
4. **[`testing/`](testing/)** — how correctness is verified.
   - [`strategy.md`](testing/strategy.md) — zero-dependency unit testing, fakes over mocks,
     subcutaneous e2e, what's deliberately left to manual validation.
   - [`coverage.md`](testing/coverage.md) — tooling, thresholds, how to run locally.
5. **[`operations/`](operations/)** — how to run and validate the system.
   - [`dev-setup.md`](operations/dev-setup.md) — local toolchain and first run.
   - [`e2e-validation.md`](operations/e2e-validation.md) — the manual end-to-end runbook.
   - [`configuration.md`](operations/configuration.md) — full environment variable reference.
   - [`ci.md`](operations/ci.md) — CI workflows and git hooks.
   - [`deployment.md`](operations/deployment.md) — compose topology and production guidance.

## Conventions

- Numbered requirements (FR-*, NFR-*) and ADRs are cited by number from other docs rather than
  restated — follow the links.
- Component docs describe ownership and module boundaries; they defer schema detail to
  `architecture/data-model.md` and decision rationale to `architecture/decisions/`.
- When a document describes something that differs from the code as it exists at any given
  commit mid-push, it says so explicitly (e.g. "changed from today", "(new)", "(removed)").
