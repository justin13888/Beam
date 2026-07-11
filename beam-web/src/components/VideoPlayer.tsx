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
			onCanPlay={() => {
				if (startTime > 0 && player.current) {
					player.current.currentTime = startTime;
				}
			}}
			onTimeUpdate={() => {
				if (player.current) {
					onProgress?.(player.current.currentTime, player.current.duration);
				}
			}}
			onEnded={() => onEnded?.(player.current?.duration ?? 0)}
			onError={onError}
		>
			<MediaProvider>
				{poster && <Poster className="vds-poster" src={poster} alt={title} />}
			</MediaProvider>
			<DefaultVideoLayout icons={defaultLayoutIcons} />
		</MediaPlayer>
	);
}
