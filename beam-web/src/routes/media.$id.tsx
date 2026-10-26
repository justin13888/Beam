import { queryOptions, useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Download, RefreshCw } from "lucide-react";
import { useMemo, useState } from "react";
import { toast } from "sonner";
import type { components } from "@/api.gen";
import { RouteError } from "@/components/RouteError";
import { VideoPlayer } from "@/components/VideoPlayer";
import { env } from "@/env";
import { useAuth } from "@/hooks/auth";
import { usePlaybackBeacon } from "@/hooks/usePlaybackBeacon";
import { apiClient } from "@/lib/apiClient";
import { artworkSrc } from "@/lib/artwork";
import {
	browserTypeSupport,
	chooseSource,
	decodesEfficiently,
	decodingQualities,
	type SourceVerdict,
	softwareDecodeDetail,
} from "@/lib/playback-capability";
import { apiError } from "@/lib/problem";
import { nextPlayableEpisode } from "@/lib/upNext";
import { formatDuration } from "@/lib/utils";

type MediaMetadata = components["schemas"]["MediaMetadata"];
type ShowMetadata = components["schemas"]["ShowMetadata"];
type EpisodeMetadata = components["schemas"]["EpisodeMetadata"];
type MediaSource = components["schemas"]["MediaSource"];
type SourcePart = components["schemas"]["SourcePart"];

const mediaQueryOptions = (mediaId: string) =>
	queryOptions({
		queryKey: ["media", mediaId],
		queryFn: async (): Promise<MediaMetadata | null> => {
			const { data, error, response } = await apiClient.GET("/v1/media/{id}", {
				params: { path: { id: mediaId } },
				credentials: "include",
			});
			if (response.status === 404) return null;
			if (error) throw apiError(error, "Failed to load media metadata");
			return data ?? null;
		},
	});

export const Route = createFileRoute("/media/$id")({
	validateSearch: (search: Record<string, unknown>): { fileId?: string } => ({
		fileId: typeof search.fileId === "string" ? search.fileId : undefined,
	}),
	loader: async ({ context: { queryClient }, params: { id } }) => {
		return queryClient.ensureQueryData(mediaQueryOptions(id));
	},
	// The app's own error surface, same as every other route -- TanStack's
	// `ErrorComponent` renders a bare stack trace and offers no way back.
	errorComponent: RouteError,
	component: RouteComponent,
});

/** A short human label for a quality/edition option in the source picker. */
function sourceLabel(source: MediaSource): string {
	const video = source.video_tracks[0];
	if (video) {
		return `${video.height}p · ${video.codec}`;
	}
	return "Original";
}

function absoluteUrl(path: string): string {
	return path.startsWith("http") ? path : `${env.C_STREAM_SERVER_URL}${path}`;
}

function RouteComponent() {
	const metadata = Route.useLoaderData();
	const { fileId: fileIdParam } = Route.useSearch();
	return <MediaDetailPage metadata={metadata} fileIdParam={fileIdParam} />;
}

/** Exported for component tests (see `media.$id.test.tsx`). The route reads
 * the loader data and the `?fileId=` deep-link search param and hands them in;
 * the page itself is framework-agnostic. */
export function MediaDetailPage({
	metadata,
	fileIdParam,
}: {
	metadata: MediaMetadata | null;
	fileIdParam: string | undefined;
}) {
	const { user } = useAuth();
	const movie = metadata && "Movie" in metadata ? metadata.Movie : null;
	const show = metadata && "Show" in metadata ? metadata.Show : null;
	const mediaId = movie?.id ?? show?.id ?? null;

	// Admin-only: force this title's enrichment (poster/genres/ratings) to be
	// re-fetched on the next worker sweep -- surfaces
	// `POST /v1/admin/media/{id}/refresh` (E3) in the UI.
	const refreshMetadataMutation = useMutation({
		mutationFn: async () => {
			if (!mediaId) return;
			const { error, response } = await apiClient.POST(
				"/v1/admin/media/{id}/refresh",
				{
					params: { path: { id: mediaId } },
					credentials: "include",
				},
			);
			if (error || !response.ok) {
				throw apiError(error, "Failed to request a metadata refresh");
			}
		},
		onSuccess: () => {
			toast.success(
				"Metadata refresh queued -- it will be re-fetched on the next enrichment sweep",
			);
		},
		onError: (error) => {
			// The thrown ApiError already carries the server's explanation and
			// its fallback; re-stating a fixed string here discarded it.
			toast.error(error.message);
		},
	});

	// The file the viewer asked for: a pick in the source picker, an episode's
	// file, or a `fileId` search param (set by continue-watching links, and
	// possibly naming a later part of a source). Absent, the best source this
	// browser can play is chosen; present but unplayable here, the same.
	const [selectedFileId, setSelectedFileId] = useState<string | null>(
		fileIdParam ?? null,
	);
	// The part of a multi-part source being played, once playback has moved
	// on from the part it started at; null until then.
	const [partOverride, setPartOverride] = useState<number | null>(null);
	const [selectedEpisodeId, setSelectedEpisodeId] = useState<string | null>(
		null,
	);
	const [playbackError, setPlaybackError] = useState<string | null>(null);
	const [startOver, setStartOver] = useState(false);
	// Set only by up-next auto-advance, so the next episode starts playing
	// without a click; any manual selection clears it again.
	const [autoPlayNext, setAutoPlayNext] = useState(false);

	// Any playable id -- the movie itself, or the selected episode (episode
	// ids are playable ids on `/v1/media/{id}/sources`, same as movie ids).
	// Media can have multiple playable editions/qualities (ADR-0004: never
	// live-transcode -- pick among pre-existing files instead).
	const playableId = movie ? mediaId : selectedEpisodeId;
	const {
		data: sources,
		error: sourcesError,
		refetch: refetchSources,
		isFetching: sourcesFetching,
	} = useQuery({
		queryKey: ["media", playableId, "sources"],
		queryFn: async (): Promise<MediaSource[]> => {
			if (!playableId) return [];
			const { data, error, response } = await apiClient.GET(
				"/v1/media/{id}/sources",
				{
					params: { path: { id: playableId } },
					credentials: "include",
				},
			);
			if (response.status === 404 || response.status === 400) return [];
			if (error) throw apiError(error, "Failed to load media sources");
			return data.items;
		},
		enabled: !!playableId,
	});

	const canPlay = useMemo(() => browserTypeSupport(), []);
	// How smoothly and power-efficiently this browser decodes each playable
	// source. The answer is asynchronous, so the choice is made from the
	// synchronous verdicts first and refined once it arrives; where
	// `navigator.mediaCapabilities` does not exist it is an empty map.
	const verdicts = useMemo(
		() => chooseSource(sources ?? [], canPlay, null).verdicts,
		[sources, canPlay],
	);
	const { data: qualities } = useQuery({
		queryKey: [
			"media",
			playableId,
			"decoding-quality",
			sources?.map((s) => s.file_id),
		],
		queryFn: () => decodingQualities(sources ?? [], verdicts),
		enabled: !!sources && sources.length > 0,
		// A property of this browser and these files, which does not change
		// while the page is open.
		staleTime: Number.POSITIVE_INFINITY,
	});
	// The source playback began in. Once playback has begun, a refined
	// choice must not swap the file out from under the viewer and restart it,
	// so the playing source is held until the viewer picks another file.
	const [playingSourceId, setPlayingSourceId] = useState<string | null>(null);
	const choice = useMemo(
		() =>
			chooseSource(
				sources ?? [],
				canPlay,
				playingSourceId ?? selectedFileId,
				qualities,
			),
		[sources, canPlay, selectedFileId, playingSourceId, qualities],
	);
	const selectedSource = choice.chosen;
	// Sources came back and not one of them plays in this browser.
	const nothingPlayable =
		!!sources && sources.length > 0 && selectedSource === null;
	// `parts` is never empty on the wire; a source without them is its own
	// single part.
	const parts: SourcePart[] = selectedSource
		? selectedSource.parts.length > 0
			? selectedSource.parts
			: [
					{
						file_id: selectedSource.file_id,
						size_bytes: selectedSource.size_bytes,
						duration_secs: selectedSource.duration_secs,
						subtitle_tracks: selectedSource.subtitle_tracks,
						stream_url: selectedSource.stream_url,
						download_url: selectedSource.download_url,
					},
				]
		: [];

	const { data: continueWatching } = useQuery({
		queryKey: ["continue-watching"],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/continue-watching", {
				// The connection's maximum page: the list is the viewer's in-progress
				// titles, and this looks up the one being played.
				params: { query: { first: 50 } },
				credentials: "include",
			});
			if (error) throw apiError(error, "Failed to load playback progress");
			return data.items;
		},
	});

	// A show deep-linked by `?fileId=` has no episode picked, so no sources to
	// judge: that file is played as it is, the one case nothing is chosen for.
	const unjudgedFileId = playableId ? null : selectedFileId;
	// Watch state is per title, with the part last played (streaming.md), so
	// a multi-part movie resumes the part that was playing.
	const savedProgress =
		continueWatching?.find(
			(item) =>
				item.file_id === unjudgedFileId ||
				parts.some((part) => part.file_id === item.file_id),
		) ?? null;
	// The part to start in: the one the saved progress names; failing that
	// (no saved progress for this title, or the lookup missed it), the part
	// the viewer asked for -- a `?fileId=` deep link may name a later part;
	// failing both, the first.
	const savedPartIndex = savedProgress
		? parts.findIndex((part) => part.file_id === savedProgress.file_id)
		: -1;
	const requestedPartIndex = parts.findIndex(
		(part) => part.file_id === selectedFileId,
	);
	const resumePartIndex = startOver
		? 0
		: savedPartIndex >= 0
			? savedPartIndex
			: Math.max(0, requestedPartIndex);
	const partIndex = partOverride ?? resumePartIndex;
	const currentPart = parts[partIndex] ?? null;
	// The saved position is within the part it was saved for; a part reached
	// by advancing starts from its beginning.
	const resumePosition =
		partOverride === null && !startOver && savedProgress
			? savedProgress.position_secs
			: 0;
	const playingFileId = currentPart?.file_id ?? unjudgedFileId;

	const { report, reset: resetBeacon } = usePlaybackBeacon(playingFileId);

	if (!metadata || (!movie && !show)) {
		return (
			<div className="container mx-auto p-4">
				<p>Media not found.</p>
			</div>
		);
	}

	const media = movie ?? show;
	if (!media) {
		return (
			<div className="container mx-auto p-4">
				<p>Media not found.</p>
			</div>
		);
	}
	const title = media.title.original;
	const year = media.year;
	const description = media.description ?? null;
	const posterUrl = artworkSrc(
		movie ? movie.poster_url : (show?.seasons[0]?.poster_url ?? null),
	);

	const streamUrl = currentPart
		? absoluteUrl(currentPart.stream_url)
		: unjudgedFileId
			? `${env.C_STREAM_SERVER_URL}/v1/files/${unjudgedFileId}/stream`
			: null;
	const downloadUrl = currentPart
		? absoluteUrl(currentPart.download_url)
		: unjudgedFileId
			? `${env.C_STREAM_SERVER_URL}/v1/files/${unjudgedFileId}/download`
			: null;
	const selectedVerdict = selectedSource
		? choice.verdicts.get(selectedSource.file_id)
		: undefined;
	// The pick plays, but this browser decodes it in software: the core's
	// `Playability::Software` case, which is said rather than hidden.
	// A quality is reported only for a source with a first video track (the
	// decoding query is built from it), so `selectedVideo` is present whenever
	// `choice.quality` is.
	const selectedVideo = selectedSource?.video_tracks[0];
	const softwareDecodeNotice =
		selectedVideo && choice.quality && !decodesEfficiently(choice.quality)
			? softwareDecodeDetail(selectedVideo.codec)
			: null;
	const hasNextPart = partIndex < parts.length - 1;

	// The episode to auto-advance to when the current one ends (null for
	// movies, for the last playable episode, and before an episode is picked).
	const upNextEpisode = show
		? nextPlayableEpisode(show.seasons, selectedEpisodeId)
		: null;

	function selectFile(fileId: string) {
		setAutoPlayNext(false);
		setPlaybackError(null);
		setStartOver(false);
		setPartOverride(null);
		resetBeacon();
		setPlayingSourceId(null);
		setSelectedFileId(fileId);
	}

	function selectEpisode(episode: EpisodeMetadata) {
		if (!episode.file_id) return;
		setAutoPlayNext(false);
		setPlaybackError(null);
		setStartOver(false);
		setPartOverride(null);
		resetBeacon();
		setPlayingSourceId(null);
		setSelectedEpisodeId(episode.id);
		setSelectedFileId(episode.file_id);
	}

	function handleEnded(duration: number) {
		// A multi-part source plays its parts as a playlist (issue #233): the
		// end of any part but the last moves on to the next one. Nothing is
		// reported for the part that ended -- only the last part can mark the
		// title played, and the next part's first report records the position.
		if (hasNextPart) {
			setAutoPlayNext(true);
			setPlaybackError(null);
			resetBeacon();
			setPartOverride(partIndex + 1);
			return;
		}
		report(duration, duration, true);
		// Up-next auto-advance: keep playing the next playable episode
		// instead of stopping at the end of the current one.
		if (upNextEpisode?.file_id) {
			setAutoPlayNext(true);
			setPlaybackError(null);
			setStartOver(false);
			setPartOverride(null);
			resetBeacon();
			setPlayingSourceId(null);
			setSelectedEpisodeId(upNextEpisode.id);
			setSelectedFileId(upNextEpisode.file_id);
		}
	}

	return (
		<div className="container mx-auto p-4">
			<div className="mb-6 flex gap-6">
				{posterUrl && (
					<img
						src={posterUrl}
						alt={title}
						className="w-48 rounded-md object-cover"
					/>
				)}
				<div className="flex-1">
					<div className="flex items-start justify-between gap-4">
						<h1 className="text-3xl font-bold">{title}</h1>
						{user?.is_admin && (
							<button
								type="button"
								onClick={() => refreshMetadataMutation.mutate()}
								disabled={refreshMetadataMutation.isPending}
								title="Re-fetch this title's metadata (poster, genres, ratings) on the next enrichment sweep"
								className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-gray-700 px-2.5 py-1 text-xs text-gray-400 hover:bg-gray-800 hover:text-white disabled:opacity-50"
							>
								<RefreshCw
									size={12}
									className={
										refreshMetadataMutation.isPending ? "animate-spin" : ""
									}
								/>
								{refreshMetadataMutation.isSuccess
									? "Refresh queued"
									: "Refresh metadata"}
							</button>
						)}
					</div>
					{year && <p className="mt-1 text-gray-400">{year}</p>}
					{description && <p className="mt-4">{description}</p>}
				</div>
			</div>

			{playbackError && <p className="mb-4 text-red-500">{playbackError}</p>}

			{savedProgress && !startOver && (
				<div className="mb-4 flex items-center gap-3 rounded-md bg-gray-800 px-4 py-2 text-sm text-gray-300">
					<span>
						Resuming from {formatDuration(savedProgress.position_secs)}
					</span>
					<button
						type="button"
						className="text-cyan-400 hover:underline"
						onClick={() => {
							setStartOver(true);
							setPartOverride(null);
							resetBeacon();
						}}
					>
						Start over
					</button>
				</div>
			)}

			{streamUrl ? (
				<>
					<VideoPlayer
						key={`${streamUrl}-${startOver}`}
						title={title}
						src={streamUrl}
						type={
							selectedVerdict?.playable ? selectedVerdict.mimeType : undefined
						}
						poster={posterUrl}
						startTime={resumePosition}
						autoPlay={autoPlayNext}
						onProgress={(currentTime, duration) => {
							if (playingSourceId === null && selectedSource) {
								setPlayingSourceId(selectedSource.file_id);
							}
							report(currentTime, duration);
						}}
						onEnded={handleEnded}
						onError={() =>
							setPlaybackError(
								"Failed to load video. Your session may have expired -- try refreshing the page.",
							)
						}
						className="mb-4 w-full max-w-4xl"
					/>

					{softwareDecodeNotice && (
						<p className="mb-4 text-sm text-amber-400">
							{softwareDecodeNotice}
						</p>
					)}

					<div className="mb-6 flex flex-wrap items-center gap-3">
						{sources && sources.length > 1 && (
							<select
								value={selectedSource?.file_id ?? ""}
								onChange={(e) => selectFile(e.target.value)}
								className="rounded-md border border-gray-700 bg-gray-800 px-3 py-1.5 text-sm text-white"
							>
								{sources.map((s) => {
									// An unplayable source is listed with its reason, not
									// hidden: it is a property of the file worth knowing.
									const verdict = choice.verdicts.get(s.file_id);
									const unplayable =
										verdict && !verdict.playable ? verdict : null;
									const quality = qualities?.get(s.file_id);
									const software =
										quality !== undefined && !decodesEfficiently(quality);
									return (
										<option
											key={s.file_id}
											value={s.file_id}
											disabled={!!unplayable}
										>
											{unplayable
												? `${sourceLabel(s)} — ${unplayable.detail}`
												: software
													? `${sourceLabel(s)} — decodes in software here`
													: sourceLabel(s)}
										</option>
									);
								})}
							</select>
						)}
						{parts.length > 1 && (
							<span className="text-sm text-gray-500">
								Part {partIndex + 1} of {parts.length}
							</span>
						)}
						{upNextEpisode && (
							<span className="text-sm text-gray-500">
								Up next: {upNextEpisode.episode_number}. {upNextEpisode.title}
							</span>
						)}
						{downloadUrl && (
							<a
								href={downloadUrl}
								className="inline-flex items-center gap-2 rounded-md border border-gray-700 px-3 py-1.5 text-sm text-gray-300 hover:bg-gray-800 hover:text-white"
							>
								<Download size={16} />
								Download
							</a>
						)}
					</div>
				</>
			) : nothingPlayable ? (
				<UnplayableSources sources={sources} verdicts={choice.verdicts} />
			) : playableId && !sources ? (
				// Loading renders nothing; a failed load says so and offers a
				// retry, rather than leaving an empty page.
				sourcesError ? (
					<div className="mb-6 flex items-center gap-3">
						<p className="text-red-500">{sourcesError.message}</p>
						<button
							type="button"
							onClick={() => refetchSources()}
							disabled={sourcesFetching}
							className="inline-flex items-center gap-1.5 rounded-md border border-gray-700 px-3 py-1.5 text-sm text-gray-300 hover:bg-gray-800 hover:text-white disabled:opacity-50"
						>
							<RefreshCw
								size={14}
								className={sourcesFetching ? "animate-spin" : ""}
							/>
							Retry
						</button>
					</div>
				) : null
			) : (
				<p className="mb-6 text-gray-500">
					{show && !selectedEpisodeId
						? "Select an episode to play."
						: "No streamable file."}
				</p>
			)}

			{show && (
				<EpisodeList
					show={show}
					activeEpisodeId={selectedEpisodeId}
					activeFileId={selectedFileId}
					onSelect={selectEpisode}
				/>
			)}
		</div>
	);
}

/** The title is in the library, but not in a form this browser can play: each
 * source with its reason, and the primary's download, which still works. */
function UnplayableSources({
	sources,
	verdicts,
}: {
	sources: MediaSource[];
	verdicts: Map<string, SourceVerdict>;
}) {
	const primary = sources.find((s) => s.is_primary) ?? sources[0];
	return (
		<section className="mb-6 max-w-4xl rounded-md border border-gray-700 bg-gray-800 p-4">
			<h2 className="mb-2 text-lg font-semibold">
				This title can't play in this browser
			</h2>
			<ul className="mb-3 space-y-1 text-sm text-gray-300">
				{sources.map((s) => {
					const verdict = verdicts.get(s.file_id);
					return (
						<li key={s.file_id}>
							<span className="font-medium">{sourceLabel(s)}</span>
							{verdict && !verdict.playable && `: ${verdict.detail}`}
						</li>
					);
				})}
			</ul>
			{primary && (
				<a
					href={absoluteUrl(primary.download_url)}
					className="inline-flex items-center gap-2 rounded-md border border-gray-700 px-3 py-1.5 text-sm text-gray-300 hover:bg-gray-700 hover:text-white"
				>
					<Download size={16} />
					Download to play it in a desktop player
				</a>
			)}
		</section>
	);
}

/** Exported for component tests (see `media.$id.test.tsx`); rendered only by
 * this route. */
export function EpisodeList({
	show,
	activeEpisodeId,
	activeFileId,
	onSelect,
}: {
	show: ShowMetadata;
	activeEpisodeId: string | null;
	/** Fallback highlight for `?fileId=` deep links, where the episode the
	 * file belongs to was never explicitly selected. */
	activeFileId: string | null;
	onSelect: (episode: EpisodeMetadata) => void;
}) {
	return (
		<div className="space-y-6">
			{show.seasons.map((season) => (
				<section key={season.season_number}>
					<h2 className="mb-3 text-xl font-semibold">
						Season {season.season_number}
					</h2>
					<ul className="divide-y divide-gray-700">
						{season.episodes.map((episode) => {
							const isActive =
								episode.id === activeEpisodeId ||
								(!!episode.file_id && episode.file_id === activeFileId);
							return (
								<li key={episode.id}>
									<button
										type="button"
										disabled={!episode.file_id}
										onClick={() => onSelect(episode)}
										className={`w-full px-2 py-3 text-left hover:bg-gray-800 disabled:opacity-50 ${isActive ? "bg-gray-800" : ""}`}
									>
										<span className="mr-2 text-gray-400">
											{episode.episode_number}.
										</span>
										<span>{episode.title}</span>
										{!episode.file_id && (
											<span className="ml-2 text-xs text-gray-500">
												No file
											</span>
										)}
										{episode.description && (
											<p className="mt-1 text-sm text-gray-500">
												{episode.description}
											</p>
										)}
									</button>
								</li>
							);
						})}
					</ul>
				</section>
			))}
		</div>
	);
}
