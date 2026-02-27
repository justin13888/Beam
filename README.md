# beam

> NOTE: Major refactoring is to be expected next year on-and-off given time. It will be alpha-ready for testing and feedback later.

Beam is a high-performance, scalable media server to stream video, audio, and other content with real-time transcoding capabilities. It is ideal for home labs, small business that need to stream content on a variety of devices.

## Features

- [x] HLS/DASH streaming
- [ ] Real-time remuxing/transcoding with hardware acceleration (NVENC, VAAPI)
- [ ] Fully-distributed and Kubernetes-native architecture

<!-- TODO: Finalize later -->

## Motivation

Beam originally started as a project to surpass the limitations of Jellyfin, a popular open-source media server. Jellyfin is a great project, but we need a more modern, straightforward solution that is as easy to use but more feature-rich and actively developed.

## Architecture

Beam consists of multiple backend services that work together to provide a seamless media streaming experience. The main components are:

- `beam-stream`: Media streaming service that handles live transcoding, caching, and streaming of media files (Rust/GraphQL/gRPC/ffmpeg).
<!-- - `beam-auth`: Authentication and user management service that handles user registration, login, and permissions (Rust/GraphQL). -->
<!-- - `beam-tasks`: GRPC microservice that manages background tasks such as transcoding, indexing, and metadata retrieval (Rust/Tonic). -->
<!-- - `beam-index`: Media indexing service that scans and indexes media files on disk (Rust). -->
<!-- - `beam-recommendation`: Recommendation service that provides personalized content recommendations based on user preferences and viewing history (Python/PyTorch). TODO -->

<!-- TODO: Add back these components ^^ -->

Currently, there is one client app to interact with Beam:

- `beam-web`: Web frontend that provides a user-friendly interface to browse and stream media content (TypeScript/React).

<!-- TODO: Add architecture diagram -->

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
   - GraphQL Playground: <http://localhost:8000/graphql>

### Production Deployment

For production deployments, we recommend reviewing all configurations in `.env` but at least:

1. **Security**:
   - Change `POSTGRES_PASSWORD` to a strong, unique password
   - Use HTTPS with a reverse proxy (nginx, Caddy, Traefik)
   - Set `SERVER_URL` and `C_STREAM_SERVER_URL` to your public domain

2. **Storage**:
   - Set `HOST_VIDEO_DIR` to your media library location
   - Ensure sufficient disk space for `HOST_CACHE_DIR`
   - Consider using external volumes for `HOST_POSTGRES_DATA`

3. **Performance**:
   - Enable hardware acceleration for transcoding (configure in beam-stream)
   - Set `ENABLE_METRICS=true` for monitoring
   - Adjust `RUST_LOG` to `info` or `warn` in production

## Development

0. Install toolchain: rustup, bun
1. Clone repository
2. `lefthook install`

### Start up

See individual README files for each component.

To spin up entire dependency stack with Docker Compose for testing, run:

```bash
podman compose -f compose.dependencies.yaml up
```

## License

This project is licensed under the AGPL License - see the [LICENSE](LICENSE) file for details.
