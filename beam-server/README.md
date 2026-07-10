# Beam Server

The main Beam server: HTTP API, auth, in-process media indexing, and streaming, built with Rust
and Salvo. See [`docs/architecture/components.md`](../docs/architecture/components.md) for
architecture and [`docs/operations/configuration.md`](../docs/operations/configuration.md) for
the full configuration reference.

## Development

- Install ffmpeg/libav 8+ libraries on your system, or use the vendored-FFmpeg build for hosts
  without system dev libraries: the `cargo build-local`/`clippy-local`/`t-local` aliases from
  `.cargo/config.toml`, per
  [ADR-0007](../docs/architecture/decisions/ADR-0007-vendored-ffmpeg-local-dev.md).
  - *Tip: Refer to [Containerfile](Containerfile) for ffmpeg build flags used in prod.*

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

- Migrations apply automatically at startup (`BEAM_AUTO_MIGRATE`, default true); the
  [`beam-migration`](../beam-migration/README.md) CLI remains for manual `up`/`down`/`status`

- Start development server:

    ```bash
    cargo watch -x run
    ```

### Build container image

```bash
# In root directory
podman build -f beam-server/Containerfile -t beam-server .
```

## API Documentation

See OpenAPI docs: `http://localhost:8000/openapi`
