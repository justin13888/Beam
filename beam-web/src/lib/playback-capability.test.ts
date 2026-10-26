import { describe, expect, it, vi } from "vitest";
import type { components } from "@/api.gen";
import * as factory from "@/test/factories";
import {
	browserTypeSupport,
	chooseSource,
	type DecodingCapabilities,
	type DecodingQuality,
	decodingConfiguration,
	decodingQualities,
	judgeSource,
	type MediaTypeSupport,
	softwareDecodeDetail,
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
		frameRate = null as number | null,
		bitRate = null as number | null,
		...overrides
	}: {
		container?: string | null;
		video?: string | null;
		height?: number;
		hdr?: string | null;
		audioCodecs?: string[];
		frameRate?: number | null;
		bitRate?: number | null;
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
						frame_rate: frameRate,
						bit_rate: bitRate,
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
		expect(judgeSource(source, chrome.canPlay)).toMatchObject({
			playable: true,
			mimeType,
		});
	});

	it("names the codec strings and the audio track the browser accepted", () => {
		const verdict = judgeSource(
			src("a", { audioCodecs: ["dts", "aac"] }),
			chrome.canPlay,
		);
		expect(verdict).toMatchObject({
			playable: true,
			videoCodec: "avc1.640033",
			audio: { codec: "mp4a.40.2", track: { codec: "aac" } },
		});
		expect(
			judgeSource(src("b", { audioCodecs: [] }), chrome.canPlay),
		).toMatchObject({ playable: true, audio: null });
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
		).toMatchObject({ playable: true, mimeType: "video/webm" });
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

const efficient: DecodingQuality = { smooth: true, powerEfficient: true };
const software: DecodingQuality = { smooth: false, powerEfficient: false };

describe("chooseSource with decoding qualities", () => {
	const sources = [
		src("4k", { height: 2160 }),
		src("hd", { height: 1080 }),
		src("sd", { height: 480 }),
	];

	it("prefers the first source that decodes smoothly and efficiently", () => {
		const choice = chooseSource(
			sources,
			chrome.canPlay,
			null,
			new Map([
				["4k", software],
				["hd", efficient],
				["sd", efficient],
			]),
		);
		expect(choice.chosen?.file_id).toBe("hd");
		expect(choice.quality).toEqual(efficient);
	});

	it.each([
		["smooth but not power-efficient", { smooth: true, powerEfficient: false }],
		["power-efficient but not smooth", { smooth: false, powerEfficient: true }],
	])("does not prefer a source that is only %s", (_, partial) => {
		const choice = chooseSource(
			sources,
			chrome.canPlay,
			null,
			new Map([
				["4k", partial],
				["hd", efficient],
			]),
		);
		expect(choice.chosen?.file_id).toBe("hd");
	});

	it("keeps the server's first playable source when none decodes efficiently", () => {
		const choice = chooseSource(
			sources,
			chrome.canPlay,
			null,
			new Map([
				["4k", software],
				["hd", software],
			]),
		);
		expect(choice.chosen?.file_id).toBe("4k");
		expect(choice.quality).toEqual(software);
	});

	it("rejects nothing on these flags: every verdict stays playable", () => {
		const choice = chooseSource(
			sources,
			chrome.canPlay,
			null,
			new Map(sources.map((s) => [s.file_id, software])),
		);
		for (const verdict of choice.verdicts.values()) {
			expect(verdict.playable).toBe(true);
		}
	});

	it("never prefers an efficient source the browser cannot play", () => {
		const choice = chooseSource(
			[src("vp8", { video: "vp8" }), ...sources],
			chrome.canPlay,
			null,
			new Map([
				["vp8", efficient],
				["4k", software],
				["hd", efficient],
			]),
		);
		expect(choice.chosen?.file_id).toBe("hd");
	});

	it("honours a requested source whatever its quality", () => {
		const choice = chooseSource(
			sources,
			chrome.canPlay,
			"4k",
			new Map([
				["4k", software],
				["hd", efficient],
			]),
		);
		expect(choice.chosen?.file_id).toBe("4k");
		expect(choice.quality).toEqual(software);
	});

	it("reports no quality for a source the browser was not asked about", () => {
		expect(
			chooseSource(sources, chrome.canPlay, null, new Map()).quality,
		).toBeNull();
	});
});

describe("decodingConfiguration", () => {
	function playable(source: MediaSource) {
		return judgeSource(source, chrome.canPlay);
	}

	it("asks about the accepted video and audio types with the probed figures", () => {
		const source = src("a", {
			height: 1080,
			frameRate: 23.976,
			bitRate: 8_000_000,
			audio_tracks: [
				{
					...audio("aac"),
					channels: 6,
					bit_rate: 384_000,
					sample_rate: 48_000,
				},
			],
		});
		expect(decodingConfiguration(source, playable(source))).toEqual({
			type: "file",
			video: {
				contentType: 'video/mp4; codecs="avc1.640033"',
				width: 2160,
				height: 1080,
				bitrate: 8_000_000,
				framerate: 23.976,
			},
			audio: {
				contentType: 'audio/mp4; codecs="mp4a.40.2"',
				channels: "6",
				bitrate: 384_000,
				samplerate: 48_000,
			},
		});
	});

	it("falls back to the file's average bit rate, and omits unknown audio figures", () => {
		const source = src("a", {
			frameRate: 25,
			size_bytes: 1_000_000,
			duration_secs: 8,
			audio_tracks: [{ ...audio("opus"), channels: 0 }],
			container_format: "matroska,webm",
			video_tracks: [
				{
					index: 0,
					codec: "vp9",
					width: 1920,
					height: 1080,
					frame_rate: 25,
					bit_rate: null,
					hdr_format: null,
				},
			],
		});
		const configuration = decodingConfiguration(source, playable(source));
		expect(configuration?.video?.bitrate).toBe(1_000_000);
		expect(configuration?.audio).toEqual({
			contentType: 'audio/webm; codecs="opus"',
		});
	});

	it("leaves out the audio half for a file without audio", () => {
		const source = src("a", { frameRate: 24, bitRate: 1, audioCodecs: [] });
		const configuration = decodingConfiguration(source, playable(source));
		expect(configuration?.video).toBeDefined();
		expect(configuration?.audio).toBeUndefined();
	});

	it.each([
		["an unknown frame rate", src("a", { bitRate: 1_000_000 })],
		[
			"no bit rate and no duration to average over",
			src("a", { frameRate: 24, duration_secs: null }),
		],
		[
			"no bit rate and a zero duration",
			src("a", { frameRate: 24, duration_secs: 0 }),
		],
	])("asks nothing for %s", (_, source) => {
		expect(decodingConfiguration(source, playable(source))).toBeNull();
	});

	it("asks nothing for an unplayable source", () => {
		const source = src("a", { video: "vp8", frameRate: 24, bitRate: 1 });
		expect(decodingConfiguration(source, playable(source))).toBeNull();
	});
});

describe("decodingQualities", () => {
	const ranked = (fileId: string, height = 1080, video = "h264") =>
		src(fileId, { video, height, frameRate: 24, bitRate: 5_000_000 });

	/** A browser that decodes pictures up to `maxHeight` in hardware. */
	function hardwareUpTo(maxHeight: number) {
		return vi.fn(async (c: MediaDecodingConfiguration) => {
			const hardware = (c.video?.height ?? 0) <= maxHeight;
			return {
				supported: true,
				smooth: hardware,
				powerEfficient: hardware,
				keySystemAccess: null,
			} as MediaCapabilitiesDecodingInfo;
		});
	}

	function verdictsOf(sources: MediaSource[]) {
		return chooseSource(sources, chrome.canPlay, null).verdicts;
	}

	it("resolves to nothing where the browser has no MediaCapabilities", async () => {
		const sources = [ranked("a")];
		expect(
			(await decodingQualities(sources, verdictsOf(sources), undefined)).size,
		).toBe(0);
	});

	it("records each answer, and only for playable sources it could ask about", async () => {
		const sources = [
			ranked("hw", 1080),
			ranked("sw", 2160),
			ranked("vp8", 480, "vp8"),
			src("unprobed"),
		];
		const decodingInfo = hardwareUpTo(1080);
		const qualities = await decodingQualities(sources, verdictsOf(sources), {
			decodingInfo,
		});
		expect(decodingInfo).toHaveBeenCalledTimes(2);
		expect([...qualities.keys()].sort()).toEqual(["hw", "sw"]);
		expect(qualities.get("hw")).toEqual(efficient);
		expect(qualities.get("sw")).toEqual(software);
	});

	it("counts a configuration the browser calls unsupported as neither smooth nor efficient", async () => {
		const sources = [ranked("a")];
		const capabilities: DecodingCapabilities = {
			decodingInfo: async () =>
				({
					supported: false,
					smooth: true,
					powerEfficient: true,
					keySystemAccess: null,
				}) as MediaCapabilitiesDecodingInfo,
		};
		const qualities = await decodingQualities(
			sources,
			verdictsOf(sources),
			capabilities,
		);
		expect(qualities.get("a")).toEqual(software);
	});

	it("leaves a source whose query fails unranked, without failing the rest", async () => {
		const sources = [ranked("bad", 2160), ranked("good", 1080)];
		const answer = hardwareUpTo(2160);
		const capabilities: DecodingCapabilities = {
			decodingInfo: async (c) => {
				if (c.video?.height === 2160) {
					throw new TypeError("unparseable content type");
				}
				return answer(c);
			},
		};
		const qualities = await decodingQualities(
			sources,
			verdictsOf(sources),
			capabilities,
		);
		expect([...qualities.keys()]).toEqual(["good"]);
	});
});

describe("softwareDecodeDetail", () => {
	it("names the video codec, as the core's Software detail does", () => {
		expect(softwareDecodeDetail(src("a", { video: "hevc" }))).toBe(
			"HEVC decodes in software here and may stutter",
		);
	});
});
