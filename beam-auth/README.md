# Beam Auth

A library crate providing user, session, and authentication logic for `beam-server`. There is no
standalone `beam-auth` binary or container image -- `beam-server` links this crate and mounts its
routes in-process. See [`docs/components/server.md`](../docs/components/server.md) and
[`docs/architecture/security.md`](../docs/architecture/security.md) for the current architecture,
and run/test `beam-server` per its own README for local development.

> Note: the design described below (username/password, JWT access tokens, Redis-backed sessions,
> scoped stream tokens) is the pre-OIDC design and is being replaced per
> [ADR-0003](../docs/architecture/decisions/ADR-0003-oidc-bff-auth.md) and
> [ADR-0005](../docs/architecture/decisions/ADR-0005-sessions-in-postgres.md). This document will be
> rewritten once that work lands.

## API Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/v1/health` | Health check — returns 200 OK when the service is running. |
| `POST` | `/v1/auth/register` | Create a new account. Sets `session_id` cookie and returns a JWT. |
| `POST` | `/v1/auth/login` | Authenticate with username/email and password. Sets `session_id` cookie and returns a JWT. |
| `POST` | `/v1/auth/refresh` | Exchange a valid session cookie or `session_id` body field for a new JWT. |
| `POST` | `/v1/auth/logout` | Invalidate the current session. Clears the `session_id` cookie. |

## Architecture

`beam-auth` is a library crate with two feature flags:

- **`utils`** — Core domain types, trait abstractions, and concrete implementations for user repositories, session stores, and the auth service.
- **`server`** (implies `utils`) — Salvo HTTP handlers that wire the auth service into a router.

`beam-server` depends on `beam-auth` and mounts its routes directly into its own router; there is
no network call between them.

### Session & Token Strategy

- **Session**: A random 32-byte URL-safe Base64 ID stored in Redis/Valkey with a configurable TTL (default: 7 days).
- **Access token**: A short-lived JWT (15 minutes) signed with HMAC-SHA256 containing the user ID (`sub`) and session ID (`sid`).
- **Stream token**: A scoped JWT (6 hours) tied to a specific `stream_id`, used to authorize time-limited media access.
