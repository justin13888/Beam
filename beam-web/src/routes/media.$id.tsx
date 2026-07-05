import { queryOptions } from "@tanstack/react-query";
import { createFileRoute, ErrorComponent } from "@tanstack/react-router";
import { useMemo, useState } from "react";
import type { components } from "@/api.gen";
import { env } from "@/env";
import { apiClient } from "@/lib/apiClient";

type MediaMetadata =
	components["schemas"]["beam_server.models.media.MediaMetadata"];
type ShowMetadata =
	components["schemas"]["beam_server.models.media.show.ShowMetadata"];

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
	loader: async ({ context: { queryClient }, params: { id } }) => {
		return queryClient.ensureQueryData(mediaQueryOptions(id));
	},
	errorComponent: ({ error }) => <ErrorComponent error={error} />,
	component: RouteComponent,
});

function RouteComponent() {
	const metadata = Route.useLoaderData();

	const movie = metadata && "Movie" in metadata ? metadata.Movie : null;
	const show = metadata && "Show" in metadata ? metadata.Show : null;

	// For a movie the file is its primary file; for a show the user picks an
	// episode and that drives playback.
	const initialFileId = movie?.file_id ?? null;
	const [activeFileId, setActiveFileId] = useState<string | null>(
		initialFileId,
	);
	const [error, setError] = useState<string | null>(null);

	// `beam_session` is a same-site cookie (see ADR-0003), so a plain <video
	// src> pointed straight at the streaming endpoint carries it automatically
	// -- no separate stream-token round trip needed.
	const streamUrl = useMemo(() => {
		if (!activeFileId) return null;
		return `${env.C_STREAM_SERVER_URL}/v1/files/${activeFileId}/stream`;
	}, [activeFileId]);

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
					<h1 className="text-3xl font-bold">{title}</h1>
					{year && <p className="mt-1 text-gray-400">{year}</p>}
					{description && <p className="mt-4">{description}</p>}
				</div>
			</div>

			{error && <p className="mb-4 text-red-500">{error}</p>}
			{streamUrl ? (
				<video
					key={streamUrl}
					controls
					src={streamUrl}
					onError={() =>
						setError(
							"Failed to load video. Your session may have expired -- try refreshing the page.",
						)
					}
					className="mb-6 w-full max-w-4xl"
				/>
			) : (
				<p className="mb-6 text-gray-500">
					{show ? "Select an episode to play." : "No streamable file."}
				</p>
			)}

			{show && (
				<EpisodeList
					show={show}
					activeFileId={activeFileId}
					onSelect={(fileId) => {
						setError(null);
						setActiveFileId(fileId);
					}}
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
