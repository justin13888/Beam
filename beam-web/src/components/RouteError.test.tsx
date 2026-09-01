import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import * as factory from "@/test/factories";
import { BASE_URL } from "@/test/handlers";
import { renderRoute } from "@/test/harness";
import { problem } from "@/test/problem";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

/**
 * Reached through a route whose loader fails, because that is the only way it
 * is reached in production -- rendering it directly would not exercise the
 * `router.invalidate()` retry, which is its entire reason to exist.
 */
describe("RouteError", () => {
	it("replaces the failed route with a retry affordance", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/media/:id`, () =>
				problem(500, "boom", "#internal"),
			),
		);
		renderRoute("/media/movie-1");

		expect(
			await screen.findByRole("heading", { name: /Something went wrong/ }),
		).toBeInTheDocument();
		expect(
			screen.getByRole("button", { name: /Try again/ }),
		).toBeInTheDocument();
	});

	it("retrying re-runs the failed request and recovers when it succeeds", async () => {
		let attempts = 0;
		server.use(
			http.get(`${BASE_URL}/v1/media/:id`, () => {
				attempts += 1;
				return attempts === 1
					? problem(500, "boom", "#internal")
					: HttpResponse.json({
							Movie: factory.movie({
								id: "movie-1",
								title: {
									original: "Recovered",
									localized: null,
									alternatives: [],
								},
							}),
						});
			}),
			http.get(`${BASE_URL}/v1/media/:id/sources`, () => HttpResponse.json([])),
		);
		const requests = recordRequests();
		const user = userEvent.setup();
		renderRoute("/media/movie-1");

		await user.click(await screen.findByRole("button", { name: /Try again/ }));

		await waitFor(() =>
			expect(
				requests.matching("GET", "/v1/media/movie-1").length,
			).toBeGreaterThan(1),
		);
		expect(
			await screen.findByRole("heading", { name: "Recovered" }),
		).toBeInTheDocument();
	});
});
