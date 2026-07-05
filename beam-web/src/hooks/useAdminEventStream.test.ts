import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAdminEventStream } from "./useAdminEventStream";

class MockEventSource {
	static instances: MockEventSource[] = [];
	url: string;
	withCredentials: boolean;
	onopen: (() => void) | null = null;
	onerror: (() => void) | null = null;
	onmessage: ((event: { data: string }) => void) | null = null;
	close = vi.fn();

	constructor(url: string, options?: { withCredentials?: boolean }) {
		this.url = url;
		this.withCredentials = options?.withCredentials ?? false;
		MockEventSource.instances.push(this);
	}
}

function makeEvent(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		id: "evt-1",
		timestamp: "2026-01-01T00:00:00Z",
		level: "info",
		category: "system",
		message: "hello",
		library_id: null,
		library_name: null,
		...overrides,
	};
}

describe("useAdminEventStream", () => {
	beforeEach(() => {
		MockEventSource.instances = [];
		vi.stubGlobal("EventSource", MockEventSource);
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("does not open a connection when disabled", () => {
		renderHook(() => useAdminEventStream(false));
		expect(MockEventSource.instances).toHaveLength(0);
	});

	it("opens a connection with credentials when enabled", () => {
		renderHook(() => useAdminEventStream(true));
		expect(MockEventSource.instances).toHaveLength(1);
		expect(MockEventSource.instances[0].withCredentials).toBe(true);
		expect(MockEventSource.instances[0].url).toContain(
			"/v1/admin/events/stream",
		);
	});

	it("marks connected once the stream opens", async () => {
		const { result } = renderHook(() => useAdminEventStream(true));
		expect(result.current.connected).toBe(false);

		act(() => {
			MockEventSource.instances[0].onopen?.();
		});
		await waitFor(() => expect(result.current.connected).toBe(true));
	});

	it("prepends parsed events as they arrive", async () => {
		const { result } = renderHook(() => useAdminEventStream(true));
		const source = MockEventSource.instances[0];

		act(() => {
			source.onmessage?.({ data: JSON.stringify(makeEvent({ id: "evt-1" })) });
		});
		await waitFor(() => expect(result.current.events).toHaveLength(1));

		act(() => {
			source.onmessage?.({ data: JSON.stringify(makeEvent({ id: "evt-2" })) });
		});
		await waitFor(() => expect(result.current.events).toHaveLength(2));
		expect(result.current.events[0].id).toBe("evt-2");
	});

	it("marks disconnected on error", async () => {
		const { result } = renderHook(() => useAdminEventStream(true));
		const source = MockEventSource.instances[0];

		act(() => {
			source.onopen?.();
		});
		await waitFor(() => expect(result.current.connected).toBe(true));

		act(() => {
			source.onerror?.();
		});
		await waitFor(() => expect(result.current.connected).toBe(false));
	});

	it("closes the connection on unmount", () => {
		const { unmount } = renderHook(() => useAdminEventStream(true));
		const source = MockEventSource.instances[0];

		unmount();
		expect(source.close).toHaveBeenCalled();
	});
});
