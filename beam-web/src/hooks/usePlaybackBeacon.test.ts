import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { usePlaybackBeacon } from "./usePlaybackBeacon";

const { mockPut } = vi.hoisted(() => ({
	mockPut: vi.fn(),
}));

vi.mock("@/lib/apiClient", () => ({
	apiClient: {
		PUT: mockPut,
	},
}));

describe("usePlaybackBeacon", () => {
	beforeEach(() => {
		mockPut.mockReset();
		mockPut.mockResolvedValue({ data: undefined, response: { ok: true } });
	});

	it("does nothing when fileId is null", () => {
		const { result } = renderHook(() => usePlaybackBeacon(null));
		result.current.report(10, 100);
		expect(mockPut).not.toHaveBeenCalled();
	});

	it("reports the first position immediately", () => {
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));
		result.current.report(5, 100);
		expect(mockPut).toHaveBeenCalledWith("/v1/files/{file_id}/progress", {
			params: { path: { file_id: "file-1" } },
			body: { position_secs: 5, duration_secs: 100 },
			credentials: "include",
		});
	});

	it("throttles subsequent reports within the interval", () => {
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));
		result.current.report(5, 100);
		result.current.report(10, 100); // only 5s later, below the 15s throttle
		expect(mockPut).toHaveBeenCalledTimes(1);
	});

	it("reports again once past the throttle interval", () => {
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));
		result.current.report(5, 100);
		result.current.report(25, 100); // 20s later, past the 15s throttle
		expect(mockPut).toHaveBeenCalledTimes(2);
	});

	it("force bypasses the throttle regardless of elapsed time", () => {
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));
		result.current.report(5, 100);
		result.current.report(6, 100, true);
		expect(mockPut).toHaveBeenCalledTimes(2);
	});

	it("reset() clears the throttle so the next report always sends", () => {
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));
		result.current.report(5, 100);
		result.current.reset();
		result.current.report(6, 100);
		expect(mockPut).toHaveBeenCalledTimes(2);
	});
});
