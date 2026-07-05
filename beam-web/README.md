# Beam Web

This is the web client for Beam. It is built with React, Vite, and Bun.

## Development

### Start development server

Prerequisite: It is assumed [beam-server](../beam-server/README.md) is running and accessible.

Copy `.env.example` to `.env` and modify any necessary environment variables (necessary for codegen in next step).

```bash
bun install
bun run codegen:full # Builds server schemas and generates client code
bun dev
```

App will be available at `http://localhost:5173`.

### Build for production

To build this application for production:

```bash
bun run build
```

### Build container image

```bash
# In `beam-web` directory
bun run codegen:full
# In root directory
podman build -f beam-web/Containerfile -t beam-web .
```

## Testing

This project uses [Vitest](https://vitest.dev/) for testing. You can run the tests with:

```bash
bun run test
```

## Linting & Formatting

This project uses [Biome](https://biomejs.dev/) for linting and formatting. The following scripts are available:

```bash
bun run lint
bun run format
bun run check
```
