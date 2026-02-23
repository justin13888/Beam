# Beam Agent Configuration

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
* **Subcutaneous E2E Testing:** Write tests that exercise complete vertical slices of the application. Instantiate the core application router/service with in-memory implementations and pass it programmatic requests (e.g., using Axum's test helpers) to verify the response and state mutation.
* **Edge-Case Codification:** Any scenario that would normally require manual verification (e.g., corrupted media streams, missing file paths, database connection drops) MUST be codified as a unit test by configuring the injected traits to return the relevant `Result::Err`.
* **Test Data Builders:** Implement builder patterns for domain entities in your `#[cfg(test)]` modules to quickly scaffold consistent, valid state across different test suites.

## Workflow Rules
1. Before modifying database schema, check `beam-migration` and `beam-entity`.
2. Do not add new external service dependencies to `compose.dependencies.yaml` without explicitly providing an in-memory trait implementation for the test suite first.

## MANDATORY: Use td for Task Management
You must run td usage --new-session at conversation start (or after /clear) to see current work.
Use td usage -q for subsequent reads.
