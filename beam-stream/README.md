# Beam Stream

A high-performance streaming service built with Rust and Axum.

## Development

- Install ffmpeg/libav 8+ libraries on your system.
  - *Tip: Refer to [Containerfile](Containerfile) for ffmpeg build flags used in prod.>*

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

- Make sure you applied [migrations](../migrations/README.md)

- Start development server:

    ```bash
    cargo watch -x run
    ```

### Generate GraphQL Schema

```bash
# With dependencies like database started up...
cargo run --bin export_schema
```

### Build container image

```bash
# In root directory
podman build -f beam-stream/Containerfile -t beam-stream .
```

## API Documentation

See OpenAPI docs: `http://localhost:8000/openapi`
