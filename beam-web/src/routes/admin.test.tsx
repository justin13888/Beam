import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { components } from "@/api.gen";
import * as factory from "@/test/factories";
import { BASE_URL, meUnauthenticatedHandler } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { problem } from "@/test/problem";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

// The admin page opens a Server-Sent Events connection; `EventSource` does not
// exist in jsdom. The hook's own behaviour is covered by
// `src/hooks/useAdminEventStream.test.ts`; here it is a collaborator.
vi.mock("../hooks/useAdminEventStream", () => ({
	useAdminEventStream: () => ({ events: [], connected: false }),
}));

type AdminLogEntry = components["schemas"]["AdminLogEntry"];
type AdminStatus = components["schemas"]["AdminStatus"];

/** The operator running these tests. Their own row must not offer a disable. */
const operator = factory.user({
	id: "user-1",
	display_name: "Ada Lovelace",
	email: "ada@example.com",
	is_admin: true,
});

const statusResponse: AdminStatus = {
	// 3d 4h 12m exactly: 3*86400 + 4*3600 + 12*60 = 274320
	uptime_secs: 274320,
	version: "1.2.3",
	counts: { users: 7, libraries: 3, files: 4242 },
	enrichment: { pending: 5, enriched: 900, unmatched: 11, failed: 2 },
	recent_scans: [
		{
			level: "info",
			message: "Scanned Movies library",
			created_at: "2024-06-01T12:00:00Z",
		},
		{
			level: "error",
			message: "Failed to read /media/broken.mkv",
			created_at: "2024-06-01T11:00:00Z",
		},
	],
};

const serverStartedLog: AdminLogEntry = {
	id: "log-1",
	level: "info",
	category: "system",
	message: "Server started",
	created_at: "2024-06-01T00:00:00Z",
	details: null,
};

function serveAdmin({
	users = [factory.adminUser({ id: "user-2", display_name: "Grace Hopper" })],
	total = 1,
	status = statusResponse,
	logs = [] as AdminLogEntry[],
	logCount = 0,
	admin = true,
} = {}) {
	server.use(
		http.get(`${BASE_URL}/v1/me`, () =>
			HttpResponse.json(factory.user({ ...operator, is_admin: admin })),
		),
		http.get(`${BASE_URL}/v1/admin/users`, () =>
			HttpResponse.json({ items: users, total }),
		),
		http.get(`${BASE_URL}/v1/admin/status`, () => HttpResponse.json(status)),
		http.get(`${BASE_URL}/v1/admin/logs`, () => HttpResponse.json(logs)),
		http.get(`${BASE_URL}/v1/admin/logs/count`, () =>
			HttpResponse.json({ count: logCount }),
		),
	);
}

describe("/admin access", () => {
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("redirects an unauthenticated visitor to login", async () => {
		server.use(meUnauthenticatedHandler);

		const { getLocation } = renderRoute("/admin");
		await waitForRouter();

		// `tab` defaults into the URL via `validateSearch`, so the captured
		// redirect carries it -- which is the point: signing in lands the
		// operator back on the tab they asked for.
		await waitFor(() =>
			expect(getLocation()).toBe("/login?redirect=%2Fadmin%3Ftab%3Dlogs"),
		);
	});

	it("keeps a signed-in non-admin out of the admin surface", async () => {
		serveAdmin({ admin: false });

		const { getLocation } = renderRoute("/admin");
		await waitForRouter();

		await waitFor(() => expect(getLocation()).not.toContain("/admin"));
	});
});

describe("/admin tabs", () => {
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("renders the three tabs and switches via URL search state", async () => {
		serveAdmin();
		const user = userEvent.setup();
		const { getLocation } = renderRoute("/admin?tab=logs");

		expect(
			await screen.findByRole("button", { name: "Users" }),
		).toBeInTheDocument();
		expect(screen.getByRole("button", { name: "Status" })).toBeInTheDocument();
		expect(screen.getByRole("button", { name: "Logs" })).toBeInTheDocument();

		await user.click(screen.getByRole("button", { name: "Users" }));

		await waitFor(() => expect(getLocation()).toBe("/admin?tab=users"));
	});
});

describe("/admin users tab", () => {
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("renders a row per user with name, email, admin badge, and disabled state", async () => {
		serveAdmin({
			users: [
				factory.adminUser({
					id: "user-2",
					display_name: "Grace Hopper",
					email: "grace@example.com",
				}),
				factory.adminUser({
					id: "user-3",
					display_name: "Alan Turing",
					email: "alan@example.com",
					is_admin: true,
				}),
				factory.adminUser({
					id: "user-4",
					display_name: "Katherine Johnson",
					email: "katherine@example.com",
					disabled: true,
				}),
			],
			total: 3,
		});
		renderRoute("/admin?tab=users");

		expect(await screen.findByText("Grace Hopper")).toBeInTheDocument();
		expect(screen.getByText("grace@example.com")).toBeInTheDocument();
		expect(screen.getByText("Alan Turing")).toBeInTheDocument();
		// The header also greets an admin operator with an "Admin" link, so
		// scope the badge assertions to the user's own row.
		const row = (name: string) =>
			screen.getByText(name).closest("li") as HTMLElement;
		expect(row("Alan Turing")).toHaveTextContent("Admin");
		expect(row("Grace Hopper")).not.toHaveTextContent("Admin");
		expect(row("Katherine Johnson")).toHaveTextContent("Disabled");
		expect(row("Grace Hopper")).not.toHaveTextContent("Disabled");
	});

	it("disables a user by id, confirming first, then refetches the list", async () => {
		const requests = recordRequests();
		vi.stubGlobal(
			"confirm",
			vi.fn(() => true),
		);
		serveAdmin();
		const user = userEvent.setup();
		renderRoute("/admin?tab=users");

		await user.click(await screen.findByRole("button", { name: /Disable/ }));

		await waitFor(() => {
			const patches = requests.matching("PATCH", "/v1/admin/users/user-2");
			expect(patches).toHaveLength(1);
			expect(patches[0].body).toEqual({ disabled: true });
		});
		// invalidateQueries(["admin","users"]) triggers a refetch.
		await waitFor(() =>
			expect(
				requests.matching("GET", "/v1/admin/users").length,
			).toBeGreaterThan(1),
		);
	});

	it("does not disable anything when the confirmation is declined", async () => {
		const requests = recordRequests();
		vi.stubGlobal(
			"confirm",
			vi.fn(() => false),
		);
		serveAdmin();
		const user = userEvent.setup();
		renderRoute("/admin?tab=users");

		await user.click(await screen.findByRole("button", { name: /Disable/ }));

		await waitFor(() =>
			expect(screen.getByText("Grace Hopper")).toBeInTheDocument(),
		);
		expect(requests.matching("PATCH", "/v1/admin/users/user-2")).toEqual([]);
	});

	it("does not offer a disable action on the operator's own row", async () => {
		serveAdmin({
			users: [
				factory.adminUser({
					id: "user-1",
					display_name: "Ada Lovelace",
					email: "ada@example.com",
				}),
				factory.adminUser({
					id: "user-2",
					display_name: "Grace Hopper",
					email: "grace@example.com",
				}),
			],
			total: 2,
		});
		renderRoute("/admin?tab=users");

		await screen.findByText("Grace Hopper");
		expect(screen.getAllByRole("button", { name: /Disable/ })).toHaveLength(1);
		expect(screen.getByText("You")).toBeInTheDocument();
	});

	it("requests the next page at offset 50", async () => {
		const requests = recordRequests();
		serveAdmin({ total: 60 });
		const user = userEvent.setup();
		renderRoute("/admin?tab=users");

		await screen.findByText("Grace Hopper");
		await user.click(screen.getByRole("button", { name: "Next" }));

		await waitFor(() => {
			const gets = requests.matching("GET", "/v1/admin/users");
			const last = gets[gets.length - 1];
			expect(last.query.get("limit")).toBe("50");
			expect(last.query.get("offset")).toBe("50");
		});
	});
});

describe("/admin status tab", () => {
	it("renders humanized uptime, counts, enrichment tallies, and recent scans", async () => {
		serveAdmin();
		renderRoute("/admin?tab=status");

		expect(await screen.findByText("3d 4h 12m")).toBeInTheDocument();
		expect(screen.getByText("v1.2.3")).toBeInTheDocument();
		expect(screen.getByText("4242")).toBeInTheDocument();
		expect(screen.getByText("900")).toBeInTheDocument();
		expect(screen.getByText("11")).toBeInTheDocument();
		expect(screen.getByText("Scanned Movies library")).toBeInTheDocument();
		expect(
			screen.getByText("Failed to read /media/broken.mkv"),
		).toBeInTheDocument();
	});
});

const playbackReport = {
	enabled: false,
	from: "2026-09-24",
	to: "2026-10-23",
	retention_days: 400,
	totals: {
		started_count: 9,
		failed_count: 3,
		rebuffer_count: 4,
		rebuffer_total_ms: 9_000,
		source_switch_count: 2,
	},
	starts: [
		{
			client_kind: "android_tv",
			container: "matroska",
			video_codec: "hevc",
			audio_codec: "aac",
			height_class: "uhd",
			count: 9,
		},
	],
	start_failures: [
		{
			client_kind: "android_tv",
			reason: "audio_codec",
			stage: "preflight",
			container: "matroska",
			video_codec: "hevc",
			audio_codec: "truehd",
			height_class: "uhd",
			count: 3,
		},
	],
	rebuffers: [
		{
			client_kind: "android_tv",
			container: "matroska",
			video_codec: "hevc",
			height_class: "uhd",
			bitrate_class: "at_least_50_mbps",
			event_count: 4,
			total_ms: 9_000,
			histogram: [
				{ bucket: "under_1_secs", count: 1 },
				{ bucket: "from_1_to_3_secs", count: 3 },
				{ bucket: "from_3_to_10_secs", count: 0 },
				{ bucket: "from_10_to_30_secs", count: 0 },
				{ bucket: "at_least_30_secs", count: 0 },
			],
		},
	],
	source_switches: [
		{
			client_kind: "android_tv",
			trigger: "auto",
			from_height_class: "uhd",
			to_height_class: "fhd",
			count: 2,
		},
	],
};

describe("/admin playback tab", () => {
	it("shows failure rates, the stall histogram, and switches for the last thirty days", async () => {
		const requests = recordRequests();
		serveAdmin();
		server.use(
			http.get(`${BASE_URL}/v1/admin/telemetry/playback`, () =>
				HttpResponse.json(playbackReport),
			),
		);
		renderRoute("/admin?tab=playback");

		const byVideo = await screen.findByRole("region", {
			name: "Failure rate by video codec",
		});
		expect(byVideo).toHaveTextContent("hevc");
		expect(byVideo).toHaveTextContent("3 of 12 failed to start");
		expect(byVideo).toHaveTextContent("25%");
		expect(
			screen.getByRole("region", { name: "Failure rate by audio codec" }),
		).toHaveTextContent("truehd");
		expect(
			screen.getByRole("region", { name: "Failure rate by client" }),
		).toHaveTextContent("Android TV");
		expect(
			screen.getByRole("region", { name: "Stalls by length" }),
		).toHaveTextContent("1 to 3 s");
		expect(
			screen.getByRole("region", { name: "Source switches" }),
		).toHaveTextContent("4K to 1080p");
		expect(screen.getByText(/Collection is off/)).toBeInTheDocument();

		const sent = requests.matching("GET", "/v1/admin/telemetry/playback")[0];
		const from = new Date(`${sent.query.get("from")}T00:00:00Z`);
		const to = new Date(`${sent.query.get("to")}T00:00:00Z`);
		expect((to.getTime() - from.getTime()) / 86_400_000).toBe(29);
	});

	it("asks for the days picked", async () => {
		const requests = recordRequests();
		serveAdmin();
		server.use(
			http.get(`${BASE_URL}/v1/admin/telemetry/playback`, () =>
				HttpResponse.json(playbackReport),
			),
		);
		const user = userEvent.setup();
		renderRoute("/admin?tab=playback");
		await screen.findByRole("region", { name: "Failure rate by client" });

		await user.click(screen.getByRole("button", { name: "7 days" }));

		await waitFor(() => {
			const gets = requests.matching("GET", "/v1/admin/telemetry/playback");
			const last = gets[gets.length - 1];
			const from = new Date(`${last.query.get("from")}T00:00:00Z`);
			const to = new Date(`${last.query.get("to")}T00:00:00Z`);
			expect((to.getTime() - from.getTime()) / 86_400_000).toBe(6);
		});
		expect(screen.getByRole("button", { name: "7 days" })).toHaveAttribute(
			"aria-pressed",
			"true",
		);
	});

	it("says what went wrong when the report cannot be read", async () => {
		serveAdmin();
		server.use(
			http.get(`${BASE_URL}/v1/admin/telemetry/playback`, () =>
				problem(400, "to is before from", "#invalid-date-range"),
			),
		);
		renderRoute("/admin?tab=playback");

		expect(
			await screen.findByText("Failed to load playback telemetry"),
		).toBeInTheDocument();
		expect(screen.getByText("to is before from")).toBeInTheDocument();
	});
});

describe("/admin logs tab", () => {
	it("renders the system log list", async () => {
		serveAdmin({ logs: [serverStartedLog], logCount: 1 });
		renderRoute("/admin?tab=logs");

		expect(await screen.findByText("System Logs")).toBeInTheDocument();
		expect(await screen.findByText("Server started")).toBeInTheDocument();
	});

	it("pages the log list, requesting offset 50 on Next and disabling it on the last page", async () => {
		const requests = recordRequests();
		// 60 entries at PAGE_SIZE 50 => two pages, so the pager renders.
		serveAdmin({ logs: [serverStartedLog], logCount: 60 });
		const user = userEvent.setup();
		renderRoute("/admin?tab=logs");

		await screen.findByText("Server started");
		expect(screen.getByRole("button", { name: "Previous" })).toBeDisabled();

		await user.click(screen.getByRole("button", { name: "Next" }));

		await waitFor(() => {
			const gets = requests.matching("GET", "/v1/admin/logs");
			const last = gets[gets.length - 1];
			expect(last.query.get("limit")).toBe("50");
			expect(last.query.get("offset")).toBe("50");
		});
		// Page 2 of 2 is the last page: Next disables (count-driven).
		await waitFor(() =>
			expect(screen.getByRole("button", { name: "Next" })).toBeDisabled(),
		);
	});
});
