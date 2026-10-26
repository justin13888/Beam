import { useState } from "react";

/**
 * Page-at-a-time navigation over a forward-only `/v1` connection.
 *
 * Every `/v1` list is a cursor connection (ADR-0021), and most of them take
 * only `first` and `after`: there is no `before`, so a page cannot be reached
 * backwards from the one after it. Going back is therefore remembered rather
 * than requested -- this keeps the `after` cursor each visited page was
 * fetched with, so "previous" re-requests exactly the page that was shown.
 *
 * `after` is what to send for the current page (`undefined` for the first,
 * which is how `openapi-fetch` leaves a query parameter off the wire).
 */
export interface CursorPages {
	/** Zero-based index of the current page. */
	page: number;
	/** The cursor the current page is fetched after. */
	after: string | undefined;
	/** Advance to the page after `endCursor`, the current page's last item. */
	next: (endCursor: string) => void;
	/** Return to the page before the current one. No-op on the first page. */
	prev: () => void;
	/** Return to the first page. */
	reset: () => void;
}

export function useCursorPages(): CursorPages {
	// One entry per visited page: the cursor that page was fetched after.
	const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);

	return {
		page: cursors.length - 1,
		after: cursors[cursors.length - 1],
		next: (endCursor) => setCursors((c) => [...c, endCursor]),
		prev: () => setCursors((c) => (c.length > 1 ? c.slice(0, -1) : c)),
		reset: () => setCursors([undefined]),
	};
}
