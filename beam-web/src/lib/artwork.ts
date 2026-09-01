import { env } from "@/env";

/**
 * Resolve an artwork path from the API against the API origin.
 *
 * The server returns artwork as a relative path (`/v1/artwork/movie/{id}/poster`)
 * rather than an absolute URL, for the same reason it does for `stream_url`:
 * one server is reached over a LAN address, a domain and a reverse proxy, and a
 * stored absolute URL would be wrong for two of the three. That makes resolving
 * it the client's job -- and an `<img src>` resolves a relative path against the
 * *app* origin, which is not the API origin in development.
 *
 * The session cookie rides along without anything here: an `<img>` is a
 * subresource request, and every supported deployment is same-site (different
 * ports on `localhost` in dev, one reverse-proxy origin in production), so
 * `SameSite=Lax` attaches it -- the same reasoning the `<video>` element relies
 * on, recorded in `docs/architecture/security.md`.
 *
 * An absolute URL passes through unchanged, so a stale cache or an older server
 * still renders instead of producing a mangled `src`.
 */
export function artworkSrc(
	path: string | null | undefined,
): string | undefined {
	if (!path) {
		return undefined;
	}
	return new URL(path, env.C_STREAM_SERVER_URL).toString();
}
