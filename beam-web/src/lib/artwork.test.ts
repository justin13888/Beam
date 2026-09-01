import { describe, expect, it } from "vitest";
import { env } from "@/env";
import { artworkSrc } from "@/lib/artwork";

describe("artworkSrc", () => {
	it("resolves a server-relative path against the API origin, not the app's", () => {
		const resolved = artworkSrc("/v1/artwork/movie/abc/poster");

		// Derived from the configured origin rather than spelled out, and
		// contrasted with the document's own origin -- which is what an
		// unresolved `<img src>` would have used, and is the actual bug.
		const apiOrigin = new URL(env.C_STREAM_SERVER_URL).origin;
		expect(new URL(resolved as string).origin).toBe(apiOrigin);
		expect(new URL(resolved as string).pathname).toBe(
			"/v1/artwork/movie/abc/poster",
		);
		expect(apiOrigin).not.toBe(window.location.origin);
	});

	it("leaves an absolute URL alone", () => {
		const absolute = "https://image.tmdb.org/t/p/w500/poster.jpg";
		expect(artworkSrc(absolute)).toBe(absolute);
	});

	it("has nothing to render for a title with no artwork", () => {
		expect(artworkSrc(null)).toBeUndefined();
		expect(artworkSrc(undefined)).toBeUndefined();
		expect(artworkSrc("")).toBeUndefined();
	});
});
