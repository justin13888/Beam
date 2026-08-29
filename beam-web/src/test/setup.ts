import "@testing-library/jest-dom/vitest";
import { configure } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, vi } from "vitest";
import { server } from "./server";

// `renderRoute` mounts a real router: a page is not on screen until the auth
// check and then the page's own query have both round-tripped through MSW.
// Testing Library's 1s default is enough on an idle machine and not enough on
// a loaded one, which showed up as three tests that passed alone and failed in
// the full run. Five seconds was not enough either: under CPU contention a
// batch of tests lands at 5.1-5.8s and fails together, and CI runners are
// slower than a dev machine. Fifteen still fails fast on a genuine break --
// the alternative, sprinkling per-call timeouts, hides the reason.
configure({ asyncUtilTimeout: 15_000 });

// Browser APIs jsdom does not implement. These are environment gaps, not
// doubles for anything beam-web owns: without them a component that merely
// *mounts* a media-query hook or a lazy-loading image throws, which would
// force those components to be mocked away -- the thing this suite is trying
// to stop doing.
if (!window.matchMedia) {
	window.matchMedia = (query: string) =>
		({
			matches: false,
			media: query,
			onchange: null,
			addListener: () => {},
			removeListener: () => {},
			addEventListener: () => {},
			removeEventListener: () => {},
			dispatchEvent: () => false,
		}) as MediaQueryList;
}

if (!("IntersectionObserver" in globalThis)) {
	class NoopIntersectionObserver implements IntersectionObserver {
		readonly root = null;
		readonly rootMargin = "";
		readonly thresholds: readonly number[] = [];
		disconnect() {}
		observe() {}
		unobserve() {}
		takeRecords(): IntersectionObserverEntry[] {
			return [];
		}
	}
	vi.stubGlobal("IntersectionObserver", NoopIntersectionObserver);
}

if (!("ResizeObserver" in globalThis)) {
	class NoopResizeObserver implements ResizeObserver {
		disconnect() {}
		observe() {}
		unobserve() {}
	}
	vi.stubGlobal("ResizeObserver", NoopResizeObserver);
}

beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
// No localStorage/sessionStorage reset here: auth is cookie-only and nothing
// in beam-web writes web storage. (On Node >= 26 the bare `localStorage`
// global is Node's own experimental, undefined-without-a-flag one -- jsdom's
// gets shadowed -- so touching it here also breaks every test.)
afterEach(() => {
	server.resetHandlers();
});
afterAll(() => server.close());
