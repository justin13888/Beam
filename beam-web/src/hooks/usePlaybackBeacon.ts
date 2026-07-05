import { useCallback, useRef } from "react";
import { apiClient } from "@/lib/apiClient";

/** Minimum playback-position delta (seconds) between two progress beacons
 * for the same file, so a `timeupdate` firing several times a second doesn't
 * turn into a PUT request several times a second. */
const REPORT_INTERVAL_SECS = 15;

/** Reports playback position to `PUT /v1/files/{file_id}/progress`, throttled
 * to at most once every `REPORT_INTERVAL_SECS` of playback per file (FR-507).
 * `force` bypasses the throttle for point-in-time events (pause, seek end,
 * unmount) where losing the last few seconds of progress would be
 * user-visible. */
export function usePlaybackBeacon(fileId: string | null) {
	const lastReportedPosition = useRef<number | null>(null);

	const report = useCallback(
		(positionSecs: number, durationSecs: number | undefined, force = false) => {
			if (!fileId) return;
			const last = lastReportedPosition.current;
			if (
				!force &&
				last !== null &&
				Math.abs(positionSecs - last) < REPORT_INTERVAL_SECS
			) {
				return;
			}
			lastReportedPosition.current = positionSecs;
			apiClient
				.PUT("/v1/files/{file_id}/progress", {
					params: { path: { file_id: fileId } },
					body: {
						position_secs: positionSecs,
						duration_secs: durationSecs ?? null,
					},
					credentials: "include",
				})
				.catch(console.error);
		},
		[fileId],
	);

	const reset = useCallback(() => {
		lastReportedPosition.current = null;
	}, []);

	return { report, reset };
}
