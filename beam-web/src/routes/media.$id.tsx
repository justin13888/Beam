import { queryOptions, useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, ErrorComponent } from "@tanstack/react-router";
import { Download, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import type { components } from "@/api.gen";
import { VideoPlayer } from "@/components/VideoPlayer";
import { env } from "@/env";
import { useAuth } from "@/hooks/auth";
import { usePlaybackBeacon } from "@/hooks/usePlaybackBeacon";
import { apiClient } from "@/lib/apiClient";
import { formatDuration } from "@/lib/utils";

type MediaMetadata =
	components["schemas"]["beam_server.models.media.MediaMetadata"];
type ShowMetadata =
	components["schemas"]["beam_server.models.media.show.ShowMetadata"];
type MediaSource =
	components["schemas"]["beam_server.models.media.source.MediaSource"];

const mediaQueryOptions = (mediaId: string) =>
	queryOptions({
		queryKey: ["media", mediaId],
		queryFn: async (): Promise<MediaMetadata | null> => {
			const { data, error, response } = await apiClient.GET("/v1/media/{id}", {
				params: { path: { id: mediaId } },
				credentials: "include",
			});
			if (response.status === 404) return null;
			if (error) throw new Error("Failed to load media metadata");
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
	errorComponent: ({ error }) => <ErrorComponent error={error} />,
	component: RouteComponent,
});

/** A short human label for a quality/edition option in the source picker. */
function sourceLabel(source: MediaSource): string {
	if (source.video) {
		return `${source.video.height}p · ${source.video.codec}`;
	}
	return "Original";
}

function absoluteUrl(path: string): string {
	return path.startsWith("http") ? path : `${env.C_STREAM_SERVER_URL}${path}`;
}

function RouteComponent() {
	const metadata = Route.useLoaderData();
	const { fileId: fileIdParam } = Route.useSearch();

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
				throw new Error("Failed to request a metadata refresh");
			}
		},
	});

	// For a movie the file is (by default) its primary file; for a show the
	// user picks an episode. A `fileId` search param (set by continue-watching
	// links) overrides both, deep-linking straight to the in-progress file.
	const [selectedFileId, setSelectedFileId] = useState<string | null>(
		fileIdParam ?? movie?.file_id ?? null,
	);
	const [playbackError, setPlaybackError] = useState<string | null>(null);
	const [startOver, setStartOver] = useState(false);

	// Movies can have multiple playable editions/qualities (ADR-0004: never
	// live-transcode -- pick among pre-existing files instead); episodes
	// don't support this yet, so they always resolve to a single direct file.
	const { data: sources } = useQuery({
		queryKey: ["media", mediaId, "sources"],
		queryFn: async (): Promise<MediaSource[]> => {
			if (!mediaId) return [];
			const { data, error, response } = await apiClient.GET(
				"/v1/media/{id}/sources",
				{
					params: { path: { id: mediaId } },
					credentials: "include",
				},
			);
			if (response.status === 404 || response.status === 400) return [];
			if (error) throw new Error("Failed to load media sources");
			return data ?? [];
		},
		enabled: !!movie,
	});

	useEffect(() => {
		if (!movie || !sources || sources.length === 0) return;
		if (selectedFileId && sources.some((s) => s.file_id === selectedFileId)) {
			return;
		}
		setSelectedFileId(sources[0].file_id);
	}, [movie, sources, selectedFileId]);

	const selectedSource =
		sources?.find((s) => s.file_id === selectedFileId) ?? null;

	const { data: continueWatching } = useQuery({
		queryKey: ["continue-watching"],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/continue-watching", {
				params: { query: { limit: 100 } },
				credentials: "include",
			});
			if (error) throw new Error("Failed to load playback progress");
			return data ?? [];
		},
	});

	const savedProgress =
		continueWatching?.find((item) => item.file_id === selectedFileId) ?? null;
	const resumePosition =
		!startOver && savedProgress ? savedProgress.position_secs : 0;

	const { report, reset: resetBeacon } = usePlaybackBeacon(selectedFileId);

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
	const posterUrl = movie
		? movie.poster_url
		: (show?.seasons[0]?.poster_url ?? null);

	const streamUrl = selectedSource
		? absoluteUrl(selectedSource.stream_url)
		: selectedFileId
			? `${env.C_STREAM_SERVER_URL}/v1/files/${selectedFileId}/stream`
			: null;
	const downloadUrl = selectedSource
		? absoluteUrl(selectedSource.download_url)
		: selectedFileId
			? `${env.C_STREAM_SERVER_URL}/v1/files/${selectedFileId}/download`
			: null;

	function selectFile(fileId: string) {
		setPlaybackError(null);
		setStartOver(false);
		resetBeacon();
		setSelectedFileId(fileId);
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
					{refreshMetadataMutation.isError && (
						<p className="mt-2 text-sm text-red-400">
							Failed to request a metadata refresh.
						</p>
					)}
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
						type={selectedSource?.mime_type ?? undefined}
						poster={posterUrl}
						startTime={resumePosition}
						onProgress={(currentTime, duration) =>
							report(currentTime, duration)
						}
						onEnded={(duration) => report(duration, duration, true)}
						onError={() =>
							setPlaybackError(
								"Failed to load video. Your session may have expired -- try refreshing the page.",
							)
						}
						className="mb-4 w-full max-w-4xl"
					/>

					<div className="mb-6 flex flex-wrap items-center gap-3">
						{movie && sources && sources.length > 1 && (
							<select
								value={selectedFileId ?? ""}
								onChange={(e) => selectFile(e.target.value)}
								className="rounded-md border border-gray-700 bg-gray-800 px-3 py-1.5 text-sm text-white"
							>
								{sources.map((s) => (
									<option key={s.file_id} value={s.file_id}>
										{sourceLabel(s)}
									</option>
								))}
							</select>
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
			) : (
				<p className="mb-6 text-gray-500">
					{show ? "Select an episode to play." : "No streamable file."}
				</p>
			)}

			{show && (
				<EpisodeList
					show={show}
					activeFileId={selectedFileId}
					onSelect={selectFile}
				/>
			)}
		</div>
	);
}

function EpisodeList({
	show,
	activeFileId,
	onSelect,
}: {
	show: ShowMetadata;
	activeFileId: string | null;
	onSelect: (fileId: string) => void;
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
								!!episode.file_id && episode.file_id === activeFileId;
							return (
								<li key={episode.id}>
									<button
										type="button"
										disabled={!episode.file_id}
										onClick={() => {
											if (episode.file_id) onSelect(episode.file_id);
										}}
										className={`w-full px-2 py-3 text-left hover:bg-gray-800 disabled:opacity-50 ${isActive ? "bg-gray-800" : ""}`}
									>
										<span className="mr-2 text-gray-400">
											{episode.episode_number}.
										</span>
										<span>{episode.title}</span>
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
