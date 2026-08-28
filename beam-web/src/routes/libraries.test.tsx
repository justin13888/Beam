import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster } from "sonner";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { mockGet, mockPost, mockDelete } = vi.hoisted(() => ({
	mockGet: vi.fn(),
	mockPost: vi.fn(),
	mockDelete: vi.fn(),
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
	Link: ({
		children,
		to: _to,
		params: _params,
		...rest
	}: {
		children: React.ReactNode;
		to?: string;
		params?: Record<string, string>;
	}) => <a {...rest}>{children}</a>,
}));

vi.mock("@/hooks/auth", () => ({
	useAuth: () => ({ isAuthenticated: true }),
}));

import { LibrariesPage } from "./libraries.index";

const testLibrary = {
	id: "lib-1",
	name: "Movies",
	description: null,
	root_path: "/media/movies",
	size: 3,
	last_scan_started_at: null,
	last_scan_finished_at: null,
	last_scan_file_count: null,
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
			<LibrariesPage />
			<Toaster />
		</QueryClientProvider>,
	);
}

describe("LibrariesPage toasts", () => {
	beforeEach(() => {
		mockGet.mockReset();
		mockPost.mockReset();
		mockDelete.mockReset();
		// The libraries list query.
		mockGet.mockResolvedValue({ data: [testLibrary], error: undefined });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("shows a toast when a library scan is started", async () => {
		mockPost.mockResolvedValue({ data: {}, error: undefined });
		const user = userEvent.setup();
		renderPage();

		await user.click(await screen.findByTitle("Scan library"));

		expect(mockPost).toHaveBeenCalledWith(
			"/v1/admin/libraries/{id}/scan",
			expect.objectContaining({ params: { path: { id: "lib-1" } } }),
		);
		expect(
			await screen.findByText(/Scan started for "Movies"/),
		).toBeInTheDocument();
	});

	it("shows an error toast when deleting a library fails", async () => {
		vi.stubGlobal(
			"confirm",
			vi.fn(() => true),
		);
		mockDelete.mockResolvedValue({
			data: undefined,
			error: { message: "boom" },
			response: { ok: false },
		});
		const user = userEvent.setup();
		renderPage();

		await user.click(await screen.findByTitle("Delete library"));

		expect(
			await screen.findByText(/Failed to delete "Movies"/),
		).toBeInTheDocument();
	});
});
