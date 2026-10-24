import { describe, expect, it } from "vitest";
import {
	failureRate,
	failureRates,
	lastDays,
	type PlaybackTelemetryReport,
	REBUFFER_BUCKETS,
	rebufferHistogram,
} from "./usePlaybackTelemetry";

type Start = PlaybackTelemetryReport["starts"][number];
type Failure = PlaybackTelemetryReport["start_failures"][number];
type Rebuffer = PlaybackTelemetryReport["rebuffers"][number];

function start(overrides: Partial<Start>): Start {
	return {
		client_kind: "android",
		container: "mp4",
		video_codec: "h264",
		audio_codec: "aac",
		height_class: "hd",
		count: 1,
		...overrides,
	} as Start;
}

function failure(overrides: Partial<Failure>): Failure {
	return {
		client_kind: "android",
		reason: "video_codec",
		stage: "preflight",
		container: "mp4",
		video_codec: "h264",
		audio_codec: "aac",
		height_class: "hd",
		count: 1,
		...overrides,
	} as Failure;
}

describe("failureRates", () => {
	it("joins starts and failures on the dimension asked for", () => {
		const rows = failureRates(
			{
				starts: [
					start({ video_codec: "hevc", count: 3 }),
					start({ video_codec: "h264", count: 5 }),
				],
				start_failures: [failure({ video_codec: "hevc", count: 1 })],
			},
			"video_codec",
		);

		expect(rows.map((row) => row.label)).toEqual(["h264", "hevc"]);
		expect(rows[1]).toEqual({
			label: "hevc",
			started: 3,
			failed: 1,
			failureRate: 0.25,
		});
	});

	it("keeps a value that only ever failed", () => {
		const rows = failureRates(
			{
				starts: [start({ audio_codec: "aac", count: 2 })],
				start_failures: [failure({ audio_codec: "truehd", count: 4 })],
			},
			"audio_codec",
		);

		expect(rows.find((row) => row.label === "truehd")).toMatchObject({
			started: 0,
			failed: 4,
			failureRate: 1,
		});
	});

	it("reads clients by their kind", () => {
		const rows = failureRates(
			{
				starts: [start({ client_kind: "android_tv" })],
				start_failures: [failure({ client_kind: "web_firefox" })],
			},
			"client_kind",
		);

		expect(rows.map((row) => row.label).sort()).toEqual([
			"android_tv",
			"web_firefox",
		]);
	});

	it("treats nothing attempted as no failures rather than a division by zero", () => {
		expect(failureRate(0, 0)).toBe(0);
	});
});

describe("rebufferHistogram", () => {
	it("sums every row and lists every bucket, shortest first", () => {
		const row = (short: number, long: number) =>
			({
				client_kind: "android",
				container: "mp4",
				video_codec: "h264",
				height_class: "hd",
				bitrate_class: "from_2_to_8_mbps",
				event_count: short + long,
				total_ms: 0,
				histogram: [
					{ bucket: "under_1_secs", count: short },
					{ bucket: "at_least_30_secs", count: long },
				],
			}) as Rebuffer;

		const histogram = rebufferHistogram({ rebuffers: [row(2, 1), row(4, 0)] });

		expect(histogram.map((entry) => entry.bucket)).toEqual([
			...REBUFFER_BUCKETS,
		]);
		expect(histogram[0].count).toBe(6);
		expect(histogram[4].count).toBe(1);
		expect(histogram[2].count).toBe(0);
	});
});

describe("lastDays", () => {
	it("ends on today's UTC date and includes it", () => {
		// 23:30 at UTC-5 is already the next day in UTC.
		const today = new Date("2026-10-23T04:30:00Z");

		expect(lastDays(7, today)).toEqual({
			from: "2026-10-17",
			to: "2026-10-23",
		});
		expect(lastDays(1, today)).toEqual({
			from: "2026-10-23",
			to: "2026-10-23",
		});
	});

	it("crosses a month boundary", () => {
		expect(lastDays(30, new Date("2026-03-01T12:00:00Z"))).toEqual({
			from: "2026-01-31",
			to: "2026-03-01",
		});
	});
});
