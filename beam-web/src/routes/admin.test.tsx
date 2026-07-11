import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster } from "sonner";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { mockGet, mockPatch } = vi.hoisted(() => ({
	mockGet: vi.fn(),
	mockPatch: vi.fn(),
}));

vi.mock("@/lib/apiClient", () => ({
	apiClient: {
		GET: mockGet,
		PATCH: mockPatch,
	},
}));

vi.mock("@tanstack/react-router", () => ({
	createFileRoute: (_path: string) => (opts: Record<string, unknown>) => opts,
	redirect: (opts: unknown) => opts,
	useNavigate: () => vi.fn(),
}));

// The authed operator is user-1: their own row must not offer a disable action.
vi.mock("@/hooks/auth", () => ({
	useAuth: () => ({
		user: {
			id: "user-1",
			display_name: "Ada Lovelace",
			email: "ada@example.com",
			is_admin: true,
		},
		isAuthenticated: true,
		isLoading: false,
		login: vi.fn(),
		logout: vi.fn(),
		refresh: vi.fn(),
	}),
}));

// SSE is exercised by the hook's own tests; here it is a no-op.
vi.mock("../hooks/useAdminEventStream", () => ({
	useAdminEventStream: () => ({ events: [], connected: false }),
}));

import { AdminPage, type AdminSearch, type AdminTab } from "./admin";

function adminUser(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		id: "user-2",
		display_name: "Grace Hopper",
		email: "grace@example.com",
		avatar_url: null,
		is_admin: false,
		disabled: false,
		created_at: "2024-01-02T00:00:00Z",
		...overrides,
	};
}

const statusResponse = {
	// 3d 4h 12m exactly: 3*86400 + 4*3600 + 12*60 = 274320
	uptime_secs: 274320,
	version: "1.2.3",
	counts: { users: 7, libraries: 3, files: 4242 },
	enrichment: { pending: 5, enriched: 900, unmatched: 11, failed: 2 },
	recent_scans: [
		{
			level: "info",
			message: "Scanned Movies library",
			timestamp: "2024-06-01T12:00:00Z",
		},
		{
			level: "error",
			message: "Failed to read /media/broken.mkv",
			timestamp: "2024-06-01T11:00:00Z",
		},
	],
};

/** Routes mocked GET calls by path. `users`/`total` back the users tab and
 * can be overridden per test; a fresh queue lets pagination assertions read
 * successive offsets. */
function mockApi({
	users = [adminUser()] as unknown[],
	total = 1,
	status = statusResponse,
	logs = [] as unknown[],
	logCount = 0,
} = {}) {
	mockGet.mockImplementation(async (path: string) => {
		switch (path) {
			case "/v1/admin/users":
				return { data: { items: users, total }, error: undefined };
			case "/v1/admin/status":
				return { data: status, error: undefined };
			case "/v1/admin/logs":
				return { data: logs, error: undefined };
			case "/v1/admin/logs/count":
				return { data: { count: logCount }, error: undefined };
			default:
				return { data: undefined, error: { message: `unexpected ${path}` } };
		}
	});
}

function renderPage(tab: AdminTab = "logs", navigate = vi.fn()) {
	const queryClient = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
			mutations: { retry: false },
		},
	});
	const utils = render(
		<QueryClientProvider client={queryClient}>
			<AdminPage tab={tab} navigate={navigate} />
			<Toaster />
		</QueryClientProvider>,
	);
	return { ...utils, navigate };
}

/** Resolves the most recent navigate({ search }) update against `prev`. */
function lastNavigatedSearch(
	navigate: ReturnType<typeof vi.fn>,
	prev: AdminSearch,
): AdminSearch {
	const calls = navigate.mock.calls;
	expect(calls.length).toBeGreaterThan(0);
	const { search } = calls[calls.length - 1][0];
	return typeof search === "function" ? search(prev) : search;
}

/** The GET calls made against a given path, oldest first. */
function callsTo(path: string) {
	return mockGet.mock.calls.filter(([p]) => p === path);
}

describe("AdminPage tabs", () => {
	beforeEach(() => {
		mockGet.mockReset();
		mockPatch.mockReset();
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("renders the three tabs and switches via URL search state", async () => {
		mockApi();
		const user = userEvent.setup();
		const { navigate } = renderPage("logs");

		expect(screen.getByRole("button", { name: "Users" })).toBeInTheDocument();
		expect(screen.getByRole("button", { name: "Status" })).toBeInTheDocument();
		expect(screen.getByRole("button", { name: "Logs" })).toBeInTheDocument();

		await user.click(screen.getByRole("button", { name: "Users" }));

		expect(lastNavigatedSearch(navigate, { tab: "logs" })).toEqual({
			tab: "users",
		});
	});
});

describe("AdminPage users tab", () => {
	beforeEach(() => {
		mockGet.mockReset();
		mockPatch.mockReset();
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("renders a row per user with name, email, admin badge, and disabled state", async () => {
		mockApi({
			users: [
				adminUser({
					id: "user-2",
					display_name: "Grace Hopper",
					email: "grace@example.com",
				}),
				adminUser({
					id: "user-3",
					display_name: "Alan Turing",
					email: "alan@example.com",
					is_admin: true,
				}),
				adminUser({
					id: "user-4",
					display_name: "Katherine Johnson",
					email: "katherine@example.com",
					disabled: true,
				}),
			],
			total: 3,
		});
		renderPage("users");

		expect(await screen.findByText("Grace Hopper")).toBeInTheDocument();
		expect(screen.getByText("grace@example.com")).toBeInTheDocument();
		expect(screen.getByText("Alan Turing")).toBeInTheDocument();
		// Read-only admin badge for the IdP-admin user.
		expect(screen.getByText("Admin")).toBeInTheDocument();
		// Local moderation state for the disabled user.
		expect(screen.getByText("Disabled")).toBeInTheDocument();
	});

	it("disables a user by id, confirming first, then refetches the list", async () => {
		vi.stubGlobal(
			"confirm",
			vi.fn(() => true),
		);
		mockApi({
			users: [adminUser({ id: "user-2", display_name: "Grace Hopper" })],
			total: 1,
		});
		mockPatch.mockResolvedValue({
			data: undefined,
			error: undefined,
			response: { ok: true },
		});
		const user = userEvent.setup();
		renderPage("users");

		await user.click(await screen.findByRole("button", { name: /Disable/ }));

		expect(mockPatch).toHaveBeenCalledWith(
			"/v1/admin/users/{id}",
			expect.objectContaining({
				params: { path: { id: "user-2" } },
				body: { disabled: true },
			}),
		);
		// invalidateQueries(["admin","users"]) triggers a refetch.
		await waitFor(() =>
			expect(callsTo("/v1/admin/users").length).toBeGreaterThan(1),
		);
	});

	it("does not offer a disable action on the operator's own row", async () => {
		mockApi({
			users: [
				adminUser({
					id: "user-1",
					display_name: "Ada Lovelace",
					email: "ada@example.com",
				}),
				adminUser({
					id: "user-2",
					display_name: "Grace Hopper",
					email: "grace@example.com",
				}),
			],
			total: 2,
		});
		renderPage("users");

		// Grace only appears in a row (Ada's name is also the header operator).
		await screen.findByText("Grace Hopper");
		// Exactly one Disable button (Grace's) -- Ada is the current operator.
		expect(screen.getAllByRole("button", { name: /Disable/ })).toHaveLength(1);
		expect(screen.getByText("You")).toBeInTheDocument();
	});

	it("requests the next page at offset 50", async () => {
		mockApi({
			users: [adminUser({ id: "user-2", display_name: "Grace Hopper" })],
			total: 60,
		});
		const user = userEvent.setup();
		renderPage("users");

		await screen.findByText("Grace Hopper");
		await user.click(screen.getByRole("button", { name: "Next" }));

		await waitFor(() =>
			expect(mockGet).toHaveBeenLastCalledWith(
				"/v1/admin/users",
				expect.objectContaining({
					params: { query: { limit: 50, offset: 50 } },
				}),
			),
		);
	});
});

describe("AdminPage status tab", () => {
	beforeEach(() => {
		mockGet.mockReset();
		mockPatch.mockReset();
	});

	it("renders humanized uptime, counts, enrichment tallies, and recent scans", async () => {
		mockApi();
		renderPage("status");

		expect(await screen.findByText("3d 4h 12m")).toBeInTheDocument();
		expect(screen.getByText("v1.2.3")).toBeInTheDocument();

		// Counts.
		expect(screen.getByText("4242")).toBeInTheDocument();
		// Enrichment tallies.
		expect(screen.getByText("900")).toBeInTheDocument();
		expect(screen.getByText("11")).toBeInTheDocument();

		// Recent scans.
		expect(screen.getByText("Scanned Movies library")).toBeInTheDocument();
		expect(
			screen.getByText("Failed to read /media/broken.mkv"),
		).toBeInTheDocument();
	});
});

describe("AdminPage logs tab", () => {
	beforeEach(() => {
		mockGet.mockReset();
		mockPatch.mockReset();
	});

	it("still renders the system log list", async () => {
		mockApi({
			logs: [
				{
					id: "log-1",
					level: "info",
					category: "system",
					message: "Server started",
					created_at: "2024-06-01T00:00:00Z",
					details: null,
				},
			],
			logCount: 1,
		});
		renderPage("logs");

		expect(await screen.findByText("System Logs")).toBeInTheDocument();
		expect(await screen.findByText("Server started")).toBeInTheDocument();
	});

	it("pages the log list, requesting offset 50 on Next and disabling it on the last page", async () => {
		// 60 entries at PAGE_SIZE 50 => two pages, so the pager renders.
		mockApi({
			logs: [
				{
					id: "log-1",
					level: "info",
					category: "system",
					message: "Server started",
					created_at: "2024-06-01T00:00:00Z",
					details: null,
				},
			],
			logCount: 60,
		});
		const user = userEvent.setup();
		renderPage("logs");

		await screen.findByText("Server started");
		// First page: Previous is disabled, Next is available.
		expect(screen.getByRole("button", { name: "Previous" })).toBeDisabled();

		await user.click(screen.getByRole("button", { name: "Next" }));

		// Advancing re-queries with the next page's offset.
		await waitFor(() =>
			expect(mockGet).toHaveBeenLastCalledWith(
				"/v1/admin/logs",
				expect.objectContaining({
					params: { query: { limit: 50, offset: 50 } },
				}),
			),
		);
		// Page 2 of 2 is the last page: Next disables (count-driven).
		await waitFor(() =>
			expect(screen.getByRole("button", { name: "Next" })).toBeDisabled(),
		);
	});
});
