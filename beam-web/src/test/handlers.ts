import { HttpResponse, http } from "msw";
import type { components } from "@/api.gen";
import { problem } from "@/test/problem";
import * as factory from "./factories";

/**
 * Default MSW handlers covering the whole API surface.
 *
 * Every response body is typed as the schema the server publishes, so a double
 * cannot drift from the contract without failing `tsc`. Individual tests
 * override what they care about with `server.use(...)`; the defaults exist so
 * that a test exercising one endpoint is not forced to stub the six unrelated
 * ones the page happens to call.
 */
type Schemas = components["schemas"];

export const BASE_URL = "http://localhost:8000";

export const mockUser = factory.user();

/** An empty, well-formed page of media. */
const emptyMediaConnection: Schemas["beam_server.services.metadata.MediaConnection"] =
	{
		edges: [],
		page_info: {
			has_next_page: false,
			has_previous_page: false,
			start_cursor: null,
			end_cursor: null,
		},
	};

const emptyHistory: Schemas["beam_server.routes.playback.HistoryResponse"] = {
	items: [],
	total: 0,
};

const emptyAdminUsers: Schemas["beam_server.models.admin.AdminUserListResponse"] =
	{
		items: [],
		total: 0,
	};

const adminStatus: Schemas["beam_server.models.admin.AdminStatusResponse"] = {
	uptime_secs: 3600,
	version: "0.1.0",
	counts: { users: 1, libraries: 1, files: 0 },
	enrichment: { pending: 0, enriched: 0, unmatched: 0, failed: 0 },
	recent_scans: [],
};

export const handlers = [
	// ── session ──────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/me`, () => HttpResponse.json(mockUser)),
	http.post(
		`${BASE_URL}/v1/logout`,
		() => new HttpResponse(null, { status: 200 }),
	),
	http.post(
		`${BASE_URL}/v1/logout-all`,
		() => new HttpResponse(null, { status: 200 }),
	),
	http.get(`${BASE_URL}/v1/sessions`, () =>
		HttpResponse.json<Schemas["beam_auth.server.oidc_routes.SessionSummary"][]>(
			[],
		),
	),
	http.delete(
		`${BASE_URL}/v1/sessions/:id`,
		() => new HttpResponse(null, { status: 204 }),
	),

	// ── browse ───────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/media`, () =>
		HttpResponse.json(emptyMediaConnection),
	),
	http.get(`${BASE_URL}/v1/media/:id`, () =>
		problem(404, "Not found", "#media-not-found"),
	),
	http.get(`${BASE_URL}/v1/media/:id/sources`, () =>
		HttpResponse.json<Schemas["beam_server.models.media.source.MediaSource"][]>(
			[],
		),
	),
	http.get(`${BASE_URL}/v1/genres`, () =>
		HttpResponse.json<Schemas["beam_server.routes.genres.GenreListResponse"]>({
			genres: [],
		}),
	),

	// ── libraries ────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/libraries`, () =>
		HttpResponse.json<Schemas["beam_server.models.library.Library"][]>([]),
	),
	http.get(`${BASE_URL}/v1/libraries/:id`, () =>
		problem(404, "Not found", "#media-not-found"),
	),
	http.get(`${BASE_URL}/v1/libraries/:id/files`, () =>
		HttpResponse.json<Schemas["beam_server.models.library.file.LibraryFile"][]>(
			[],
		),
	),

	// ── playback ─────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/continue-watching`, () =>
		HttpResponse.json<
			Schemas["beam_server.services.playback.ContinueWatchingItem"][]
		>([]),
	),
	http.get(`${BASE_URL}/v1/history`, () => HttpResponse.json(emptyHistory)),
	http.put(
		`${BASE_URL}/v1/files/:file_id/progress`,
		() => new HttpResponse(null, { status: 204 }),
	),

	// ── admin ────────────────────────────────────────────────────────────
	http.get(`${BASE_URL}/v1/admin/users`, () =>
		HttpResponse.json(emptyAdminUsers),
	),
	http.patch(`${BASE_URL}/v1/admin/users/:id`, () =>
		HttpResponse.json(factory.adminUser()),
	),
	http.get(`${BASE_URL}/v1/admin/logs`, () =>
		HttpResponse.json<Schemas["beam_server.models.admin.AdminLogEntryDto"][]>(
			[],
		),
	),
	http.get(`${BASE_URL}/v1/admin/logs/count`, () =>
		HttpResponse.json<
			Schemas["beam_server.models.admin.AdminLogCountResponse"]
		>({ count: 0 }),
	),
	http.get(`${BASE_URL}/v1/admin/status`, () => HttpResponse.json(adminStatus)),
	http.get(`${BASE_URL}/v1/admin/events`, () =>
		HttpResponse.json<Schemas["beam_server.models.admin.AdminEventDto"][]>([]),
	),
	http.post(`${BASE_URL}/v1/admin/libraries`, () =>
		HttpResponse.json(factory.library(), { status: 201 }),
	),
	http.post(`${BASE_URL}/v1/admin/libraries/:id/scan`, () =>
		HttpResponse.json<Schemas["beam_server.models.admin.ScanLibraryResponse"]>({
			added: 0,
		}),
	),
	http.delete(
		`${BASE_URL}/v1/admin/libraries/:id`,
		() => new HttpResponse(null, { status: 204 }),
	),
	http.post(
		`${BASE_URL}/v1/admin/media/:id/refresh`,
		() => new HttpResponse(null, { status: 202 }),
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
