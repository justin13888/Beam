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

const testLibrary = factory.library({ id: "lib-1", name: "Movies", size: 3 });

function serveLibraries(...extra: Parameters<typeof server.use>) {
	server.use(
		http.get(`${BASE_URL}/v1/libraries`, () =>
			HttpResponse.json([testLibrary]),
		),
		...extra,
	);
}

describe("/libraries", () => {
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("lists the libraries the server returns", async () => {
		serveLibraries();
		renderRoute("/libraries");

		expect(await screen.findByText("Movies")).toBeInTheDocument();
	});

	it("starts a scan against the library's own id and confirms it", async () => {
		const requests = recordRequests();
		serveLibraries();
		const user = userEvent.setup();
		renderRoute("/libraries");

		await user.click(await screen.findByTitle("Scan library"));

		await waitFor(() =>
			expect(
				requests.matching("POST", "/v1/admin/libraries/lib-1/scan"),
			).toHaveLength(1),
		);
		expect(
			await screen.findByText(/Scan started for "Movies"/),
		).toBeInTheDocument();
	});

	it("surfaces a failed delete as an error toast rather than silently", async () => {
		serveLibraries(
			http.delete(`${BASE_URL}/v1/admin/libraries/:id`, () =>
				problem(500, "boom", "#internal"),
			),
		);
		vi.stubGlobal(
			"confirm",
			vi.fn(() => true),
		);
		const user = userEvent.setup();
		renderRoute("/libraries");

		await user.click(await screen.findByTitle("Delete library"));

		expect(
			await screen.findByText(/Failed to delete "Movies"/),
		).toBeInTheDocument();
	});

	it("does not delete anything when the confirmation is declined", async () => {
		const requests = recordRequests();
		serveLibraries();
		vi.stubGlobal(
			"confirm",
			vi.fn(() => false),
		);
		const user = userEvent.setup();
		renderRoute("/libraries");

		await user.click(await screen.findByTitle("Delete library"));

		await waitFor(() => expect(screen.getByText("Movies")).toBeInTheDocument());
		expect(requests.matching("DELETE", "/v1/admin/libraries/lib-1")).toEqual(
			[],
		);
	});

	it("shows the empty state and no cards when nothing is configured", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/libraries`, () => HttpResponse.json([])),
		);
		renderRoute("/libraries");

		expect(await screen.findByText(/No libraries yet/)).toBeInTheDocument();
	});

	it("surfaces a failed list as an error state with a retry", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/libraries`, () =>
				problem(500, "boom", "#internal"),
			),
		);
		renderRoute("/libraries");

		expect(
			await screen.findByText(/Failed to load libraries/),
		).toBeInTheDocument();
	});

	// A 500's `detail` is diagnostic text that frequently interpolates an
	// internal error, so it is the one case where the server's own words are
	// deliberately not shown (NFR-108). The test above asserts the fallback;
	// this asserts that the internal message did not come with it.
	it("does not put a 500's internal message in front of a viewer", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/libraries`, () =>
				problem(500, "Database error: connection refused", "#internal"),
			),
		);
		renderRoute("/libraries");

		await screen.findByText(/Failed to load libraries/);
		expect(screen.queryByText(/connection refused/)).not.toBeInTheDocument();
	});

	// The bug this whole path existed to hide: every call site threw a
	// hardcoded string and discarded the body, so a viewer saw the same seven
	// words whichever of these came back.
	it("shows the server's own explanation for a client error", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/libraries`, () =>
				problem(
					400,
					"library id 7 is not a valid identifier",
					"#invalid-library-id",
				),
			),
		);
		renderRoute("/libraries");

		expect(
			await screen.findByText(/not a valid identifier/),
		).toBeInTheDocument();
	});

	it("creates a library from the form and refetches the list", async () => {
		const requests = recordRequests();
		serveLibraries();
		const user = userEvent.setup();
		renderRoute("/libraries");

		await user.click(
			await screen.findByRole("button", { name: /Add Library/ }),
		);
		await user.type(screen.getByLabelText("Name"), "Shows");
		await user.type(screen.getByLabelText("Root Path"), "/media/shows");
		await user.click(screen.getByRole("button", { name: "Create" }));

		await waitFor(() => {
			const posts = requests.matching("POST", "/v1/admin/libraries");
			expect(posts).toHaveLength(1);
			expect(posts[0].body).toEqual({
				name: "Shows",
				root_path: "/media/shows",
			});
		});
		// invalidateQueries triggers a refetch of the list.
		await waitFor(() =>
			expect(requests.matching("GET", "/v1/libraries").length).toBeGreaterThan(
				1,
			),
		);
	});

	it("surfaces a failed create as an error toast and keeps the form open", async () => {
		serveLibraries(
			http.post(`${BASE_URL}/v1/admin/libraries`, () =>
				problem(500, "boom", "#internal"),
			),
		);
		const user = userEvent.setup();
		renderRoute("/libraries");

		await user.click(
			await screen.findByRole("button", { name: /Add Library/ }),
		);
		await user.type(screen.getByLabelText("Name"), "Shows");
		await user.type(screen.getByLabelText("Root Path"), "/media/shows");
		await user.click(screen.getByRole("button", { name: "Create" }));

		expect(
			await screen.findByText(/Failed to create library/),
		).toBeInTheDocument();
		expect(screen.getByLabelText("Name")).toHaveValue("Shows");
	});
});
