import createClient from "openapi-fetch";
import type { paths } from "@/api.gen";
import { env } from "@/env";

/**
 * Typed HTTP client generated from the OpenAPI specification.
 *
 * Regenerate types with: `bun run codegen:openapi:full`
 * (exports openapi.json from the backend, then generates src/api.gen.ts)
 */
export const apiClient = createClient<paths>({
	baseUrl: env.C_STREAM_SERVER_URL,
});
