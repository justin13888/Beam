/**
 * Which of a title's sources this browser can play, and why not the others.
 *
 * The browser counterpart of `beam-client-core::capability` (`playability()`
 * and `select_source()` in `select.rs`): every source is judged, the first
 * playable one in the server's order is chosen, and every unplayable one keeps
 * its reason so the picker can say why it is there and greyed out rather than
 * hiding it. Under direct play an unplayable file is a permanent property of
 * that file, which the viewer -- or the operator who could index a compatible
 * version -- may need to act on (`docs/architecture/components.md`).
 *
 * The browser is asked, not assumed: `HTMLMediaElement.canPlayType` and
 * `MediaSource.isTypeSupported` judge the container and codecs together, and
 * this module only translates a source's probed container and FFmpeg codec
 * names into the MIME types those two take. Where the browser also has
 * `MediaCapabilities.decodingInfo`, its smooth and power-efficient answers
 * rank the playable sources the way the core's hardware-before-software pool
 * does (`decodingQualities`), without rejecting any.
 */
import type { components } from "@/api.gen";

type MediaSource = components["schemas"]["MediaSource"];
type AudioTrack = components["schemas"]["AudioTrack"];

/** Whether the browser can play a MIME type, `codecs` parameter included. */
export type MediaTypeSupport = (type: string) => boolean;

/**
 * The browser's own answer. `canPlayType` is the authority for a `<video
 * src>` direct play; its "maybe" is taken as a yes, since a codec-qualified
 * type the browser cannot decode answers "". `MediaSource.isTypeSupported` is
 * asked as well, because some browsers hedge on `canPlayType` for types their
 * media pipeline does decode.
 */
export function browserTypeSupport(
	video: Pick<HTMLMediaElement, "canPlayType"> = document.createElement(
		"video",
	),
	mse:
		| { isTypeSupported(type: string): boolean }
		| undefined = globalThis.MediaSource,
): MediaTypeSupport {
	return (type) =>
		video.canPlayType(type) !== "" || (mse?.isTypeSupported(type) ?? false);
}

/** Why a source cannot be played here, mirroring the core's `RejectionReason`
 * where the two overlap. */
export type RejectionReason =
	| "disc_structure"
	| "transport_stream"
	| "container_unsupported"
	| "no_video_stream"
	| "video_codec_unsupported"
	| "audio_codec_unsupported";

export type SourceVerdict =
	| {
			playable: true;
			/** The container's MIME type, without codecs, as the browser accepted it. */
			mimeType: string;
			/** The RFC 6381 string the browser accepted for the first video track. */
			videoCodec: string;
			/** The audio track the browser plays (the first it decodes) and its
			 * RFC 6381 string; null for a file with no audio. */
			audio: { track: AudioTrack; codec: string } | null;
	  }
	| { playable: false; reason: RejectionReason; detail: string };

/** RFC 6381 codec strings for FFmpeg's video codec names. A representative
 * profile and level is enough: the browser is being asked whether it has a
 * decoder for the codec at all, which is what the core asks too. The HDR
 * variant is the 10-bit profile an HDR stream needs. */
const VIDEO_CODECS: Record<string, { sdr: string; hdr: string }> = {
	h264: { sdr: "avc1.640033", hdr: "avc1.6E0033" },
	hevc: { sdr: "hvc1.1.6.L153.B0", hdr: "hvc1.2.4.L153.B0" },
	av1: { sdr: "av01.0.13M.08", hdr: "av01.0.13M.10" },
	vp9: { sdr: "vp09.00.51.08", hdr: "vp09.02.51.10" },
	vp8: { sdr: "vp8", hdr: "vp8" },
	mpeg4: { sdr: "mp4v.20.9", hdr: "mp4v.20.9" },
	theora: { sdr: "theora", hdr: "theora" },
};

/** RFC 6381 codec strings for FFmpeg's audio codec names. */
const AUDIO_CODECS: Record<string, string> = {
	aac: "mp4a.40.2",
	mp3: "mp4a.6B",
	opus: "opus",
	vorbis: "vorbis",
	flac: "flac",
	ac3: "ac-3",
	eac3: "ec-3",
	dts: "dtsc",
	truehd: "mlpa",
	alac: "alac",
};

/** The codecs a WebM file may carry. Matroska is WebM's superset and FFmpeg
 * names one demuxer for both (`matroska,webm`), so a Matroska file holding
 * only these is asked about as the WebM it is in every way a browser sees. */
const WEBM_VIDEO = new Set(["vp8", "vp9", "av1"]);
const WEBM_AUDIO = new Set(["opus", "vorbis"]);

type Container =
	| { kind: "transport_stream" }
	| { kind: "unknown"; label: string }
	| { kind: "mp4" | "matroska" | "ogg" | "avi" | "mpeg_ps" };

/**
 * The container a source's file is in. `container_format` is FFmpeg's demuxer
 * name list (`mov,mp4,m4a,3gp,3g2,mj2`, `matroska,webm`, `mpegts`); the
 * `mime_type` is consulted only when that is absent.
 */
function containerOf(source: MediaSource): Container {
	const raw =
		source.container_format ?? source.mime_type?.replace(/^video\//, "") ?? "";
	const names = raw.toLowerCase().split(/[,\s]+/);
	const has = (...candidates: string[]) =>
		candidates.some((c) => names.includes(c));
	if (has("mpegts", "mp2t", "m2ts", "ts")) return { kind: "transport_stream" };
	if (has("mp4", "mov", "quicktime", "m4v")) return { kind: "mp4" };
	if (has("matroska", "webm", "x-matroska")) return { kind: "matroska" };
	if (has("ogg")) return { kind: "ogg" };
	if (has("avi", "x-msvideo")) return { kind: "avi" };
	if (has("mpeg")) return { kind: "mpeg_ps" };
	return { kind: "unknown", label: names[0] || "unknown" };
}

const CONTAINER_LABEL = {
	mp4: "MP4",
	matroska: "MKV",
	ogg: "Ogg",
	avi: "AVI",
	mpeg_ps: "MPEG program stream",
} as const;

/** The MIME type to ask about a container holding these codecs. */
function containerMime(
	kind: keyof typeof CONTAINER_LABEL,
	video: string,
	audio: string | null,
): string {
	switch (kind) {
		case "mp4":
			return "video/mp4";
		case "matroska":
			return WEBM_VIDEO.has(video) && (audio === null || WEBM_AUDIO.has(audio))
				? "video/webm"
				: "video/x-matroska";
		case "ogg":
			return "video/ogg";
		case "avi":
			return "video/x-msvideo";
		case "mpeg_ps":
			return "video/mpeg";
	}
}

function withCodecs(mime: string, codecs: string[]): string {
	return `${mime}; codecs="${codecs.join(", ")}"`;
}

/** FFmpeg's names normalised onto the keys of the tables above. */
function normaliseVideo(codec: string): string {
	const name = codec.trim().toLowerCase();
	if (name === "h265") return "hevc";
	if (name === "libdav1d" || name === "libaom-av1") return "av1";
	return name;
}

function normaliseAudio(codec: string): string {
	const name = codec.trim().toLowerCase();
	if (name === "mp3float" || name === "mp3") return "mp3";
	return name;
}

/** Judge one source against this browser, in the core's order: container,
 * then video, then audio. */
export function judgeSource(
	source: MediaSource,
	canPlay: MediaTypeSupport,
): SourceVerdict {
	if (source.disc_structure) {
		const disc =
			source.disc_structure === "dvd"
				? "a DVD folder rip, whose VOB files"
				: "a Blu-ray folder rip, whose M2TS files";
		return {
			playable: false,
			reason: "disc_structure",
			detail: `This is ${disc} browsers cannot open`,
		};
	}

	const container = containerOf(source);
	if (container.kind === "transport_stream") {
		return {
			playable: false,
			reason: "transport_stream",
			detail: "Browsers cannot open MPEG transport streams (TS, M2TS)",
		};
	}
	if (container.kind === "unknown") {
		return {
			playable: false,
			reason: "container_unsupported",
			detail: `This browser cannot open ${container.label.toUpperCase()} files`,
		};
	}
	const containerUnsupported: SourceVerdict = {
		playable: false,
		reason: "container_unsupported",
		detail: `This browser cannot open ${CONTAINER_LABEL[container.kind]} files`,
	};

	const video = source.video_tracks[0];
	if (!video) {
		return {
			playable: false,
			reason: "no_video_stream",
			detail: "This file has no video stream",
		};
	}
	const videoName = normaliseVideo(video.codec);
	const videoUnsupported: SourceVerdict = {
		playable: false,
		reason: "video_codec_unsupported",
		detail: `This browser cannot decode ${video.codec.toUpperCase()} video`,
	};
	const videoStrings = VIDEO_CODECS[videoName];
	if (!videoStrings) return videoUnsupported;
	const videoCodec = video.hdr_format ? videoStrings.hdr : videoStrings.sdr;

	const videoMime = containerMime(container.kind, videoName, null);
	if (!canPlay(videoMime)) return containerUnsupported;
	if (!canPlay(withCodecs(videoMime, [videoCodec]))) return videoUnsupported;

	if (source.audio_tracks.length === 0) {
		return { playable: true, mimeType: videoMime, videoCodec, audio: null };
	}

	// Playable when any one audio track is: a browser plays the first track it
	// can decode, as the core's `playability()` accepts any decodable track.
	let containerRefused = false;
	for (const track of source.audio_tracks) {
		const audioName = normaliseAudio(track.codec);
		const audioCodec = AUDIO_CODECS[audioName];
		if (!audioCodec) continue;
		const mime = containerMime(container.kind, videoName, audioName);
		if (canPlay(withCodecs(mime, [videoCodec, audioCodec]))) {
			return {
				playable: true,
				mimeType: mime,
				videoCodec,
				audio: { track, codec: audioCodec },
			};
		}
		// The track would move the file out of the container the browser
		// accepted (WebM to Matroska), and the browser does not open that one.
		if (mime !== videoMime && !canPlay(mime)) containerRefused = true;
	}
	if (containerRefused) return containerUnsupported;
	const names = source.audio_tracks.map((t) => t.codec.toUpperCase());
	return {
		playable: false,
		reason: "audio_codec_unsupported",
		detail: `This browser cannot decode this file's audio (${[...new Set(names)].join(", ")})`,
	};
}

/**
 * How well this browser decodes a source it can play, from
 * `MediaCapabilities.decodingInfo`: the browser's nearest analogue to the
 * core's `Playability::Hardware` / `Playability::Software` split. A source
 * that is both smooth and power-efficient is decoded the way the core calls
 * hardware; anything less is the software case. These flags rank sources and
 * never reject one: `canPlayType` already said the file plays.
 */
export interface DecodingQuality {
	smooth: boolean;
	powerEfficient: boolean;
}

/** Whether a quality is the hardware-like case the choice prefers. */
export function decodesEfficiently(quality: DecodingQuality): boolean {
	return quality.smooth && quality.powerEfficient;
}

/** The notice for a pick that decodes in software here, worded as the core's
 * `Playability::Software` detail is. */
export function softwareDecodeDetail(source: MediaSource): string {
	const codec = source.video_tracks[0]?.codec ?? "This file";
	return `${codec.toUpperCase()} decodes in software here and may stutter`;
}

/** The part of `navigator.mediaCapabilities` this module asks. */
export type DecodingCapabilities = Pick<MediaCapabilities, "decodingInfo">;

/**
 * The `decodingInfo` query for a source `judgeSource` found playable, or null
 * where the probe lacks what the query requires. A `file` video
 * configuration requires a bit rate and a frame rate; the bit rate falls back
 * to the file's average (size over duration), but an unknown frame rate is
 * not guessed, because the answer depends on it -- such a source is left
 * unranked rather than ranked on an invented number.
 */
export function decodingConfiguration(
	source: MediaSource,
	verdict: SourceVerdict,
): MediaDecodingConfiguration | null {
	if (!verdict.playable) return null;
	const video = source.video_tracks[0];
	if (!video?.frame_rate) return null;
	const averageBitRate =
		source.duration_secs && source.duration_secs > 0
			? Math.round((source.size_bytes * 8) / source.duration_secs)
			: null;
	const bitrate = video.bit_rate ?? averageBitRate;
	if (!bitrate) return null;

	const configuration: MediaDecodingConfiguration = {
		type: "file",
		video: {
			contentType: withCodecs(verdict.mimeType, [verdict.videoCodec]),
			width: video.width,
			height: video.height,
			bitrate,
			framerate: video.frame_rate,
		},
	};
	if (verdict.audio) {
		const { track, codec } = verdict.audio;
		configuration.audio = {
			// The audio half is asked as an audio type of the same container.
			contentType: withCodecs(verdict.mimeType.replace(/^video\//, "audio/"), [
				codec,
			]),
			...(track.channels ? { channels: String(track.channels) } : {}),
			...(track.bit_rate ? { bitrate: track.bit_rate } : {}),
			...(track.sample_rate ? { samplerate: track.sample_rate } : {}),
		};
	}
	return configuration;
}

/**
 * Ask the browser how well it decodes each playable source. Resolves to the
 * qualities it answered, by `file_id`; a source it was not asked about (not
 * playable, or no usable configuration) or whose query failed is absent, and
 * so unranked. Where `navigator.mediaCapabilities` does not exist (older
 * browsers, jsdom) it resolves to an empty map, which leaves `chooseSource`
 * choosing exactly as it does without one.
 */
export async function decodingQualities(
	sources: readonly MediaSource[],
	verdicts: ReadonlyMap<string, SourceVerdict>,
	capabilities: DecodingCapabilities | undefined = globalThis.navigator
		?.mediaCapabilities,
): Promise<Map<string, DecodingQuality>> {
	const qualities = new Map<string, DecodingQuality>();
	if (!capabilities) return qualities;
	await Promise.all(
		sources.map(async (source) => {
			const verdict = verdicts.get(source.file_id);
			if (!verdict) return;
			const configuration = decodingConfiguration(source, verdict);
			if (!configuration) return;
			try {
				const info = await capabilities.decodingInfo(configuration);
				qualities.set(source.file_id, {
					// A configuration the browser calls unsupported is neither
					// smooth nor efficient, whatever the other flags say; it still
					// plays, since `canPlayType` accepted it.
					smooth: info.supported && info.smooth,
					powerEfficient: info.supported && info.powerEfficient,
				});
			} catch {
				// A configuration the browser refuses to evaluate (a TypeError for
				// a content type it cannot parse) ranks nothing.
			}
		}),
	);
	return qualities;
}

export interface SourceChoice {
	/** The source to play, or null when none plays here. */
	chosen: MediaSource | null;
	/** Every source's verdict, by `file_id`, in the order given. */
	verdicts: Map<string, SourceVerdict>;
	/** The chosen source's decoding quality, where the browser reported one. */
	quality: DecodingQuality | null;
}

/** Whether `fileId` names this source: its first file, or any of its parts. */
export function sourceHoldsFile(source: MediaSource, fileId: string): boolean {
	return (
		source.file_id === fileId || source.parts.some((p) => p.file_id === fileId)
	);
}

/**
 * Choose what to play: the requested source when it plays here, otherwise
 * the first playable one in the order given.
 *
 * The server lists a title's sources best first -- the primary (default
 * edition), then the tallest picture, the highest video bit rate, the largest
 * file -- so the first playable source is the highest-quality one this
 * browser can play, which is `QualityPolicy::Highest` and FR-704. The ranking
 * is not repeated here: the server's is the one every client shares.
 *
 * `requestedFileId` -- a viewer's pick, a deep link, an episode's file -- may
 * name any part of a source. An unplayable request falls back to the best
 * playable source rather than being attempted and failing.
 *
 * `qualities`, from `decodingQualities`, refines the fallback the way the
 * core's hardware-first pool does: the first playable source that decodes
 * smoothly and power-efficiently is preferred to the rest, so a source that
 * merely *can* play does not beat one that plays well. Where none does, or
 * the answers are absent or still pending, the first playable source is
 * chosen as before. A requested source is honoured whatever its quality.
 */
export function chooseSource(
	sources: readonly MediaSource[],
	canPlay: MediaTypeSupport,
	requestedFileId: string | null,
	qualities: ReadonlyMap<string, DecodingQuality> = new Map(),
): SourceChoice {
	const verdicts = new Map<string, SourceVerdict>();
	for (const source of sources) {
		verdicts.set(source.file_id, judgeSource(source, canPlay));
	}
	const playable = sources.filter(
		(s) => verdicts.get(s.file_id)?.playable === true,
	);
	const requested = requestedFileId
		? playable.find((s) => sourceHoldsFile(s, requestedFileId))
		: undefined;
	const efficient = playable.find((s) => {
		const quality = qualities.get(s.file_id);
		return quality !== undefined && decodesEfficiently(quality);
	});
	const chosen = requested ?? efficient ?? playable[0] ?? null;
	return {
		chosen,
		verdicts,
		quality: (chosen && qualities.get(chosen.file_id)) ?? null,
	};
}
