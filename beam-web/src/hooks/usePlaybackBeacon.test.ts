import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { recordRequests } from "@/test/requests";
import { usePlaybackBeacon } from "./usePlaybackBeacon";

describe("usePlaybackBeacon", () => {
	it("does nothing when fileId is null", async () => {
		const requests = recordRequests();
		const { result } = renderHook(() => usePlaybackBeacon(null));

		result.current.report(10, 100);

		expect(requests.all()).toEqual([]);
	});

	it("reports the first position immediately, as the file's progress", async () => {
		const requests = recordRequests();
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));

		result.current.report(5, 100);

		await waitFor(() => {
			const puts = requests.matching("PUT", "/v1/files/file-1/progress");
			expect(puts).toHaveLength(1);
			expect(puts[0].body).toEqual({ position_secs: 5, duration_secs: 100 });
		});
	});

	it("throttles subsequent reports within the interval", async () => {
		const requests = recordRequests();
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));

		result.current.report(5, 100);
		result.current.report(10, 100); // only 5s later, below the 15s throttle

		await waitFor(() =>
			expect(
				requests.matching("PUT", "/v1/files/file-1/progress"),
			).toHaveLength(1),
		);
	});

	it("reports again once past the throttle interval", async () => {
		const requests = recordRequests();
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));

		result.current.report(5, 100);
		result.current.report(25, 100); // 20s later, past the 15s throttle

		await waitFor(() =>
			expect(
				requests.matching("PUT", "/v1/files/file-1/progress"),
			).toHaveLength(2),
		);
	});

	it("force bypasses the throttle regardless of elapsed time", async () => {
		const requests = recordRequests();
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));

		result.current.report(5, 100);
		result.current.report(6, 100, true);

		await waitFor(() =>
			expect(
				requests.matching("PUT", "/v1/files/file-1/progress"),
			).toHaveLength(2),
		);
	});

	it("reset() clears the throttle so the next report always sends", async () => {
		const requests = recordRequests();
		const { result } = renderHook(() => usePlaybackBeacon("file-1"));

		result.current.report(5, 100);
		result.current.reset();
		result.current.report(6, 100);

		await waitFor(() =>
			expect(
				requests.matching("PUT", "/v1/files/file-1/progress"),
			).toHaveLength(2),
		);
	});
});
