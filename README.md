# beam

> NOTE: Beam is pre-alpha. Expect breaking changes (including destructive database migrations)
> on-and-off given time, ahead of a first alpha release for testing and feedback.

Beam is a media server for streaming your own video library to a variety of devices -- built for
home labs and small setups that want something modern, straightforward, and actively developed.

Beam deliberately **never transcodes on the fly**. Instead of live remuxing/transcoding, it
serves pre-existing files directly (byte-range HTTP streaming) and, where a title has multiple
encoded versions, lets the client pick among them for constrained-bandwidth playback. See
[`docs/architecture/streaming.md`](docs/architecture/streaming.md) and
[ADR-0004](docs/architecture/decisions/ADR-0004-never-transcode.md) for the rationale.

## Motivation

Beam originally started as a project to surpass the limitations of Jellyfin, a popular open-source media server. Jellyfin is a great project, but we need a more modern, straightforward solution that is as easy to use but more feature-rich and actively developed.

## Clients

*Every client is native and uses the public API contracts as source-of-truth.*

- [ ] Web
- [ ] Swift (iOS/macOS)
- [ ] Kotlin (Android)
- [ ] GTK (Linux)

Not in scope currently: Native Windows client (see [#64](https://github.com/justin13888/Beam/issues/64))

## Architecture

Beam is a modular monolith: one Rust binary handles the HTTP API, OIDC auth, in-process media
indexing (scanning, filesystem watching, metadata enrichment), and direct-play streaming, backed
by Postgres. See [`docs/`](docs/) for the full ratified architecture -- start with
[`docs/architecture/overview.md`](docs/architecture/overview.md) and
[`docs/requirements/product.md`](docs/requirements/product.md) -- and each crate's own README for
its specific responsibilities:

- [`beam-server`](beam-server/README.md): HTTP API, OIDC auth/sessions, admin API, direct-play
  streaming, and process wiring (main binary).
- [`beam-index`](beam-index/README.md): media library scanning, filesystem watching, FFmpeg
  probing, and metadata enrichment (via [`cameo`](https://crates.io/crates/cameo) against
  TMDB/AniList).
- [`beam-auth`](beam-auth/README.md): OIDC Authorization Code + PKCE flow, session management
  (library crate, no binary).
- [`beam-domain`](beam-domain/README.md): core domain models and repository/provider trait
  definitions, framework- and infrastructure-agnostic.
- [`beam-entity`](beam-entity/README.md): sea-orm entity definitions for the Postgres schema.
- [`beam-migration`](beam-migration/README.md): sea-orm-migration schema migrations.

Currently, there is one client app to interact with Beam:

- [`beam-web`](beam-web/README.md): web frontend (TypeScript/React/TanStack Router) to browse,
  search, and play media, plus an admin area.

## Installation & Deployment

### Quick Start with Docker/Podman Compose

1. **Clone the repository**:

   ```bash
   git clone https://github.com/justin13888/beam.git
   cd beam
   ```

2. **Configure environment variables**:

   ```bash
   cp .env.example .env
   ```

3. **Start the services**:

   ```bash
   # Using Podman
   podman compose up -d
   
   # Or using Docker
   docker compose up -d
   ```

4. **Access the application**:

   - Frontend: <http://localhost:8080>
   - Backend API: <http://localhost:8000>
   - API docs (Scalar): <http://localhost:8000/openapi>

### Production Deployment

For production deployments, we recommend reviewing all configurations in `.env` but at least:

1. **Security**:
   - Change `POSTGRES_PASSWORD` to a strong, unique password
   - Use HTTPS with a reverse proxy (nginx, Caddy, Traefik)
   - Set `BEAM_SERVER_URL` and `C_STREAM_SERVER_URL` to your public domain

2. **Storage**:
   - Set `HOST_VIDEO_DIR` to your media library location
   - Ensure sufficient disk space for `HOST_DATA_DIR`
   - Postgres data lives in the `postgres` named volume declared in
     `compose.dependencies.yaml`; point it at dependable storage (edit that
     file to bind-mount a host path if preferred) and back it up

3. **Performance**:
   - Set `BEAM_ENABLE_METRICS=true` for monitoring
   - Adjust `RUST_LOG` to `info` or `warn` in production

Run [`verify-config.sh`](verify-config.sh) after editing `.env` to sanity-check the required
variables are set (see [`docs/operations/configuration.md`](docs/operations/configuration.md)
for the full reference).

## Development

0. Install [`rustup`](https://rustup.rs/) and [`mise`](https://mise.jdx.dev/) (mise supplies Bun and
   the rest of the tooling; rustup reads the pin in `rust-toolchain.toml`)
1. Clone repository
2. `mise install && mise run setup` -- installs tools, JS dependencies, and the git hooks

`mise.toml` is the single source of truth for every command CI and the git hooks run. `mise tasks`
lists them; `mise run ci` runs everything CI does except coverage and image builds. See
[ADR-0009](docs/architecture/decisions/ADR-0009-release-engineering.md).

On hosts without system FFmpeg development libraries, the Rust tasks statically vendor an LGPL-only
FFmpeg (requires a `nasm` assembler on `PATH`) -- see
[ADR-0007](docs/architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md). If you do have the
system libraries, set `BEAM_CARGO_FEATURES = ""` in a `mise.local.toml` to link against them
instead.

### Start up

See individual README files for each component.

To spin up the dependency stack (Postgres + the bundled [Dex](https://dexidp.io/) dev OIDC
provider, fronted by Traefik) with Docker/Podman Compose for local development or testing, run:

```bash
podman compose -f compose.dependencies.yaml up
```

The bundled Dex ships static test users for local development (see `dex/config.yaml`); run
`beam-server` on the host to exercise the OIDC login flow against it (the fully containerized
Dex topology is tracked in [#73](https://github.com/justin13888/beam/issues/73)).

## License

This project is licensed under the AGPL License - see the [LICENSE](LICENSE) file for details.
