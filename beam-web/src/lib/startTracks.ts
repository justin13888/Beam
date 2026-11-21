import type { components } from "@/api.gen";

type SubtitleTrack = components["schemas"]["SubtitleTrack"];

/** A subtitle file beside the media file, as the player is handed one. */
export interface SidecarTextTrack {
	/** Stable identity: the subtitle file's id. */
	id: string;
	/** Where the player fetches it. */
	src: string;
	/** The format the player parses it as. */
	type: "vtt" | "srt" | "ssa" | "ass";
	/** What the captions menu calls it. */
	label: string;
	/** Its language, where the file declared one. */
	language: string | undefined;
	/** Whether playback starts on it: the server named it for this viewer. */
	isDefault: boolean;
}

/** The stored formats the player parses itself, by FFmpeg codec name. */
const NATIVE_FORMATS: Record<string, SidecarTextTrack["type"]> = {
	webvtt: "vtt",
	subrip: "srt",
	srt: "srt",
	ssa: "ssa",
	ass: "ass",
};

/**
 * The subtitle files beside a part the player can render, each marked default
 * when it is the one the server named for the viewer (issue #329):
 * `defaultIndex` counts every track the server lists, the file's own streams
 * included.
 *
 * A browser cannot reach a subtitle stream inside the file, so a server-named
 * embedded track is not shown here: the part starts without subtitles. A
 * sidecar is loaded as its WebVTT conversion where the server offers one, else
 * as stored where the player parses that format, else it is left out --
 * listing a track that renders nothing would be worse than not listing it.
 */
export function sidecarTextTracks(
	tracks: SubtitleTrack[],
	defaultIndex: number | null | undefined,
	absoluteUrl: (path: string) => string,
): SidecarTextTrack[] {
	return tracks.flatMap((track, index) => {
		if (track.origin !== "sidecar" || !track.sidecar_id) return [];
		const stored = NATIVE_FORMATS[track.codec.toLowerCase()];
		const loaded = track.webvtt_url
			? { src: track.webvtt_url, type: "vtt" as const }
			: track.url && stored
				? { src: track.url, type: stored }
				: null;
		if (!loaded) return [];
		return [
			{
				id: track.sidecar_id,
				src: absoluteUrl(loaded.src),
				type: loaded.type,
				label: subtitleLabel(track),
				language: track.language ?? undefined,
				isDefault: index === defaultIndex,
			},
		];
	});
}

/** A track's own title, else its language's name, with forced and SDH said. */
export function subtitleLabel(track: SubtitleTrack): string {
	const name = track.title?.trim() || languageName(track.language);
	const qualifiers = [
		track.is_forced ? "Forced" : null,
		track.is_hearing_impaired ? "SDH" : null,
	].filter((q): q is string => q !== null);
	if (!name) return qualifiers.join(", ") || "Subtitles";
	return qualifiers.length > 0 ? `${name} (${qualifiers.join(", ")})` : name;
}

/** A language code as a person reads it, or the code where none is known. */
function languageName(code: string | null | undefined): string | null {
	if (!code) return null;
	try {
		const names = new Intl.DisplayNames(undefined, { type: "language" });
		return names.of(code) ?? code;
	} catch {
		return code;
	}
}

/** The slice of `HTMLMediaElement.audioTracks` this reads and writes. */
export interface AudioTrackListLike {
	readonly length: number;
	[index: number]: { enabled: boolean };
}

/**
 * Start on the audio track the server named, where the browser exposes the
 * file's audio tracks (`HTMLMediaElement.audioTracks`, which only some
 * browsers implement). Returns whether it was applied: absent the list, or
 * with an index it does not hold, the browser keeps its own choice.
 */
export function enableAudioTrack(
	tracks: AudioTrackListLike | undefined,
	index: number | null | undefined,
): boolean {
	if (!tracks || index == null || index < 0 || index >= tracks.length) {
		return false;
	}
	for (let i = 0; i < tracks.length; i++) {
		tracks[i].enabled = i === index;
	}
	return true;
}
