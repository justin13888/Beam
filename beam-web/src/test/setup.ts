import "@testing-library/jest-dom/vitest";
import { configure } from "@testing-library/react";
import { afterAll, afterEach, beforeAll } from "vitest";
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
	// Assigned rather than `vi.stubGlobal`: four suites call
	// `vi.unstubAllGlobals()` in `afterEach`, which clears that registry
	// and would take this with it after their first test.
	Object.defineProperty(globalThis, "IntersectionObserver", {
		value: NoopIntersectionObserver,
		configurable: true,
		writable: true,
	});
}

if (!("ResizeObserver" in globalThis)) {
	class NoopResizeObserver implements ResizeObserver {
		disconnect() {}
		observe() {}
		unobserve() {}
	}
	// Assigned rather than `vi.stubGlobal`: four suites call
	// `vi.unstubAllGlobals()` in `afterEach`, which clears that registry
	// and would take this with it after their first test.
	Object.defineProperty(globalThis, "ResizeObserver", {
		value: NoopResizeObserver,
		configurable: true,
		writable: true,
	});
}

// The same kind of gap, for web storage, and the one that actually bit.
//
// On Node >= 26 the bare `localStorage` global is Node's own experimental one,
// which is `undefined` unless `--localstorage-file` is passed -- and because
// vitest's jsdom environment makes `window === globalThis`, it shadows jsdom's
// implementation rather than sitting beside it. Both `globalThis.localStorage`
// and `window.localStorage` are therefore `undefined`, so there is nothing to
// point at and one has to be supplied.
//
// `@vidstack/react` reads `localStorage.getItem` at module scope, so every
// suite that transitively imports the video player -- which is every route
// test, since `renderRoute` mounts the real route tree -- died on import with
// `Cannot read properties of undefined (reading 'getItem')`. Twelve of the
// twenty suites never ran. It went unnoticed because `ts:test` has been
// switched off since #146.
//
// This is the environment, not a double for anything beam-web owns: auth is
// cookie-only and nothing here writes web storage. A per-test reset is still
// unnecessary for that reason.
//
// Installed per storage rather than behind one condition, and by assignment
// rather than `vi.stubGlobal`. The single `localStorage === undefined` guard
// it replaces could not tell the Node >= 26 shadowing it targets from jsdom
// failing to install storage at all, and it stubbed `sessionStorage` inside a
// branch keyed on *`localStorage`*'s absence -- so an environment where only
// the second was missing kept the hole. `vi.stubGlobal` registers in the same
// per-file registry that `vi.unstubAllGlobals()` clears, and four suites call
// that in `afterEach`, which unwound the shim after their first test. It is
// latent only because `@vidstack/react` reads at module scope, before any
// test runs; a plain assignment survives, as `window.matchMedia` above
// already does.
class MemoryStorage implements Storage {
	#entries = new Map<string, string>();

	get length() {
		return this.#entries.size;
	}
	key(index: number) {
		return [...this.#entries.keys()][index] ?? null;
	}
	getItem(key: string) {
		return this.#entries.get(key) ?? null;
	}
	setItem(key: string, value: string) {
		this.#entries.set(key, String(value));
	}
	removeItem(key: string) {
		this.#entries.delete(key);
	}
	clear() {
		this.#entries.clear();
	}
}

for (const name of ["localStorage", "sessionStorage"] as const) {
	if (globalThis[name] === undefined) {
		Object.defineProperty(globalThis, name, {
			value: new MemoryStorage(),
			configurable: true,
			writable: true,
		});
	}
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
