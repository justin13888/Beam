/**
 * Reading the server's RFC 9457 problem documents.
 *
 * Every `/v1` failure is one: `{type, title, status, detail}` served as
 * `application/problem+json`, where `type` is a stable identifier a client may
 * branch on and `detail` is a sentence for a person. See
 * https://beam.justinchung.net/reference/errors/.
 *
 * Until this existed, all 27 call sites did `if (error) throw new Error("Failed
 * to load libraries")` and the server's body was discarded unread -- so a
 * viewer saw the same seven words whether their session had expired, the title
 * had been removed, or the database was down.
 *
 * TODO(#146): this reads the body at runtime rather than through `api.gen.ts`,
 * which still describes every error as `text/plain: string` because it cannot
 * be regenerated from an OpenAPI 3.2 document yet. `openapi-fetch` parses the
 * body regardless of its declared type, so `error` really is the parsed
 * document at runtime; once the client is regenerated, `Problem` below should
 * come from `components["schemas"]["Problem"]` instead of being written out.
 */

/** The members of a problem document this app acts on. */
export interface Problem {
	/** The stable, machine-readable identifier. */
	type: string;
	/** The HTTP status, which the document repeats. */
	status: number;
	/** The occurrence-specific explanation, where the server sent one. */
	detail?: string;
}

/** RFC 9457's "the status code is the whole story". */
const ABOUT_BLANK = "about:blank";

/**
 * The code for a file the catalogue lists and the server no longer has.
 *
 * Matched as a suffix because the origin in front of it moves with the
 * deployment while the fragment is the stable half. Mirrors
 * `SOURCE_FILE_MISSING` in `BeamErrors.kt` and `sourceFileMissing` in
 * `BeamFailure.swift`.
 */
const SOURCE_FILE_MISSING = "#source-file-missing";

/**
 * Read a problem document out of whatever `openapi-fetch` put in `error`.
 *
 * Takes `unknown` and returns a fresh object rather than narrowing its
 * argument, so it typechecks against the stale generated types without a cast.
 */
export function problemFrom(error: unknown): Problem | null {
	if (typeof error !== "object" || error === null) return null;

	const candidate = error as Record<string, unknown>;
	if (typeof candidate.type !== "string") return null;
	if (typeof candidate.status !== "number") return null;

	return {
		type: candidate.type,
		status: candidate.status,
		detail: typeof candidate.detail === "string" ? candidate.detail : undefined,
	};
}

/**
 * A failed request, phrased for a person.
 *
 * `message` is what a screen renders; `code` and `status` are what it may
 * branch on.
 */
export class ApiError extends Error {
	/** The problem `type`, or `about:blank` when the body carried none. */
	readonly code: string;
	/** The HTTP status, where the response carried a problem document. */
	readonly status: number | null;

	constructor(message: string, code: string, status: number | null) {
		super(message);
		this.name = "ApiError";
		this.code = code;
		this.status = status;
	}
}

/**
 * Turn a failed request into something a screen can render.
 *
 * `fallback` is what to say when the server's own explanation is not fit to
 * show, which is the case more often than it looks:
 *
 * - A **5xx** `detail` is diagnostic text and frequently interpolates an
 *   internal error. Putting it in front of a viewer leaks implementation
 *   detail (NFR-108) and tells them nothing they can act on.
 * - A **`source-file-missing`** 404 reads, in the server's words, as though the
 *   viewer asked for the wrong thing. It means the catalogue and the server's
 *   storage have diverged, which only an administrator can fix -- the same
 *   distinction the Android and Apple clients draw.
 * - A response with **no problem document** is a proxy or gateway page, not
 *   beam-server, so there is nothing to read.
 *
 * Everything else -- a 400 naming the parameter, a 404 naming the title -- is
 * more specific than anything this layer could substitute, and is shown.
 */
export function apiError(error: unknown, fallback: string): ApiError {
	const problem = problemFrom(error);
	if (problem === null) return new ApiError(fallback, ABOUT_BLANK, null);

	if (problem.type.endsWith(SOURCE_FILE_MISSING)) {
		return new ApiError(
			"This title is in the library but its file is missing from the server. Ask an administrator to rescan the library.",
			problem.type,
			problem.status,
		);
	}

	const usable =
		problem.status < 500 &&
		problem.detail !== undefined &&
		problem.detail.trim() !== "";

	return new ApiError(
		usable ? (problem.detail as string) : fallback,
		problem.type,
		problem.status,
	);
}
