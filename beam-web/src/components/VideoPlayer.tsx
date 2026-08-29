import "@vidstack/react/player/styles/default/theme.css";
import "@vidstack/react/player/styles/default/layouts/video.css";

import {
	MediaPlayer,
	type MediaPlayerInstance,
	MediaProvider,
	type PlayerSrc,
	Poster,
} from "@vidstack/react";
import {
	DefaultVideoLayout,
	defaultLayoutIcons,
} from "@vidstack/react/player/layouts/default";
import { useRef } from "react";

/** The slice of Vidstack's player instance this wrapper's callbacks read. */
export interface PlayerLike {
	currentTime: number;
	duration: number;
}

/**
 * The wrapper's own behaviour, separated from the JSX that wires it up.
 *
 * Vidstack's `<MediaPlayer>` cannot be driven in jsdom -- it needs a real
 * media element -- so inline callbacks on it are untestable by construction.
 * Pulled out here they are ordinary functions over a `{currentTime, duration}`
 * pair, which is all they ever touched.
 */
export function playerHandlers({
	startTime = 0,
	onProgress,
	onEnded,
}: {
	startTime?: number;
	onProgress?: (currentTime: number, duration: number) => void;
	onEnded?: (duration: number) => void;
}) {
	return {
		/** Seek to the resume position once the source can play. A zero or
		 * negative `startTime` means "from the beginning" and must not seek --
		 * assigning `currentTime = 0` on some providers restarts a stream that
		 * had already begun buffering elsewhere. */
		canPlay(player: PlayerLike | null) {
			if (startTime > 0 && player) {
				player.currentTime = startTime;
			}
		},
		/** Report progress. Skipped entirely when there is no player yet. */
		timeUpdate(player: PlayerLike | null) {
			if (player) {
				onProgress?.(player.currentTime, player.duration);
			}
		},
		/** Report completion, defaulting to 0 when the duration never resolved. */
		ended(player: PlayerLike | null) {
			onEnded?.(player?.duration ?? 0);
		},
	};
}

export interface VideoPlayerProps {
	title: string;
	src: string;
	type?: string;
	poster?: string | null;
	/** Playback position (seconds) to seek to once the player can play --
	 * used to resume a previously in-progress file. */
	startTime?: number;
	/** Start playback as soon as the source can play -- used by up-next
	 * auto-advance so the next episode keeps playing without a click. */
	autoPlay?: boolean;
	/** Fired on every `timeupdate` with the player's current position and
	 * (once known) total duration -- used to drive progress beacons. */
	onProgress?: (currentTime: number, duration: number) => void;
	/** Fired when playback reaches the end, with the final duration. */
	onEnded?: (duration: number) => void;
	onError?: () => void;
	className?: string;
}

/** Thin wrapper around Vidstack's `<MediaPlayer>` with the default video
 * layout (keyboard shortcuts, buffering states, fullscreen/PiP) -- see
 * ADR-0005 (player) and F5 in the engineering plan. Never live-transcodes:
 * `src` always points straight at a pre-existing file's direct-play stream
 * endpoint (see ADR-0004). */
export function VideoPlayer({
	title,
	src,
	type = "video/mp4",
	poster,
	startTime = 0,
	autoPlay = false,
	onProgress,
	onEnded,
	onError,
	className,
}: VideoPlayerProps) {
	const player = useRef<MediaPlayerInstance>(null);
	const handlers = playerHandlers({ startTime, onProgress, onEnded });

	return (
		<MediaPlayer
			key={src}
			ref={player}
			title={title}
			// `type` comes straight from the file's detected container format
			// (see `MediaSource.mime_type`), which isn't guaranteed to match
			// Vidstack's known mime-type literals -- it's only a provider-
			// selection hint, so a mismatch just falls back gracefully.
			src={{ src, type } as PlayerSrc}
			autoPlay={autoPlay}
			playsInline
			className={className}
			onCanPlay={() => handlers.canPlay(player.current)}
			onTimeUpdate={() => handlers.timeUpdate(player.current)}
			onEnded={() => handlers.ended(player.current)}
			onError={onError}
		>
			<MediaProvider>
				{poster && <Poster className="vds-poster" src={poster} alt={title} />}
			</MediaProvider>
			<DefaultVideoLayout icons={defaultLayoutIcons} />
		</MediaPlayer>
	);
}
