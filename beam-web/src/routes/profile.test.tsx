import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster } from "sonner";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { mockGet, mockPost, mockDelete, mockNavigate, mockLogout, mockRefresh } =
	vi.hoisted(() => ({
		mockGet: vi.fn(),
		mockPost: vi.fn(),
		mockDelete: vi.fn(),
		mockNavigate: vi.fn(),
		mockLogout: vi.fn(),
		mockRefresh: vi.fn(),
	}));

vi.mock("@/lib/apiClient", () => ({
	apiClient: {
		GET: mockGet,
		POST: mockPost,
		DELETE: mockDelete,
	},
}));

vi.mock("@tanstack/react-router", () => ({
	createFileRoute: (_path: string) => (opts: Record<string, unknown>) => opts,
	redirect: (opts: unknown) => opts,
	useNavigate: () => mockNavigate,
}));

vi.mock("@/hooks/auth", () => ({
	useAuth: () => ({
		user: {
			id: "user-1",
			display_name: "Ada Lovelace",
			email: "ada@example.com",
		},
		isAuthenticated: true,
		isLoading: false,
		login: vi.fn(),
		logout: mockLogout,
		refresh: mockRefresh,
	}),
}));

import { ProfilePage } from "./profile";

const sessionA = {
	id: "sess-a",
	device_hash: "a".repeat(64),
	ip: "203.0.113.7",
	created_at: 1700000000,
	last_active: 1700003600,
};

const sessionB = {
	id: "sess-b",
	device_hash: "b".repeat(64),
	ip: "198.51.100.4",
	created_at: 1699000000,
	last_active: 1699003600,
};

function renderPage() {
	const queryClient = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
			mutations: { retry: false },
		},
	});
	return render(
		<QueryClientProvider client={queryClient}>
			<ProfilePage />
			<Toaster />
		</QueryClientProvider>,
	);
}

describe("ProfilePage active sessions", () => {
	beforeEach(() => {
		mockGet.mockReset();
		mockPost.mockReset();
		mockDelete.mockReset();
		mockNavigate.mockReset();
		mockLogout.mockReset();
		mockRefresh.mockReset();
		mockLogout.mockResolvedValue(undefined);
		mockRefresh.mockResolvedValue(undefined);
		mockGet.mockResolvedValue({ data: [sessionA, sessionB], error: undefined });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("renders a row per active session with device fingerprint and IP", async () => {
		renderPage();

		expect(await screen.findByText("203.0.113.7")).toBeInTheDocument();
		expect(screen.getByText("198.51.100.4")).toBeInTheDocument();
		// Truncated device fingerprint (first 12 chars of the hash).
		expect(screen.getByText(`${"a".repeat(12)}…`)).toBeInTheDocument();
		expect(screen.getAllByRole("button", { name: "Revoke" })).toHaveLength(2);
	});

	it("revokes a session by id and refetches the list", async () => {
		mockDelete.mockResolvedValue({
			data: undefined,
			error: undefined,
			response: { ok: true },
		});
		const user = userEvent.setup();
		renderPage();

		const revokeButtons = await screen.findAllByRole("button", {
			name: "Revoke",
		});
		await user.click(revokeButtons[0]);

		expect(mockDelete).toHaveBeenCalledWith(
			"/v1/sessions/{id}",
			expect.objectContaining({ params: { path: { id: "sess-a" } } }),
		);
		// invalidateQueries triggers a refetch: the sessions GET runs again.
		await waitFor(() => expect(mockGet.mock.calls.length).toBeGreaterThan(1));
		expect(mockRefresh).toHaveBeenCalled();
	});

	it("signs out of all sessions and routes through the login flow", async () => {
		vi.stubGlobal(
			"confirm",
			vi.fn(() => true),
		);
		mockPost.mockResolvedValue({
			data: undefined,
			error: undefined,
			response: { ok: true },
		});
		const user = userEvent.setup();
		renderPage();

		await user.click(
			await screen.findByRole("button", { name: /sign out all sessions/i }),
		);

		expect(mockPost).toHaveBeenCalledWith(
			"/v1/logout-all",
			expect.objectContaining({ credentials: "include" }),
		);
		await waitFor(() => expect(mockLogout).toHaveBeenCalled());
		expect(mockNavigate).toHaveBeenCalledWith({ to: "/login" });
	});

	it("does not fire logout-all when the confirmation is dismissed", async () => {
		vi.stubGlobal(
			"confirm",
			vi.fn(() => false),
		);
		const user = userEvent.setup();
		renderPage();

		await user.click(
			await screen.findByRole("button", { name: /sign out all sessions/i }),
		);

		expect(mockPost).not.toHaveBeenCalled();
		expect(mockNavigate).not.toHaveBeenCalled();
	});

	it("shows an error state when the sessions request fails", async () => {
		mockGet.mockResolvedValue({
			data: undefined,
			error: { message: "boom" },
		});
		renderPage();

		expect(
			await screen.findByText(/Failed to load active sessions/),
		).toBeInTheDocument();
	});
});
