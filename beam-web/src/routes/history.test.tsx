import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockGet, mockPut, mockNavigate } = vi.hoisted(() => ({
	mockGet: vi.fn(),
	mockPut: vi.fn(),
	mockNavigate: vi.fn(),
}));

vi.mock("@/lib/apiClient", () => ({
	apiClient: {
		GET: mockGet,
		PUT: mockPut,
	},
}));

vi.mock("@tanstack/react-router", () => ({
	createFileRoute: (_path: string) => (opts: Record<string, unknown>) => opts,
	redirect: (opts: unknown) => opts,
	useNavigate: () => mockNavigate,
	Link: ({
		children,
		to,
		params,
		search,
		...rest
	}: {
		children: React.ReactNode;
		to?: string;
		params?: Record<string, string>;
		search?: Record<string, string>;
	}) => {
		// Build a concrete href from the typed link pieces so tests can assert
		// where a Link points.
		let href = to ?? "";
		for (const [key, value] of Object.entries(params ?? {})) {
			href = href.replace(`$${key}`, value);
		}
		if (search && Object.keys(search).length > 0) {
			href += `?${new URLSearchParams(search).toString()}`;
		}
		return (
			<a href={href} {...rest}>
				{children}
			</a>
		);
	},
}));

vi.mock("@/hooks/auth", () => ({
	useAuth: () => ({
		user: { id: "user-1", display_name: "Ada" },
		isAuthenticated: true,
		isLoading: false,
		login: vi.fn(),
		logout: vi.fn(),
		refresh: vi.fn(),
	}),
}));

import { HistoryPage } from "./history";

const movieInProgress = {
	file_id: "file-1",
	media_id: "movie-1",
	media_type: "movie",
	episode_id: null,
	position_secs: 600,
	duration_secs: 6000,
	completed: false,
	updated_at: new Date(Date.now() - 5 * 60_000).toISOString(),
};

const movieCompleted = {
	file_id: "file-2",
	media_id: "movie-2",
	media_type: "movie",
	episode_id: null,
	position_secs: 5900,
	duration_secs: 6000,
	completed: true,
	updated_at: new Date(Date.now() - 3 * 3600_000).toISOString(),
};

const showEpisode = {
	file_id: "file-3",
	media_id: "show-1",
	media_type: "show",
	episode_id: "ep-1",
	position_secs: 300,
	duration_secs: 1500,
	completed: false,
	updated_at: new Date(Date.now() - 2 * 86400_000).toISOString(),
};

const mediaById: Record<string, unknown> = {
	"movie-1": {
		Movie: {
			id: "movie-1",
			title: { original: "Inception" },
			poster_url: "https://img.example/inception.jpg",
			genres: [],
			streams: [],
		},
	},
	"movie-2": {
		Movie: {
			id: "movie-2",
			title: { original: "Arrival" },
			poster_url: null,
			genres: [],
			streams: [],
		},
	},
	"show-1": {
		Show: {
			id: "show-1",
			title: { original: "Severance" },
			seasons: [
				{
					season_number: 1,
					poster_url: "https://img.example/severance-s1.jpg",
					dates: {},
					genres: [],
					episodes: [{ id: "ep-1", episode_number: 2, title: "Half Loop" }],
				},
			],
		},
	},
};

/** Routes mocked GETs: `/v1/history` serves `items`/`total` (any offset),
 * `/v1/media/{id}` serves the fixture metadata. */
function mockApi({ items, total }: { items: unknown[]; total: number }) {
	mockGet.mockImplementation(
		async (path: string, opts: { params?: { path?: { id?: string } } }) => {
			if (path === "/v1/history") {
				return {
					data: { items, total },
					error: undefined,
					response: { status: 200, ok: true },
				};
			}
			if (path === "/v1/media/{id}") {
				const id = opts.params?.path?.id ?? "";
				return {
					data: mediaById[id],
					error: undefined,
					response: { status: 200, ok: true },
				};
			}
			throw new Error(`unexpected GET ${path}`);
		},
	);
}

function renderPage() {
	const queryClient = new QueryClient({
		defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
	});
	return render(
		<QueryClientProvider client={queryClient}>
			<HistoryPage />
		</QueryClientProvider>,
	);
}

describe("HistoryPage", () => {
	beforeEach(() => {
		mockGet.mockReset();
		mockPut.mockReset();
		mockNavigate.mockReset();
	});

	it("renders a row per history item with title, completed badge and episode tag", async () => {
		mockApi({
			items: [movieInProgress, movieCompleted, showEpisode],
			total: 3,
		});
		renderPage();

		expect(await screen.findByText("Inception")).toBeInTheDocument();
		// "Arrival" has no poster, so its title also renders inside the
		// poster-fallback tile — hence at least one match, not exactly one.
		expect(screen.getAllByText("Arrival").length).toBeGreaterThan(0);
		expect(screen.getByText("Severance")).toBeInTheDocument();
		// Only the completed movie carries the badge.
		expect(screen.getAllByText("Completed")).toHaveLength(1);
		// SxxEyy derived from the show metadata's seasons/episodes.
		expect(screen.getByText("S01E02")).toBeInTheDocument();
		// Relative "when watched" labels.
		expect(screen.getByText("Watched 5m ago")).toBeInTheDocument();
		expect(screen.getByText("Watched 3h ago")).toBeInTheDocument();
	});

	it("requests the next page with offset=50 when Next is clicked", async () => {
		mockApi({ items: [movieInProgress], total: 120 });
		const user = userEvent.setup();
		renderPage();

		await screen.findByText("Inception");
		expect(mockGet).toHaveBeenCalledWith(
			"/v1/history",
			expect.objectContaining({
				params: { query: { limit: 50, offset: 0 } },
			}),
		);

		await user.click(screen.getByRole("button", { name: /next/i }));

		await waitFor(() =>
			expect(mockGet).toHaveBeenCalledWith(
				"/v1/history",
				expect.objectContaining({
					params: { query: { limit: 50, offset: 50 } },
				}),
			),
		);
	});

	it("links Resume to the media page with the file deep-link", async () => {
		mockApi({ items: [movieInProgress], total: 1 });
		renderPage();

		const resume = await screen.findByRole("link", { name: /resume/i });
		expect(resume).toHaveAttribute("href", "/media/movie-1?fileId=file-1");
	});

	it("Start over zeroes progress for the right file then navigates to it", async () => {
		mockApi({ items: [movieInProgress], total: 1 });
		mockPut.mockResolvedValue({
			data: undefined,
			error: undefined,
			response: { ok: true },
		});
		const user = userEvent.setup();
		renderPage();

		await user.click(
			await screen.findByRole("button", { name: /start over/i }),
		);

		expect(mockPut).toHaveBeenCalledWith(
			"/v1/files/{file_id}/progress",
			expect.objectContaining({
				params: { path: { file_id: "file-1" } },
				body: { position_secs: 0, duration_secs: 6000 },
			}),
		);
		await waitFor(() =>
			expect(mockNavigate).toHaveBeenCalledWith({
				to: "/media/$id",
				params: { id: "movie-1" },
				search: { fileId: "file-1" },
			}),
		);
	});

	it("shows an empty state when nothing has been watched", async () => {
		mockApi({ items: [], total: 0 });
		renderPage();

		expect(await screen.findByText(/Nothing watched yet/)).toBeInTheDocument();
	});
});
