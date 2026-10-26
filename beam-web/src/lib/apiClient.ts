import createClient from "openapi-fetch";
import type { components, paths } from "@/api.gen";
import { env } from "@/env";

/**
 * Typed HTTP client generated from the OpenAPI specification.
 *
 * Regenerate types with: `mise run codegen:openapi` (exports openapi.json from
 * the backend, then generates src/api.gen.ts), or `mise run
 * codegen:openapi:web` to regenerate from the committed document alone.
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

/** A `/v1` connection's paging state (`PageInfo` in the OpenAPI document). */
export type PageInfo = components["schemas"]["PageInfo"];

/**
 * The one `/v1` list envelope (ADR-0021): `{ items, page_info }`. Every
 * `*Connection` schema the document declares is this shape over its own item.
 */
export interface Connection<T> {
	items: T[];
	page_info: PageInfo;
}
