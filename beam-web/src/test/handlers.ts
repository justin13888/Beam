import { HttpResponse, http } from "msw";
import type { components } from "@/api.gen";
import { problem } from "@/test/problem";
import * as factory from "./factories";

/**
 * Default MSW handlers covering the whole API surface.
 *
 * Every response body is typed as the schema the server publishes, so a double
 * cannot drift from the contract without failing `tsc`. Statuses are the ones
 * the document declares for the operation: `tsc` cannot check those, so they
 * are read off `openapi.json` rather than guessed (a `PATCH` of a user and a
 * media refresh answer 204, creating a library 200, queueing a scan 202). Individual tests
 * override what they care about with `server.use(...)`; the defaults exist so
 * that a test exercising one endpoint is not forced to stub the six unrelated
 * ones the page happens to call.
 */
type Schemas = components["schemas"];

export const BASE_URL = "http://localhost:8000";

export const mockUser = factory.user();

export const handlers = [
	// ── session ──────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/me`, () =>
		HttpResponse.json<Schemas["CurrentUser"]>(mockUser),
	),
	http.post(
		`${BASE_URL}/v1/logout`,
		() => new HttpResponse(null, { status: 204 }),
	),
	http.post(
		`${BASE_URL}/v1/logout-all`,
		() => new HttpResponse(null, { status: 204 }),
	),
	http.get(`${BASE_URL}/v1/sessions`, () =>
		HttpResponse.json<Schemas["SessionConnection"]>(factory.connection([])),
	),
	http.delete(
		`${BASE_URL}/v1/sessions/:id`,
		() => new HttpResponse(null, { status: 204 }),
	),

	// ── browse ───────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/media`, () =>
		HttpResponse.json<Schemas["MediaConnection"]>(factory.connection([])),
	),
	http.get(`${BASE_URL}/v1/media/:id`, () =>
		problem(404, "Not found", "#media-not-found"),
	),
	http.get(`${BASE_URL}/v1/media/:id/sources`, () =>
		HttpResponse.json<Schemas["MediaSourceConnection"]>(factory.connection([])),
	),
	http.get(`${BASE_URL}/v1/genres`, () =>
		HttpResponse.json<Schemas["GenreConnection"]>(factory.connection([])),
	),

	// ── libraries ────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/libraries`, () =>
		HttpResponse.json<Schemas["LibraryConnection"]>(factory.connection([])),
	),
	http.get(`${BASE_URL}/v1/libraries/:id`, () =>
		problem(404, "No library has that id", "#library-not-found"),
	),
	http.get(`${BASE_URL}/v1/libraries/:id/files`, () =>
		HttpResponse.json<Schemas["LibraryFileConnection"]>(factory.connection([])),
	),

	// ── playback ─────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/continue-watching`, () =>
		HttpResponse.json<Schemas["ContinueWatchingConnection"]>(
			factory.connection([]),
		),
	),
	http.get(`${BASE_URL}/v1/history`, () =>
		HttpResponse.json<Schemas["HistoryConnection"]>(
			factory.countedConnection([]),
		),
	),
	http.put(`${BASE_URL}/v1/files/:file_id/progress`, () =>
		HttpResponse.json<Schemas["UserTitleState"]>(factory.userTitleState()),
	),

	// ── admin ────────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/admin/users`, () =>
		HttpResponse.json<Schemas["AdminUserConnection"]>(
			factory.countedConnection([]),
		),
	),
	http.patch(
		`${BASE_URL}/v1/admin/users/:id`,
		() => new HttpResponse(null, { status: 204 }),
	),
	http.get(`${BASE_URL}/v1/admin/logs`, () =>
		HttpResponse.json<Schemas["AdminLogConnection"]>(
			factory.countedConnection([]),
		),
	),
	http.get(`${BASE_URL}/v1/admin/status`, () =>
		HttpResponse.json<Schemas["AdminStatus"]>(factory.adminStatus()),
	),
	http.get(`${BASE_URL}/v1/admin/events`, () =>
		HttpResponse.json<Schemas["AdminEventConnection"]>(factory.connection([])),
	),
	http.post(`${BASE_URL}/v1/admin/libraries`, () =>
		HttpResponse.json<Schemas["Library"]>(factory.library()),
	),
	http.post(`${BASE_URL}/v1/admin/libraries/:id/scan`, ({ params }) =>
		HttpResponse.json<Schemas["ScanJob"]>(
			factory.scanJob({ library_id: String(params.id) }),
			{ status: 202 },
		),
	),
	http.delete(
		`${BASE_URL}/v1/admin/libraries/:id`,
		() => new HttpResponse(null, { status: 204 }),
	),
	http.post(
		`${BASE_URL}/v1/admin/media/:id/refresh`,
		() => new HttpResponse(null, { status: 204 }),
	),
];

/** Reusable override for an unauthenticated / expired session. */
export const meUnauthenticatedHandler = http.get(`${BASE_URL}/v1/me`, () =>
	problem(401, "Missing session cookie"),
);

/** Reusable override for an authenticated admin. */
export const meAdminHandler = http.get(`${BASE_URL}/v1/me`, () =>
	HttpResponse.json(factory.user({ is_admin: true, display_name: "Ada" })),
);
