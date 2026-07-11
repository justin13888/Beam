import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockGet } = vi.hoisted(() => ({
	mockGet: vi.fn(),
}));

vi.mock("@/lib/apiClient", () => ({
	apiClient: {
		GET: mockGet,
	},
}));

vi.mock("@tanstack/react-router", () => ({
	createFileRoute: (_path: string) => (opts: Record<string, unknown>) => opts,
	redirect: (opts: unknown) => opts,
	useNavigate: () => vi.fn(),
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

import { ExplorePage, type ExploreSearch } from "./explore";

interface PageInfoFixture {
	hasNext: boolean;
	endCursor?: string;
}

function movieEdge(id: string, title: string, cursor: string) {
	return {
		cursor,
		node: {
			Movie: {
				id,
				title: { original: title },
				year: 2000,
				poster_url: null,
				genres: [],
				streams: [],
			},
		},
	};
}

function showEdge(
	id: string,
	title: string,
	cursor: string,
	seasons: { poster_url: string | null }[],
) {
	return {
		cursor,
		node: {
			Show: {
				id,
				title: { original: title },
				year: 2010,
				seasons: seasons.map((s, i) => ({
					season_number: i,
					poster_url: s.poster_url,
					dates: {},
					genres: [],
					episodes: [],
				})),
			},
		},
	};
}

function connectionPage(
	edges: unknown[],
	{ hasNext, endCursor }: PageInfoFixture,
) {
	return {
		edges,
		page_info: {
			has_next_page: hasNext,
			has_previous_page: false,
			start_cursor: null,
			end_cursor: endCursor ?? null,
		},
	};
}

const emptyPage = connectionPage([], { hasNext: false });

/** Routes mocked GET calls: a genre list plus a queue of media pages served
 * in order (the last page repeats once the queue drains). */
function mockApi({
	genres = [] as string[],
	mediaPages = [emptyPage] as unknown[],
} = {}) {
	const queue = [...mediaPages];
	mockGet.mockImplementation(async (path: string) => {
		if (path === "/v1/genres") {
			return { data: { genres }, error: undefined };
		}
		const page = queue.length > 1 ? queue.shift() : queue[0];
		return { data: page, error: undefined };
	});
}

function renderPage(search: ExploreSearch = {}, navigate = vi.fn()) {
	const queryClient = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
		},
	});
	const utils = render(
		<QueryClientProvider client={queryClient}>
			<ExplorePage search={search} navigate={navigate} />
		</QueryClientProvider>,
	);
	return { ...utils, navigate };
}

/** The media (`/v1/media`) calls made so far, oldest first. */
function mediaCalls() {
	return mockGet.mock.calls.filter(([path]) => path === "/v1/media");
}

/** Applies the most recent navigate({ search }) update to `prev`. */
function lastNavigatedSearch(
	navigate: ReturnType<typeof vi.fn>,
	prev: ExploreSearch,
): ExploreSearch {
	const calls = navigate.mock.calls;
	expect(calls.length).toBeGreaterThan(0);
	const { search } = calls[calls.length - 1][0];
	return typeof search === "function" ? search(prev) : search;
}

describe("ExplorePage", () => {
	beforeEach(() => {
		mockGet.mockReset();
	});

	it("composes the URL search state into the GET /v1/media params", async () => {
		mockApi({ genres: ["Action"] });
		renderPage({
			q: "matrix",
			mediaType: "movie",
			genre: "Action",
			yearFrom: 1990,
			yearTo: 2001,
			minRating: 8,
			sortBy: "rating",
			sortOrder: "desc",
		});

		await waitFor(() => expect(mediaCalls().length).toBeGreaterThan(0));

		expect(mockGet).toHaveBeenCalledWith("/v1/media", {
			params: {
				query: {
					first: 48,
					after: undefined,
					sort_by: "rating",
					sort_order: "desc",
					query: "matrix",
					media_type: "movie",
					genre: "Action",
					year_from: 1990,
					year_to: 2001,
					// minRating is user-facing 0-10; the API takes 0-100.
					min_rating: 80,
				},
			},
			credentials: "include",
		});
	});

	it("defaults to alphabetical title sort when browsing without a query", async () => {
		mockApi();
		renderPage();

		await waitFor(() => expect(mediaCalls().length).toBeGreaterThan(0));

		expect(mockGet).toHaveBeenCalledWith(
			"/v1/media",
			expect.objectContaining({
				params: expect.objectContaining({
					query: expect.objectContaining({
						sort_by: "title",
						sort_order: "asc",
					}),
				}),
			}),
		);
	});

	it("loads the next page with the previous end_cursor and hides the button on the last page", async () => {
		mockApi({
			mediaPages: [
				connectionPage(
					[
						movieEdge("m1", "Movie One", "cursor-1"),
						movieEdge("m2", "Movie Two", "cursor-2"),
					],
					{ hasNext: true, endCursor: "cursor-2" },
				),
				connectionPage([movieEdge("m3", "Movie Three", "cursor-3")], {
					hasNext: false,
					endCursor: "cursor-3",
				}),
			],
		});
		const user = userEvent.setup();
		renderPage();

		// Titles render twice per card (poster fallback + caption); the
		// heading role pins the assertion to the caption.
		expect(
			await screen.findByRole("heading", { name: "Movie One" }),
		).toBeInTheDocument();
		expect(
			screen.getByRole("heading", { name: "Movie Two" }),
		).toBeInTheDocument();

		await user.click(screen.getByRole("button", { name: "Load more" }));

		// The second page is appended to the grid, not swapped in.
		expect(
			await screen.findByRole("heading", { name: "Movie Three" }),
		).toBeInTheDocument();
		expect(
			screen.getByRole("heading", { name: "Movie One" }),
		).toBeInTheDocument();

		expect(mockGet).toHaveBeenLastCalledWith(
			"/v1/media",
			expect.objectContaining({
				params: expect.objectContaining({
					query: expect.objectContaining({ after: "cursor-2" }),
				}),
			}),
		);

		// Last page reached: the affordance disappears.
		expect(
			screen.queryByRole("button", { name: "Load more" }),
		).not.toBeInTheDocument();
	});

	it("renders the genre select from GET /v1/genres", async () => {
		mockApi({ genres: ["Action", "Drama"] });
		renderPage();

		const select = await screen.findByLabelText("Genre");
		const options = Array.from(select.querySelectorAll("option")).map(
			(o) => o.textContent,
		);
		expect(options).toEqual(["All genres", "Action", "Drama"]);
	});

	it("hides the genre select when the genre list is empty", async () => {
		mockApi({ genres: [] });
		renderPage();

		await waitFor(() => expect(mediaCalls().length).toBeGreaterThan(0));
		expect(screen.queryByLabelText("Genre")).not.toBeInTheDocument();
	});

	it("clears every filter but keeps the search text", async () => {
		const prev: ExploreSearch = {
			q: "matrix",
			mediaType: "movie",
			genre: "Action",
			yearFrom: 1990,
			minRating: 8,
			sortBy: "rating",
			sortOrder: "desc",
		};
		mockApi({ genres: ["Action"] });
		const user = userEvent.setup();
		const { navigate } = renderPage(prev);

		await user.click(screen.getByRole("button", { name: /Clear filters/ }));

		expect(lastNavigatedSearch(navigate, prev)).toEqual({ q: "matrix" });
		const lastCall = navigate.mock.calls[navigate.mock.calls.length - 1][0];
		expect(lastCall.replace).toBe(true);
	});

	it("hides the clear-filters affordance when no filter is active", async () => {
		mockApi();
		renderPage({ q: "matrix" });

		await waitFor(() => expect(mediaCalls().length).toBeGreaterThan(0));
		expect(
			screen.queryByRole("button", { name: /Clear filters/ }),
		).not.toBeInTheDocument();
	});

	it("keeps the media-type toggle writing to the URL", async () => {
		mockApi();
		const user = userEvent.setup();
		const { navigate } = renderPage();

		await user.click(screen.getByRole("button", { name: "Shows" }));

		expect(lastNavigatedSearch(navigate, {})).toEqual({ mediaType: "show" });
	});

	it("shows the empty state when nothing is indexed", async () => {
		mockApi();
		renderPage();

		expect(await screen.findByText(/No media indexed yet/)).toBeInTheDocument();
	});

	it("uses the first season that has a poster for show cards", async () => {
		mockApi({
			mediaPages: [
				connectionPage(
					[
						showEdge("s1", "Specials First", "cursor-1", [
							{ poster_url: null },
							{ poster_url: "https://posters.test/season-1.jpg" },
						]),
					],
					{ hasNext: false },
				),
			],
		});
		renderPage();

		const poster = await screen.findByAltText("Specials First");
		expect(poster).toHaveAttribute("src", "https://posters.test/season-1.jpg");
	});
});
