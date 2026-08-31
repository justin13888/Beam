import createClient from "openapi-fetch";
import type { paths } from "@/api.gen";
import { env } from "@/env";

/**
 * Typed HTTP client generated from the OpenAPI specification.
 *
 * Regenerate types with: `bun run codegen:openapi:full`
 * (exports openapi.json from the backend, then generates src/api.gen.ts)
 *
 * TODO(#118-followup): `src/api.gen.ts` is STALE and does not describe the
 * server.
 *
 * The Kynos migration moved the contract to OpenAPI 3.2 and renamed every
 * generated identifier -- `beam_server.models.media.MediaMetadata` is now
 * `MediaMetadata`, and so on for ~55 schema keys across ~20 files here.
 * `api.gen.ts` cannot be regenerated yet: `openapi-typescript` 7.13.0 delegates
 * to @redocly/openapi-core 1.34.8, whose `detectSpec` throws
 * `Unsupported OpenAPI version: 3.2.0`. Redocly 2.x reads 3.2; openapi-typescript
 * has not bumped to it.
 *
 * beam-web is being rewritten shortly, so it is deliberately NOT being renamed
 * in the meantime. `ts:typecheck` and `ts:test` are switched off in CI until
 * then -- see the codegen:openapi task in mise.toml.
 */
export const apiClient = createClient<paths>({
	baseUrl: env.C_STREAM_SERVER_URL,
	// The session cookie is the only credential, and the API is a different
	// origin from the app in every supported deployment, so every request needs
	// this. Set once here rather than per call: a call site that forgets it
	// silently makes an unauthenticated request, which surfaces as a confusing
	// 401 rather than as a mistake.
	credentials: "include",
	// Resolved per call instead of captured at module load. `createClient`
	// otherwise snapshots `globalThis.fetch` the moment this module is
	// imported, which is before a test's request interceptor installs itself --
	// so MSW could never see a single request the app made, and the web suite
	// had to `vi.mock` this module away to test anything. Mocking the client
	// stops testing the wire; this indirection is what lets the real one be
	// driven by fakes at the network boundary instead.
	fetch: (request) => globalThis.fetch(request),
});
