import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import * as factory from "@/test/factories";
import { BASE_URL, meUnauthenticatedHandler } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { problem } from "@/test/problem";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

/**
 * Exercised through the dashboard route that mounts it, so the `useQueries`
 * fan-out runs against the real router and the real client. Rendering it in
 * isolation would need its own router context anyway.
 */
function serveContinueWatching(
	items: ReturnType<typeof factory.continueWatchingItem>[],
	media: Record<string, unknown> = {},
) {
	server.use(
		http.get(`${BASE_URL}/v1/continue-watching`, () =>
			HttpResponse.json(items),
		),
		http.get(`${BASE_URL}/v1/media/:id`, ({ params }) => {
			const found = media[String(params.id)];
			return found
				? HttpResponse.json(found)
				: problem(404, "Not found", "#media-not-found");
		}),
	);
}

describe("ContinueWatchingRow", () => {
	it("renders nothing when there is nothing in progress", async () => {
		serveContinueWatching([]);
		renderRoute("/");
		await waitForRouter();

		await waitFor(() =>
			expect(screen.getByText(/Welcome to/)).toBeInTheDocument(),
		);
		expect(screen.queryByText("Continue Watching")).not.toBeInTheDocument();
	});

	it("links each card straight back to the exact file that was playing", async () => {
		serveContinueWatching(
			[
				factory.continueWatchingItem({
					file_id: "file-9",
					media_id: "movie-9",
					position_secs: 300,
					duration_secs: 1200,
				}),
			],
			{
				"movie-9": {
					Movie: factory.movie({
						id: "movie-9",
						title: {
							original: "Primer",
							localized: null,
							alternatives: [],
						},
						poster_url: null,
					}),
				},
			},
		);
		renderRoute("/");

		expect(await screen.findByText("Continue Watching")).toBeInTheDocument();
		const link = await screen.findByRole("link", { name: /Primer/ });
		expect(link).toHaveAttribute("href", "/media/movie-9?fileId=file-9");
		// 1200 - 300 = 900s remaining, rendered as m:ss.
		expect(screen.getByText("15:00 left")).toBeInTheDocument();
	});

	it("drops an item whose media document 404s rather than rendering a blank card", async () => {
		serveContinueWatching(
			[
				factory.continueWatchingItem({
					file_id: "file-gone",
					media_id: "movie-gone",
				}),
				factory.continueWatchingItem({
					file_id: "file-ok",
					media_id: "movie-ok",
				}),
			],
			{
				"movie-ok": {
					Movie: factory.movie({
						id: "movie-ok",
						title: {
							original: "Still Here",
							localized: null,
							alternatives: [],
						},
					}),
				},
			},
		);
		renderRoute("/");

		expect(await screen.findByText("Continue Watching")).toBeInTheDocument();
		expect(
			await screen.findByRole("link", { name: /Still Here/ }),
		).toBeInTheDocument();
		expect(
			screen
				.getAllByRole("link")
				.filter((el) => el.textContent?.includes("left")),
		).toHaveLength(1);
	});

	it("does not query continue-watching at all for a signed-out visitor", async () => {
		const requests = recordRequests();
		server.use(meUnauthenticatedHandler);

		renderRoute("/");
		await waitForRouter();

		await waitFor(() =>
			expect(screen.getByText(/Welcome to/)).toBeInTheDocument(),
		);
		expect(requests.matching("GET", "/v1/continue-watching")).toEqual([]);
	});
});
