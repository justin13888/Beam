import { describe, expect, it } from "vitest";
import { apiError, problemFrom } from "@/lib/problem";

const BASE = "https://beam.justinchung.net/reference/errors/";

describe("problemFrom", () => {
	it("reads the members a screen acts on", () => {
		expect(
			problemFrom({
				type: `${BASE}#media-not-found`,
				title: "Media not found",
				status: 404,
				detail: "media 7 not found",
				instance: "/v1/media/7",
			}),
		).toEqual({
			type: `${BASE}#media-not-found`,
			status: 404,
			detail: "media 7 not found",
		});
	});

	// A proxy's HTML page, a gateway's plain text, a network failure that never
	// produced a body: none of these came from beam-server, so there is no
	// document to find.
	it("returns null for anything that is not a problem document", () => {
		expect(problemFrom(undefined)).toBeNull();
		expect(problemFrom(null)).toBeNull();
		expect(problemFrom("502 Bad Gateway")).toBeNull();
		expect(problemFrom({ error: "Media not found" })).toBeNull();
		expect(problemFrom({ type: `${BASE}#internal` })).toBeNull();
	});
});

describe("apiError", () => {
	it("shows the server's explanation for a client error", () => {
		const error = apiError(
			{
				type: `${BASE}#invalid-library-id`,
				status: 400,
				detail: "library id 7 is not a valid identifier",
			},
			"Failed to load libraries",
		);

		expect(error.message).toBe("library id 7 is not a valid identifier");
		expect(error.code).toBe(`${BASE}#invalid-library-id`);
		expect(error.status).toBe(400);
	});

	// A 500's detail is diagnostic text and frequently interpolates an internal
	// error. Showing it leaks implementation detail (NFR-108) and tells a
	// viewer nothing they can act on.
	it("keeps a server fault's own words out of the interface", () => {
		const error = apiError(
			{
				type: `${BASE}#internal`,
				status: 500,
				detail: "Database error: connection refused",
			},
			"Failed to load libraries",
		);

		expect(error.message).toBe("Failed to load libraries");
		expect(error.code).toBe(`${BASE}#internal`);
	});

	// Both of these are 404s, so the status cannot separate them. One means the
	// viewer asked for something that is not there; the other means the
	// catalogue still lists the title and the server no longer has its file,
	// which no amount of retrying fixes and which only an operator can.
	it("tells a missing source file from a missing title", () => {
		const absent = apiError(
			{
				type: `${BASE}#media-not-found`,
				status: 404,
				detail: "media 7 not found",
			},
			"Failed to load media",
		);
		const diverged = apiError(
			{
				type: `${BASE}#source-file-missing`,
				status: 404,
				detail: "Source video file not found",
			},
			"Failed to load media",
		);

		expect(absent.message).toBe("media 7 not found");
		expect(diverged.message).toMatch(/administrator/);
		expect(diverged.message).not.toBe("Source video file not found");
	});

	it("falls back when the response carried no problem document", () => {
		const error = apiError(
			"<html>502 Bad Gateway</html>",
			"Failed to load libraries",
		);

		expect(error.message).toBe("Failed to load libraries");
		expect(error.code).toBe("about:blank");
		expect(error.status).toBeNull();
	});

	it("falls back when the document carried no detail", () => {
		const error = apiError(
			{ type: `${BASE}#media-not-found`, status: 404 },
			"Failed to load media",
		);

		expect(error.message).toBe("Failed to load media");
	});
});
