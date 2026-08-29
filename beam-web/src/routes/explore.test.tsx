import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import type { components } from "@/api.gen";
import * as factory from "@/test/factories";
import { BASE_URL, meUnauthenticatedHandler } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

type MediaConnection =
	components["schemas"]["beam_server.services.metadata.MediaConnection"];
type MediaEdge =
	components["schemas"]["beam_server.services.metadata.MediaEdge"];

function movieEdge(id: string, title: string, cursor: string): MediaEdge {
	return {
		cursor,
		node: {
			Movie: factory.movie({
				id,
				title: { original: title, localized: null, alternatives: [] },
				year: 2000,
			}),
		},
	};
}

function showEdge(
	id: string,
	title: string,
	cursor: string,
	seasons: { poster_url: string | null }[],
): MediaEdge {
	return {
		cursor,
		node: {
			Show: factory.show({
				id,
				title: { original: title, localized: null, alternatives: [] },
				year: 2010,
				seasons: seasons.map((s, i) => ({
					season_number: i,
					poster_url: s.poster_url,
					dates: {},
					genres: [],
					episodes: [],
				})),
			}),
		},
	};
}

function connectionPage(
	edges: MediaEdge[],
	{ hasNext, endCursor }: { hasNext: boolean; endCursor?: string },
): MediaConnection {
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

/**
 * Serve the genre list and a queue of media pages, in order (the last page
 * repeats once the queue drains).
 */
function serveExplore({
	genres = [] as string[],
	mediaPages = [emptyPage] as MediaConnection[],
} = {}) {
	const queue = [...mediaPages];
	server.use(
		http.get(`${BASE_URL}/v1/genres`, () => HttpResponse.json({ genres })),
		http.get(`${BASE_URL}/v1/media`, () =>
			HttpResponse.json(queue.length > 1 ? queue.shift() : queue[0]),
		),
	);
}

/** Query parameters of the media requests made so far, oldest first. */
function mediaQueries(requests: ReturnType<typeof recordRequests>) {
	return requests.matching("GET", "/v1/media").map((r) => r.query);
}

describe("/explore", () => {
	it("redirects to login when the session is not authenticated", async () => {
		server.use(meUnauthenticatedHandler);

		const { getLocation } = renderRoute("/explore");
		await waitForRouter();

		await waitFor(() =>
			expect(getLocation()).toBe("/login?redirect=%2Fexplore"),
		);
	});

	it("composes the URL search state into the GET /v1/media query string", async () => {
		const requests = recordRequests();
		serveExplore({ genres: ["Action"] });
		renderRoute(
			"/explore?q=matrix&mediaType=movie&genre=Action&yearFrom=1990&yearTo=2001&minRating=8&sortBy=rating&sortOrder=desc",
		);

		await waitFor(() =>
			expect(mediaQueries(requests).length).toBeGreaterThan(0),
		);
		const query = mediaQueries(requests)[0];

		expect(query.get("first")).toBe("48");
		expect(query.get("sort_by")).toBe("rating");
		expect(query.get("sort_order")).toBe("desc");
		expect(query.get("query")).toBe("matrix");
		expect(query.get("media_type")).toBe("movie");
		expect(query.get("genre")).toBe("Action");
		expect(query.get("year_from")).toBe("1990");
		expect(query.get("year_to")).toBe("2001");
		// minRating is user-facing 0-10; the API takes 0-100.
		expect(query.get("min_rating")).toBe("80");
		// An unset cursor must be absent, not the string "undefined".
		expect(query.has("after")).toBe(false);
	});

	it("defaults to alphabetical title sort when browsing without a query", async () => {
		const requests = recordRequests();
		serveExplore();
		renderRoute("/explore");

		await waitFor(() =>
			expect(mediaQueries(requests).length).toBeGreaterThan(0),
		);
		const query = mediaQueries(requests)[0];
		expect(query.get("sort_by")).toBe("title");
		expect(query.get("sort_order")).toBe("asc");
	});

	it("loads the next page with the previous end_cursor and hides the button on the last page", async () => {
		const requests = recordRequests();
		serveExplore({
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
		renderRoute("/explore");

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

		const queries = mediaQueries(requests);
		expect(queries[queries.length - 1].get("after")).toBe("cursor-2");

		// Last page reached: the affordance disappears.
		expect(
			screen.queryByRole("button", { name: "Load more" }),
		).not.toBeInTheDocument();
	});

	it("renders the genre select from GET /v1/genres", async () => {
		serveExplore({ genres: ["Action", "Drama"] });
		renderRoute("/explore");

		const select = await screen.findByLabelText("Genre");
		const options = Array.from(select.querySelectorAll("option")).map(
			(o) => o.textContent,
		);
		expect(options).toEqual(["All genres", "Action", "Drama"]);
	});

	it("hides the genre select when the genre list is empty", async () => {
		const requests = recordRequests();
		serveExplore({ genres: [] });
		renderRoute("/explore");

		await waitFor(() =>
			expect(mediaQueries(requests).length).toBeGreaterThan(0),
		);
		expect(screen.queryByLabelText("Genre")).not.toBeInTheDocument();
	});

	it("clears every filter but keeps the search text, in the URL", async () => {
		serveExplore({ genres: ["Action"] });
		const user = userEvent.setup();
		const { getLocation } = renderRoute(
			"/explore?q=matrix&mediaType=movie&genre=Action&yearFrom=1990&minRating=8&sortBy=rating&sortOrder=desc",
		);

		await user.click(
			await screen.findByRole("button", { name: /Clear filters/ }),
		);

		await waitFor(() => expect(getLocation()).toBe("/explore?q=matrix"));
	});

	it("hides the clear-filters affordance when no filter is active", async () => {
		const requests = recordRequests();
		serveExplore();
		renderRoute("/explore?q=matrix");

		await waitFor(() =>
			expect(mediaQueries(requests).length).toBeGreaterThan(0),
		);
		expect(
			screen.queryByRole("button", { name: /Clear filters/ }),
		).not.toBeInTheDocument();
	});

	it("writes the media-type toggle into the URL", async () => {
		serveExplore();
		const user = userEvent.setup();
		const { getLocation } = renderRoute("/explore");

		await user.click(await screen.findByRole("button", { name: "Shows" }));

		await waitFor(() => expect(getLocation()).toBe("/explore?mediaType=show"));
	});

	it("shows the empty state when nothing is indexed", async () => {
		serveExplore();
		renderRoute("/explore");

		expect(await screen.findByText(/No media indexed yet/)).toBeInTheDocument();
	});

	it("uses the first season that has a poster for show cards", async () => {
		serveExplore({
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
		renderRoute("/explore");

		const poster = await screen.findByAltText("Specials First");
		expect(poster).toHaveAttribute("src", "https://posters.test/season-1.jpg");
	});
});
