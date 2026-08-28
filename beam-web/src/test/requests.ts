import { afterEach } from "vitest";

import { server } from "./server";

/** One request the app actually put on the wire. */
export interface RecordedRequest {
	method: string;
	/** Path only, e.g. `/v1/history`. */
	path: string;
	/** Query string parameters, as sent. */
	query: URLSearchParams;
	/** Full URL, for assertions about the whole thing. */
	url: string;
	/** Parsed JSON body, or `undefined` for a body-less request. */
	body?: unknown;
}

/**
 * Record every request MSW sees for the duration of a test.
 *
 * This replaces asserting on a mocked `apiClient`'s call arguments. Those
 * assertions were about the arguments passed to a function the test itself
 * defined -- they could not catch a wrong URL template, a query parameter
 * serialized the wrong way, or a body the server would reject, because none of
 * that happens until `openapi-fetch` builds the real request.
 */
export function recordRequests(): {
	all: () => RecordedRequest[];
	matching: (method: string, path: string) => RecordedRequest[];
} {
	const recorded: RecordedRequest[] = [];

	// The entry is pushed synchronously so `matching(...)` sees a request the
	// moment it is issued; only `body` (which needs an async read of the
	// stream) is filled in afterwards. Tests asserting on a body therefore wrap
	// the assertion in `waitFor`, as they would anyway.
	const listener = ({ request }: { request: Request }) => {
		const url = new URL(request.url);
		const entry: RecordedRequest = {
			method: request.method,
			path: url.pathname,
			query: url.searchParams,
			url: request.url,
		};
		recorded.push(entry);
		if (request.method !== "GET" && request.method !== "HEAD") {
			void request
				.clone()
				.text()
				.then((text) => {
					if (text.length === 0) return;
					try {
						entry.body = JSON.parse(text);
					} catch {
						entry.body = text;
					}
				});
		}
	};

	server.events.on("request:start", listener);
	// Vitest tears down `server.events` between files; removing the listener
	// after each test keeps recorders from one test leaking into the next.
	afterEach(() => server.events.removeListener("request:start", listener));

	return {
		all: () => [...recorded],
		matching: (method, path) =>
			recorded.filter((r) => r.method === method && r.path === path),
	};
}
