import { HttpResponse } from "msw";

/**
 * An RFC 9457 problem document, in the shape beam-server actually sends.
 *
 * The doubles used to answer with `{ message, code }`, which matched neither
 * the old `text/plain`/`{"error": …}` bodies nor the problem documents that
 * replaced them. Nothing caught it: the literals are unannotated, so `tsc` had
 * nothing to check them against, and every error-path test asserted the
 * hardcoded English string the app threw away the body in favour of -- so a
 * test would have passed against a body of any shape at all, including none.
 *
 * @param status the HTTP status, which the document repeats.
 * @param detail the sentence for a person. Shown by the app for a 4xx.
 * @param code the fragment identifying the condition, or `about:blank` for a
 *   response the framework answered rather than the application.
 */
export function problem(
	status: number,
	detail: string,
	code = "about:blank",
): Response {
	const type =
		code === "about:blank"
			? code
			: `https://beam.justinchung.net/reference/errors/${code}`;

	return HttpResponse.json(
		{ type, title: detail, status, detail },
		{ status, headers: { "content-type": "application/problem+json" } },
	);
}
