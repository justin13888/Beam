import { useQueries, useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Play } from "lucide-react";
import type { components } from "@/api.gen";
import { useAuth } from "@/hooks/auth";
import { apiClient } from "@/lib/apiClient";
import { formatDuration } from "@/lib/utils";

type ContinueWatchingItem =
	components["schemas"]["beam_server.services.playback.ContinueWatchingItem"];
type MediaMetadata =
	components["schemas"]["beam_server.models.media.MediaMetadata"];

async function fetchMediaMetadata(
	mediaId: string,
): Promise<MediaMetadata | null> {
	const { data, error, response } = await apiClient.GET("/v1/media/{id}", {
		params: { path: { id: mediaId } },
		credentials: "include",
	});
	if (response.status === 404) return null;
	if (error) throw new Error("Failed to load media metadata");
	return data ?? null;
}

/** Horizontal row of in-progress files (FR-509), linking straight back into
 * the exact file that was playing via the `fileId` search param -- so a
 * mid-season episode resumes without having to re-pick it from the list. */
export function ContinueWatchingRow() {
	const { isAuthenticated } = useAuth();
	const { data: items, isLoading } = useQuery({
		queryKey: ["continue-watching"],
		queryFn: async (): Promise<ContinueWatchingItem[]> => {
			const { data, error } = await apiClient.GET("/v1/continue-watching", {
				params: { query: { limit: 20 } },
				credentials: "include",
			});
			if (error) throw new Error("Failed to load continue watching");
			return data ?? [];
		},
		enabled: isAuthenticated,
	});

	const metadataQueries = useQueries({
		queries: (items ?? []).map((item) => ({
			queryKey: ["media", item.media_id],
			queryFn: () => fetchMediaMetadata(item.media_id),
			staleTime: 60_000,
		})),
	});

	if (isLoading || !items || items.length === 0) return null;

	const cards = items
		.map((item, i) => ({ item, metadata: metadataQueries[i]?.data }))
		.filter(
			(c): c is { item: ContinueWatchingItem; metadata: MediaMetadata } =>
				!!c.metadata,
		);

	if (cards.length === 0) return null;

	return (
		<div className="mb-12">
			<h2 className="mb-4 text-xl font-semibold text-white">
				Continue Watching
			</h2>
			<div className="flex gap-4 overflow-x-auto pb-2">
				{cards.map(({ item, metadata }) => {
					const isMovie = "Movie" in metadata;
					const media = isMovie ? metadata.Movie : metadata.Show;
					const title = media.title.original;
					const poster = isMovie
						? metadata.Movie.poster_url
						: (metadata.Show.seasons[0]?.poster_url ?? null);
					const progressPct = item.duration_secs
						? Math.min(100, (item.position_secs / item.duration_secs) * 100)
						: 0;
					const remaining =
						item.duration_secs != null
							? Math.max(0, item.duration_secs - item.position_secs)
							: null;

					return (
						<Link
							key={item.file_id}
							to="/media/$id"
							params={{ id: media.id }}
							search={{ fileId: item.file_id }}
							className="group w-40 shrink-0"
						>
							<div className="relative aspect-2/3 w-full overflow-hidden rounded-md bg-gray-800">
								{poster ? (
									<img
										src={poster}
										alt={title}
										className="h-full w-full object-cover transition-transform group-hover:scale-105"
									/>
								) : (
									<div className="flex h-full items-center justify-center p-3 text-center text-sm text-gray-400">
										{title}
									</div>
								)}
								<div className="absolute inset-0 flex items-center justify-center bg-black/40 opacity-0 transition-opacity group-hover:opacity-100">
									<Play className="text-white" size={28} />
								</div>
								<div className="absolute right-0 bottom-0 left-0 h-1 bg-gray-700">
									<div
										className="h-full bg-cyan-500"
										style={{ width: `${progressPct}%` }}
									/>
								</div>
							</div>
							<h3 className="mt-2 line-clamp-1 text-sm font-medium text-white">
								{title}
							</h3>
							{remaining != null && (
								<p className="text-xs text-gray-500">
									{formatDuration(remaining)} left
								</p>
							)}
						</Link>
					);
				})}
			</div>
		</div>
	);
}
