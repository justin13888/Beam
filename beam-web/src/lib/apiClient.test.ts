import { waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import { BASE_URL } from "@/test/handlers";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";
import { apiClient } from "./apiClient";

/**
 * These assertions are about the requests the client actually puts on the
 * wire. The previous version of this file asserted that `apiClient.GET` was a
 * function -- a fact about `openapi-fetch`, not about beam-web, and one that
 * could not fail for any reason that mattered.
 */
describe("apiClient", () => {
	it("resolves path templates against the configured base URL", async () => {
		const requests = recordRequests();

		await apiClient.GET("/v1/media/{id}", {
			params: { path: { id: "abc-123" } },
		});

		const [request] = requests.all();
		expect(request.url).toBe(`${BASE_URL}/v1/media/abc-123`);
	});

	it("serializes query parameters and omits the ones left undefined", async () => {
		const requests = recordRequests();

		await apiClient.GET("/v1/media", {
			params: {
				query: { first: 48, query: "blade runner", after: undefined },
			},
		});

		const [request] = requests.all();
		expect(request.query.get("first")).toBe("48");
		expect(request.query.get("query")).toBe("blade runner");
		expect(request.query.has("after")).toBe(false);
	});

	it("sends the session cookie on every request without the caller asking", async () => {
		// `credentials: "include"` is a client-level default. A call site that
		// forgets it must still be authenticated, or it fails with a confusing
		// 401 instead of an obvious mistake.
		let credentials: RequestCredentials | undefined;
		server.use(
			http.get(`${BASE_URL}/v1/me`, ({ request }) => {
				credentials = request.credentials;
				return HttpResponse.json({
					id: "user-1",
					display_name: "Test User",
					is_admin: false,
				});
			}),
		);

		await apiClient.GET("/v1/me");

		expect(credentials).toBe("include");
	});

	it("surfaces an error response as `error`, with the server's body", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/media/:id`, () =>
				HttpResponse.json(
					{ message: "No such media", code: "not_found" },
					{ status: 404 },
				),
			),
		);

		const { data, error, response } = await apiClient.GET("/v1/media/{id}", {
			params: { path: { id: "missing" } },
		});

		expect(data).toBeUndefined();
		expect(response.status).toBe(404);
		expect(error).toMatchObject({
			message: "No such media",
			code: "not_found",
		});
	});

	it("puts a JSON body on the wire with the right content type", async () => {
		const requests = recordRequests();

		await apiClient.PUT("/v1/files/{file_id}/progress", {
			params: { path: { file_id: "file-1" } },
			body: { position_secs: 42, duration_secs: 100 },
		});

		const [request] = requests.all();
		expect(request.method).toBe("PUT");
		expect(request.path).toBe("/v1/files/file-1/progress");
		await waitFor(() =>
			expect(request.body).toEqual({ position_secs: 42, duration_secs: 100 }),
		);
	});
});
