import { describe, expect, it, vi } from "vitest";
import type { components } from "@/api.gen";
import * as factory from "@/test/factories";
import {
	browserTypeSupport,
	chooseSource,
	judgeSource,
	type MediaTypeSupport,
} from "./playback-capability";

type MediaSource = components["schemas"]["MediaSource"];
type AudioTrack = components["schemas"]["AudioTrack"];

/** A browser that opens `containers` and decodes codec strings starting with
 * any of `codecs`, recording every type it was asked about. */
function browser(containers: string[], codecs: string[]) {
	const asked: string[] = [];
	const canPlay: MediaTypeSupport = (type) => {
		asked.push(type);
		const [mime, params = ""] = type.split(";");
		const listed =
			/codecs="([^"]*)"/
				.exec(params)?.[1]
				.split(",")
				.map((c) => c.trim()) ?? [];
		return (
			containers.includes(mime.trim()) &&
			listed.every((c) => codecs.some((prefix) => c.startsWith(prefix)))
		);
	};
	return { canPlay, asked };
}

const chrome = browser(
	["video/mp4", "video/webm"],
	["avc1", "hvc1", "vp09", "av01", "mp4a", "opus", "flac"],
);

function audio(codec: string): AudioTrack {
	return {
		index: 1,
		codec,
		language: null,
		title: null,
		channels: 2,
		channel_layout: null,
		sample_rate: null,
		bit_rate: null,
		is_default: false,
		is_forced: false,
	};
}

function src(
	fileId: string,
	{
		container = "mov,mp4,m4a,3gp,3g2,mj2",
		video = "h264" as string | null,
		height = 1080,
		hdr = null as string | null,
		audioCodecs = ["aac"],
		...overrides
	}: {
		container?: string | null;
		video?: string | null;
		height?: number;
		hdr?: string | null;
		audioCodecs?: string[];
	} & Partial<MediaSource> = {},
): MediaSource {
	return factory.mediaSource({
		file_id: fileId,
		container_format: container,
		video_tracks: video
			? [
					{
						index: 0,
						codec: video,
						width: height * 2,
						height,
						frame_rate: null,
						bit_rate: null,
						hdr_format: hdr,
					},
				]
			: [],
		audio_tracks: audioCodecs.map(audio),
		...overrides,
	});
}

describe("judgeSource", () => {
	it.each([
		["MP4 H.264/AAC", src("a"), "video/mp4"],
		[
			"Matroska with only WebM codecs, asked about as WebM",
			src("a", {
				container: "matroska,webm",
				video: "vp9",
				audioCodecs: ["opus"],
			}),
			"video/webm",
		],
		["no audio track at all", src("a", { audioCodecs: [] }), "video/mp4"],
		[
			"one decodable track among undecodable ones",
			src("a", { audioCodecs: ["dts", "truehd", "aac"] }),
			"video/mp4",
		],
	])("plays %s", (_, source, mimeType) => {
		expect(judgeSource(source, chrome.canPlay)).toEqual({
			playable: true,
			mimeType,
		});
	});

	it.each([
		[
			"a DVD folder rip, whatever its codecs",
			src("a", { disc_structure: "dvd" }),
			"disc_structure",
			/DVD folder rip/,
		],
		[
			"a Blu-ray folder rip",
			src("a", { disc_structure: "blu_ray" }),
			"disc_structure",
			/Blu-ray folder rip, whose M2TS files/,
		],
		[
			"a transport stream",
			src("a", { container: "mpegts" }),
			"transport_stream",
			/MPEG transport streams/,
		],
		[
			"a container FFmpeg names that no table knows",
			src("a", { container: "flv" }),
			"container_unsupported",
			/cannot open FLV files/,
		],
		[
			"Matroska carrying H.264, which this browser refuses as Matroska",
			src("a", { container: "matroska,webm" }),
			"container_unsupported",
			/cannot open MKV files/,
		],
		[
			"Matroska whose WebM video becomes Matroska with its AAC audio",
			src("a", { container: "matroska,webm", video: "vp9" }),
			"container_unsupported",
			/cannot open MKV files/,
		],
		[
			"a file with no video stream",
			src("a", { video: null }),
			"no_video_stream",
			/no video stream/,
		],
		[
			"a codec with no codec string",
			src("a", { video: "mpeg2video" }),
			"video_codec_unsupported",
			/cannot decode MPEG2VIDEO video/,
		],
		[
			"a codec the browser has no decoder for",
			src("a", { video: "vp8" }),
			"video_codec_unsupported",
			/cannot decode VP8 video/,
		],
		[
			"only undecodable audio",
			src("a", { audioCodecs: ["dts", "truehd", "dts"] }),
			"audio_codec_unsupported",
			/this file's audio \(DTS, TRUEHD\)/,
		],
	])("rejects %s", (_, source, reason, detail) => {
		const verdict = judgeSource(source, chrome.canPlay);
		expect(verdict.playable).toBe(false);
		if (verdict.playable) return;
		expect(verdict.reason).toBe(reason);
		expect(verdict.detail).toMatch(detail);
	});

	it("asks for the 10-bit profile when the stream is HDR", () => {
		const { canPlay, asked } = browser(["video/mp4"], ["hvc1.1", "mp4a"]);
		const verdict = judgeSource(
			src("a", { video: "hevc", hdr: "HDR10" }),
			canPlay,
		);
		expect(verdict).toMatchObject({ reason: "video_codec_unsupported" });
		expect(asked.some((t) => t.includes("hvc1.2"))).toBe(true);
	});

	it("never asks the browser about a disc or a transport stream", () => {
		const { canPlay, asked } = browser(["video/mp4"], ["avc1", "mp4a"]);
		judgeSource(src("a", { disc_structure: "blu_ray" }), canPlay);
		judgeSource(src("b", { container: "mpegts" }), canPlay);
		expect(asked).toEqual([]);
	});

	it("falls back to the MIME type when no container format was probed", () => {
		expect(
			judgeSource(
				src("a", { container: null, mime_type: "video/webm", video: "vp9" }),
				browser(["video/webm"], ["vp09", "mp4a"]).canPlay,
			),
		).toMatchObject({ playable: false, reason: "container_unsupported" });
		expect(
			judgeSource(
				src("a", {
					container: null,
					mime_type: "video/webm",
					video: "vp9",
					audioCodecs: ["opus"],
				}),
				browser(["video/webm"], ["vp09", "opus"]).canPlay,
			),
		).toEqual({ playable: true, mimeType: "video/webm" });
	});
});

describe("chooseSource", () => {
	const sources = [
		src("4k-vp8", { video: "vp8", height: 2160 }),
		src("hd", { height: 1080 }),
		src("sd", { height: 480 }),
	];

	it("chooses the first playable source in the server's order", () => {
		const choice = chooseSource(sources, chrome.canPlay, null);
		expect(choice.chosen?.file_id).toBe("hd");
		expect(choice.verdicts.get("4k-vp8")?.playable).toBe(false);
		expect([...choice.verdicts.keys()]).toEqual(["4k-vp8", "hd", "sd"]);
	});

	it("honours a request for a playable source", () => {
		expect(chooseSource(sources, chrome.canPlay, "sd").chosen?.file_id).toBe(
			"sd",
		);
	});

	it("honours a request naming a later part of a source", () => {
		const parted = src("cd1", {
			parts: [
				{
					file_id: "cd1",
					part_number: 1,
					size_bytes: 1,
					subtitle_tracks: [],
					stream_url: "/v1/files/cd1/stream",
					download_url: "/v1/files/cd1/download",
				},
				{
					file_id: "cd2",
					part_number: 2,
					size_bytes: 1,
					subtitle_tracks: [],
					stream_url: "/v1/files/cd2/stream",
					download_url: "/v1/files/cd2/download",
				},
			],
		});
		expect(
			chooseSource([...sources, parted], chrome.canPlay, "cd2").chosen?.file_id,
		).toBe("cd1");
	});

	it.each([
		["an unplayable source", "4k-vp8"],
		["a file no source holds", "gone"],
	])("falls back to the best playable source for %s", (_, requested) => {
		expect(
			chooseSource(sources, chrome.canPlay, requested).chosen?.file_id,
		).toBe("hd");
	});

	it("chooses nothing when nothing plays, keeping every reason", () => {
		const discs = [
			src("dvd", { disc_structure: "dvd" }),
			src("bd", { disc_structure: "blu_ray" }),
		];
		const choice = chooseSource(discs, chrome.canPlay, null);
		expect(choice.chosen).toBeNull();
		expect(choice.verdicts.size).toBe(2);
	});
});

describe("browserTypeSupport", () => {
	const type = 'video/mp4; codecs="avc1.640033"';

	it.each([
		["probably", false, true],
		["maybe", false, true],
		["", true, true],
		["", false, false],
	] as const)(
		"canPlayType %j with MediaSource answering %s plays: %s",
		(answer, mseAnswer, expected) => {
			const video = { canPlayType: vi.fn(() => answer) };
			const mse = { isTypeSupported: vi.fn(() => mseAnswer) };
			expect(browserTypeSupport(video, mse)(type)).toBe(expected);
			expect(video.canPlayType).toHaveBeenCalledWith(type);
		},
	);

	it("relies on canPlayType alone where MediaSource does not exist", () => {
		const video = { canPlayType: () => "" as const };
		expect(browserTypeSupport(video, undefined)(type)).toBe(false);
	});
});
