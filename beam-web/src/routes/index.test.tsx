import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import * as factory from "@/test/factories";
import { BASE_URL, meUnauthenticatedHandler } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

describe("/ (dashboard)", () => {
	it("totals the file counts across libraries", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/libraries`, () =>
				HttpResponse.json([
					factory.library({ id: "lib-1", name: "Movies", size: 40 }),
					factory.library({ id: "lib-2", name: "Shows", size: 2 }),
				]),
			),
		);
		renderRoute("/");

		// Two libraries, 42 files between them.
		expect(await screen.findByText("42")).toBeInTheDocument();
		expect(screen.getByText("2")).toBeInTheDocument();
	});

	it("lists each library with a link into its detail page", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/libraries`, () =>
				HttpResponse.json([
					factory.library({ id: "lib-1", name: "Movies", size: 3 }),
				]),
			),
		);
		renderRoute("/");

		const link = await screen.findByRole("link", { name: /Movies/ });
		expect(link).toHaveAttribute("href", "/libraries/lib-1");
	});

	it("does not fetch libraries for a signed-out visitor", async () => {
		const requests = recordRequests();
		server.use(meUnauthenticatedHandler);

		renderRoute("/");
		await waitForRouter();

		await waitFor(() =>
			expect(screen.getByText(/Welcome to/)).toBeInTheDocument(),
		);
		expect(requests.matching("GET", "/v1/libraries")).toEqual([]);
	});
});
