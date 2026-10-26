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
 * names into the MIME types those two take.
 */
import type { components } from "@/api.gen";

type MediaSource = components["schemas"]["MediaSource"];

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
		return { playable: true, mimeType: videoMime };
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
			return { playable: true, mimeType: mime };
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

export interface SourceChoice {
	/** The source to play, or null when none plays here. */
	chosen: MediaSource | null;
	/** Every source's verdict, by `file_id`, in the order given. */
	verdicts: Map<string, SourceVerdict>;
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
 */
export function chooseSource(
	sources: readonly MediaSource[],
	canPlay: MediaTypeSupport,
	requestedFileId: string | null,
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
	return { chosen: requested ?? playable[0] ?? null, verdicts };
}
