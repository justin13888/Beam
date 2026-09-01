import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { afterEach, describe, expect, it, vi } from "vitest";
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

	// The point of the error taxonomy: the server's own sentence reaches the
	// viewer. It used to be gated behind `import.meta.env.DEV`, so in a
	// production build every failure on every route read "Something went
	// wrong" and the document was parsed and thrown away.
	// `DEV` is pinned false in both tests below, and that is what gives them
	// teeth. The gate they replace was `import.meta.env.DEV &&`, which vitest
	// leaves true -- so a test that did not pin it would render the message
	// either way and pass against the implementation it was written to reject.
	afterEach(() => {
		vi.unstubAllEnvs();
	});

	// A 400 rather than a 404: the media route turns a 404 into "no such
	// title" itself (`media.$id.tsx`) and never reaches the error boundary.
	it("shows the server's explanation for a client error in production", async () => {
		vi.stubEnv("DEV", false);
		server.use(
			http.get(`${BASE_URL}/v1/media/:id`, () =>
				problem(400, "movie-1 is not a valid id", "#invalid-media-id"),
			),
		);
		renderRoute("/media/movie-1");

		expect(
			await screen.findByText("movie-1 is not a valid id"),
		).toBeInTheDocument();
	});

	// The other half of the same rule (NFR-108). A 5xx `detail` is diagnostic
	// text that frequently interpolates an internal error, so the viewer gets
	// the fallback -- which is now rendered where nothing was before.
	it("shows the fallback rather than a 5xx detail", async () => {
		vi.stubEnv("DEV", false);
		server.use(
			http.get(`${BASE_URL}/v1/media/:id`, () =>
				problem(500, "connection refused at 10.0.0.7:5432", "#internal"),
			),
		);
		renderRoute("/media/movie-1");

		expect(
			await screen.findByText("Failed to load media metadata"),
		).toBeInTheDocument();
		expect(screen.queryByText(/10\.0\.0\.7/)).not.toBeInTheDocument();
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
