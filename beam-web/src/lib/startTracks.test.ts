import { describe, expect, it } from "vitest";
import type { components } from "@/api.gen";
import {
	enableAudioTrack,
	sidecarTextTracks,
	subtitleLabel,
} from "./startTracks";

type SubtitleTrack = components["schemas"]["SubtitleTrack"];

function track(overrides: Partial<SubtitleTrack> = {}): SubtitleTrack {
	return {
		origin: "sidecar",
		index: null,
		sidecar_id: "s1",
		codec: "subrip",
		language: "eng",
		title: "English",
		is_default: false,
		is_forced: false,
		is_hearing_impaired: false,
		is_text: true,
		url: "/v1/files/f/subtitles/s1",
		webvtt_url: null,
		...overrides,
	};
}

const absolute = (path: string) => `https://beam.test${path}`;

describe("sidecarTextTracks", () => {
	it("marks the track the server names, counting the file's own streams", () => {
		const tracks = [
			track({ origin: "embedded", index: 2, sidecar_id: null, url: null }),
			track({ sidecar_id: "s1" }),
			track({ sidecar_id: "s2", url: "/v1/files/f/subtitles/s2" }),
		];

		const loaded = sidecarTextTracks(tracks, 2, absolute);

		expect(loaded.map((t) => [t.id, t.isDefault])).toEqual([
			["s1", false],
			["s2", true],
		]);
	});

	it("marks nothing when the server names no track, or an embedded one", () => {
		const tracks = [
			track({ origin: "embedded", index: 2, sidecar_id: null, url: null }),
			track(),
		];

		expect(sidecarTextTracks(tracks, null, absolute)[0].isDefault).toBe(false);
		expect(sidecarTextTracks(tracks, 0, absolute)[0].isDefault).toBe(false);
	});

	it.each([
		[
			"its WebVTT conversion where the server offers one",
			track({ webvtt_url: "/v1/files/f/subtitles/s1/webvtt" }),
			{ src: "https://beam.test/v1/files/f/subtitles/s1/webvtt", type: "vtt" },
		],
		[
			"a SubRip file as stored",
			track(),
			{ src: "https://beam.test/v1/files/f/subtitles/s1", type: "srt" },
		],
		[
			"an ASS file as stored",
			track({ codec: "ass" }),
			{ src: "https://beam.test/v1/files/f/subtitles/s1", type: "ass" },
		],
	])("loads %s", (_, input, expected) => {
		expect(sidecarTextTracks([input], null, absolute)[0]).toMatchObject(
			expected,
		);
	});

	it("leaves out a file the player cannot render", () => {
		expect(
			sidecarTextTracks(
				[track({ codec: "hdmv_pgs_subtitle", is_text: false })],
				0,
				absolute,
			),
		).toEqual([]);
	});
});

describe("subtitleLabel", () => {
	it("says forced and SDH beside the track's own title", () => {
		expect(
			subtitleLabel(
				track({ title: "Signs", is_forced: true, is_hearing_impaired: true }),
			),
		).toBe("Signs (Forced, SDH)");
		expect(subtitleLabel(track({ title: null, language: null }))).toBe(
			"Subtitles",
		);
		expect(
			subtitleLabel(track({ title: null, language: null, is_forced: true })),
		).toBe("Forced");
	});
});

describe("enableAudioTrack", () => {
	function list(count: number) {
		return Array.from({ length: count }, (_, i) => ({ enabled: i === 0 }));
	}

	it("enables the named track alone", () => {
		const tracks = list(3);

		expect(enableAudioTrack(tracks, 2)).toBe(true);
		expect(tracks.map((t) => t.enabled)).toEqual([false, false, true]);
	});

	it("keeps the browser's choice without the list or with an index it lacks", () => {
		const tracks = list(2);

		expect(enableAudioTrack(undefined, 1)).toBe(false);
		expect(enableAudioTrack(tracks, 2)).toBe(false);
		expect(enableAudioTrack(tracks, null)).toBe(false);
		expect(tracks.map((t) => t.enabled)).toEqual([true, false]);
	});
});
