import { useQuery } from "@tanstack/react-query";
import type { components } from "@/api.gen";
import { apiClient } from "@/lib/apiClient";
import { apiError } from "@/lib/problem";

/**
 * What `GET /v1/admin/telemetry/playback` answers: daily counts of playback
 * starts, start failures, rebuffers and source switches, summed over a range
 * of UTC days. No row names a user, a file or a title.
 *
 * TODO(#146): typed by the current schema name, which the stale `api.gen.ts`
 * does not carry yet; it resolves once the client is regenerated.
 */
export type PlaybackTelemetryReport =
	components["schemas"]["PlaybackTelemetryReport"];

/** The duration ranges a rebuffer falls into, shortest first, as the server
 * names them. */
export const REBUFFER_BUCKETS = [
	"under_1_secs",
	"from_1_to_3_secs",
	"from_3_to_10_secs",
	"from_10_to_30_secs",
	"at_least_30_secs",
] as const;

export type RebufferBucket = (typeof REBUFFER_BUCKETS)[number];

/** Starts and start failures under one value of one dimension. */
export interface FailureRateRow {
	/** The value as the server names it: `hevc`, `android_tv`. */
	label: string;
	started: number;
	failed: number;
	/** `failed / (started + failed)`, from 0 to 1. */
	failureRate: number;
}

/** The dimensions a failure rate can be read by. */
export type FailureDimension = "video_codec" | "audio_codec" | "client_kind";

/**
 * Join starts and failures on one dimension, most attempts first.
 *
 * A codec that only ever failed still has a row -- it is the row an operator
 * most needs to see -- so the join is over the union of both lists.
 */
export function failureRates(
	report: Pick<PlaybackTelemetryReport, "starts" | "start_failures">,
	dimension: FailureDimension,
): FailureRateRow[] {
	const tally = new Map<string, { started: number; failed: number }>();
	const entry = (label: string) => {
		let row = tally.get(label);
		if (!row) {
			row = { started: 0, failed: 0 };
			tally.set(label, row);
		}
		return row;
	};
	for (const start of report.starts) {
		entry(start[dimension]).started += start.count;
	}
	for (const failure of report.start_failures) {
		entry(failure[dimension]).failed += failure.count;
	}
	return [...tally.entries()]
		.map(([label, { started, failed }]) => ({
			label,
			started,
			failed,
			failureRate: failureRate(started, failed),
		}))
		.sort(
			(a, b) =>
				b.started + b.failed - (a.started + a.failed) ||
				a.label.localeCompare(b.label),
		);
}

/** `failed / (started + failed)`, or zero with nothing attempted. */
export function failureRate(started: number, failed: number): number {
	const attempts = started + failed;
	return attempts === 0 ? 0 : failed / attempts;
}

/** Every rebuffer row's histogram summed into one, every bucket listed. */
export function rebufferHistogram(
	report: Pick<PlaybackTelemetryReport, "rebuffers">,
): { bucket: RebufferBucket; count: number }[] {
	const counts = new Map<string, number>(
		REBUFFER_BUCKETS.map((bucket) => [bucket, 0]),
	);
	for (const row of report.rebuffers) {
		for (const { bucket, count } of row.histogram) {
			counts.set(bucket, (counts.get(bucket) ?? 0) + count);
		}
	}
	return REBUFFER_BUCKETS.map((bucket) => ({
		bucket,
		count: counts.get(bucket) ?? 0,
	}));
}

/** An inclusive range of UTC days, `YYYY-MM-DD`. */
export interface DayRange {
	from: string;
	to: string;
}

/** The `days` UTC days ending on `today`, today included. */
export function lastDays(days: number, today: Date = new Date()): DayRange {
	const to = new Date(
		Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate()),
	);
	const from = new Date(to);
	from.setUTCDate(to.getUTCDate() - (days - 1));
	return { from: isoDay(from), to: isoDay(to) };
}

function isoDay(date: Date): string {
	return date.toISOString().slice(0, 10);
}

/** Playback telemetry for `range`, from the admin endpoint. */
export function usePlaybackTelemetry(range: DayRange) {
	return useQuery({
		queryKey: ["admin", "telemetry", "playback", range.from, range.to],
		queryFn: async () => {
			const { data, error } = await apiClient.GET(
				"/v1/admin/telemetry/playback",
				{
					params: { query: { from: range.from, to: range.to } },
					credentials: "include",
				},
			);
			if (error) throw apiError(error, "Failed to load playback telemetry");
			return data as PlaybackTelemetryReport;
		},
	});
}
