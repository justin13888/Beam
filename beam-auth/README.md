# Beam Auth

A standalone authentication service for the Beam platform, providing user registration, login, session management, and JWT issuance.

## Development

- Copy `.env.example` to `.env` and modify as needed:

    ```bash
    cp .env.example .env
    ```

- Install some dependencies:

    ```bash
    cargo install cargo-watch
    ```

- Start other dependencies:

    ```bash
    podman compose -f compose.dependencies.yaml up -d
    ```

- Make sure you applied [migrations](../beam-migration/README.md)

- Start development server:

    ```bash
    cargo watch -x run
    ```

### Build container image

```bash
# In root directory
podman build -f beam-auth/Containerfile -t beam-auth .
```

## API Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/v1/health` | Health check — returns 200 OK when the service is running. |
| `POST` | `/v1/auth/register` | Create a new account. Sets `session_id` cookie and returns a JWT. |
| `POST` | `/v1/auth/login` | Authenticate with username/email and password. Sets `session_id` cookie and returns a JWT. |
| `POST` | `/v1/auth/refresh` | Exchange a valid session cookie or `session_id` body field for a new JWT. |
| `POST` | `/v1/auth/logout` | Invalidate the current session. Clears the `session_id` cookie. |

## Architecture

`beam-auth` is both a standalone binary service and a library crate with two feature flags:

- **`utils`** — Core domain types, trait abstractions, and concrete implementations for user repositories, session stores, and the auth service.
- **`server`** (implies `utils`) — Salvo HTTP handlers that wire the auth service into a router.

Other services (e.g., `beam-stream`) depend on `beam-auth` as a library (with the `utils` feature) to perform local JWT verification without making network calls.

### Session & Token Strategy

- **Session**: A random 32-byte URL-safe Base64 ID stored in Redis/Valkey with a configurable TTL (default: 7 days).
- **Access token**: A short-lived JWT (15 minutes) signed with HMAC-SHA256 containing the user ID (`sub`) and session ID (`sid`).
- **Stream token**: A scoped JWT (6 hours) tied to a specific `stream_id`, used to authorize time-limited media access.
