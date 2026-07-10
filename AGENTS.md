# Beam

## Context & Persona
You are an expert software engineer working on `beam`, a media management server. The project is a multi-language monorepo.

Your primary goal is to write highly performant, robust code while strictly adhering to architecture patterns that allow for offline, dependency-free testing.

## API Implementation Patterns for High-Quality Testing
To ensure the system remains highly testable, you must apply the following patterns to all new code:

* **Trait-Based Abstraction:** All external boundaries—including database access (`beam-entity`), file system I/O, and external APIs—MUST be abstracted behind Rust traits. Never tightly couple business logic to concrete infrastructure implementations.
* **Dependency Injection:** Pass dependencies (via generic bounds or `Arc<dyn Trait>`) into services and handlers. 
* **Domain Isolation:** Isolate core media management and streaming logic from web framework types. Your service layer should not know about HTTP requests, responses, or extractors.
* **Fakes over Mocks:** Prefer building robust, stateful `InMemory*` structs (e.g., `InMemoryMediaRepository`) for data stores over pure mocking frameworks when simulating complex state changes. Use `mockall` only for simple, strict contract verifications.

## Unit Testing Requirements (Zero-Dependency)
Unit tests must verify essential services end-to-end without spinning up external dependencies (e.g., Postgres, Docker Compose). 

* **Zero Infrastructure:** All tests must pass immediately using `cargo test --workspace`. They must NEVER require the services in `compose.dependencies.yaml` to be running.
* **Subcutaneous E2E Testing:** Write tests that exercise complete vertical slices of the application. Instantiate the core application router/service with in-memory implementations and pass it programmatic requests (e.g., using Salvo's `salvo::test::TestClient`) to verify the response and state mutation.
* **Edge-Case Codification:** Any scenario that would normally require manual verification (e.g., corrupted media streams, missing file paths, database connection drops) MUST be codified as a unit test by configuring the injected traits to return the relevant `Result::Err`.
* **Test Data Builders:** Implement builder patterns for domain entities in your `#[cfg(test)]` modules to quickly scaffold consistent, valid state across different test suites.

## Rust Styling
- Prefer more verbose, explicit patterns if it avoids refactoring bugs (e.g., destructure if almost all struct fields are being used.)

## Workflow Rules
1. Before modifying database schema, check `beam-migration` and `beam-entity`.
2. Do not add new external service dependencies to `compose.dependencies.yaml` without explicitly providing an in-memory trait implementation for the test suite first.

## Where to look first

`docs/` is the canonical, ratified engineering documentation: `docs/requirements/` (product/FRs/
NFRs), `docs/architecture/` (overview, api, data model, streaming, security, components, ADRs),
`docs/testing.md` (strategy and coverage gates), `docs/operations/` (configuration, deployment).
Check the relevant doc there before making an architectural assumption.

## CI Commands to ensure pass before pushing completed work (e.g. before PR)

`mise.toml` is the single source of truth for every command CI and the git hooks run
([ADR-0009](docs/architecture/decisions/ADR-0009-release-engineering.md)). Do not add a check by
writing a command into a workflow or a hook -- add a mise task and call it from both.

```
mise run ci        # everything CI enforces, except coverage and image builds
mise tasks         # list every task
```

Individual tasks, if you need them: `rust:fmt`, `rust:clippy`, `rust:test`, `rust:deny`,
`rust:lockfile`, `rust:coverage`, `ts:check`, `ts:typecheck`, `ts:test`, `docs:build`,
`codegen:openapi`, `check:ffmpeg-version`. The `:fix` variants (`rust:fmt:fix`, `ts:check:fix`)
write their fixes.

Commits must be [Conventional Commits](https://www.conventionalcommits.org/); `convco` enforces this
in the `commit-msg` hook, and release-please derives the version and `CHANGELOG.md` from them.

The Rust tasks statically vendor an LGPL-only FFmpeg by default (via `ffmpeg-sys-next`'s `build`
feature), so they compile on hosts without system FFmpeg development libraries -- no `.pc` files for
`libavutil` etc., which is common outside CI/containers. This requires a `nasm` assembler on `PATH`.
See [ADR-0007](docs/architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md). CI and container
builds dynamically link a system FFmpeg instead, by setting `BEAM_CARGO_FEATURES=""`; do the same in
a gitignored `mise.local.toml` if your host has the development libraries.
