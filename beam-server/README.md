# Beam Server

The main Beam server: HTTP API, auth, in-process media indexing, and streaming, built with Rust
and Salvo. See [`docs/components/server.md`](../docs/components/server.md) for architecture and
[`docs/operations/dev-setup.md`](../docs/operations/dev-setup.md) for the full local dev setup
(including the `vendored-ffmpeg` path for hosts without system FFmpeg development libraries).

## Development

- Install ffmpeg/libav 8+ libraries on your system, or see
  [`docs/operations/dev-setup.md`](../docs/operations/dev-setup.md) for a vendored-FFmpeg
  alternative that doesn't require system dev libraries.
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

- Make sure you applied [migrations](../beam-migration/README.md)

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
