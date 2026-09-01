import { useQueries, useQuery } from "@tanstack/react-query";
import {
	createFileRoute,
	Link,
	redirect,
	useNavigate,
} from "@tanstack/react-router";
import {
	CheckCircle2,
	ChevronLeft,
	ChevronRight,
	Loader2,
	Play,
	RotateCcw,
} from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import type { components } from "@/api.gen";
import { useAuth } from "@/hooks/auth";
import { apiClient } from "@/lib/apiClient";
import { artworkSrc } from "@/lib/artwork";

type HistoryItem =
	components["schemas"]["beam_server.services.playback.HistoryItem"];
type MediaMetadata =
	components["schemas"]["beam_server.models.media.MediaMetadata"];
type ShowMetadata =
	components["schemas"]["beam_server.models.media.show.ShowMetadata"];

/** Matches the server's default page size for `GET /v1/history`. */
const PAGE_SIZE = 50;

export const Route = createFileRoute("/history")({
	beforeLoad: ({ context, location }) => {
		if (!context.auth.isAuthenticated) {
			throw redirect({
				to: "/login",
				search: { redirect: location.href },
			});
		}
	},
	component: HistoryPage,
});

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

/** Short relative label for an RFC3339 timestamp (falls back to a date). */
function formatRelative(iso: string): string {
	const then = new Date(iso).getTime();
	if (Number.isNaN(then)) return iso;
	const diffSecs = Math.floor((Date.now() - then) / 1000);
	if (diffSecs < 60) return "Just now";
	const diffMins = Math.floor(diffSecs / 60);
	if (diffMins < 60) return `${diffMins}m ago`;
	const diffHrs = Math.floor(diffMins / 60);
	if (diffHrs < 24) return `${diffHrs}h ago`;
	const diffDays = Math.floor(diffHrs / 24);
	if (diffDays < 30) return `${diffDays}d ago`;
	return new Date(then).toLocaleDateString();
}

/** SxxEyy for the episode within the fetched show metadata, or a generic
 * "Episode" tag when the episode is no longer listed (e.g. season pruned). */
function episodeLabel(show: ShowMetadata, episodeId: string): string {
	for (const season of show.seasons) {
		const episode = season.episodes.find((e) => e.id === episodeId);
		if (episode) {
			const s = String(season.season_number).padStart(2, "0");
			const e = String(episode.episode_number).padStart(2, "0");
			return `S${s}E${e}`;
		}
	}
	return "Episode";
}

export function HistoryPage() {
	const { isAuthenticated } = useAuth();
	const navigate = useNavigate();
	const [page, setPage] = useState(0);

	const { data, isLoading, error } = useQuery({
		queryKey: ["history", page],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/history", {
				params: { query: { limit: PAGE_SIZE, offset: page * PAGE_SIZE } },
				credentials: "include",
			});
			if (error || data === undefined) {
				throw new Error("Failed to load watch history");
			}
			return data;
		},
		enabled: isAuthenticated,
	});

	const items = data?.items ?? [];
	const total = data?.total ?? 0;

	// Poster/title fan-out, one query per media id (deduplicated by key),
	// following ContinueWatchingRow.
	const metadataQueries = useQueries({
		queries: items.map((item) => ({
			queryKey: ["media", item.media_id],
			queryFn: () => fetchMediaMetadata(item.media_id),
			staleTime: 60_000,
		})),
	});

	/** Restart from the beginning: zero the resume point (keeping the known
	 * duration so the row's percentage stays meaningful), then open the same
	 * deep link Resume uses. */
	const handleStartOver = async (item: HistoryItem) => {
		const { error, response } = await apiClient.PUT(
			"/v1/files/{file_id}/progress",
			{
				params: { path: { file_id: item.file_id } },
				body: { position_secs: 0, duration_secs: item.duration_secs },
				credentials: "include",
			},
		);
		if (error || !response.ok) {
			toast.error("Failed to reset progress");
			return;
		}
		navigate({
			to: "/media/$id",
			params: { id: item.media_id },
			search: { fileId: item.file_id },
		});
	};

	const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE));
	const hasPrev = page > 0;
	const hasNext = (page + 1) * PAGE_SIZE < total;

	// Rows whose media metadata has resolved; stale/loading ones are omitted
	// rather than rendered blank (same policy as ContinueWatchingRow).
	const rows = items
		.map((item, i) => ({ item, metadata: metadataQueries[i]?.data }))
		.filter(
			(r): r is { item: HistoryItem; metadata: MediaMetadata } => !!r.metadata,
		);

	return (
		<div className="container mx-auto max-w-3xl p-4">
			<h1 className="mb-6 text-2xl font-bold text-white">Watch History</h1>

			{isLoading ? (
				<div className="flex items-center justify-center py-12 text-gray-400">
					<Loader2 className="mr-2 animate-spin" size={18} />
					Loading...
				</div>
			) : error ? (
				<p className="text-red-400">{error.message}</p>
			) : total === 0 ? (
				<p className="py-8 text-gray-500">
					Nothing watched yet. Anything you play shows up here.
				</p>
			) : (
				<>
					<ul className="space-y-3">
						{rows.map(({ item, metadata }) => {
							const isMovie = "Movie" in metadata;
							const media = isMovie ? metadata.Movie : metadata.Show;
							const title = media.title.original;
							// A show's first season can lack art, so take the first
							// season that actually has a poster (matching explore).
							const poster = artworkSrc(
								isMovie
									? metadata.Movie.poster_url
									: (metadata.Show.seasons.find((s) => s.poster_url)
											?.poster_url ?? null),
							);
							const progressPct = item.duration_secs
								? Math.min(100, (item.position_secs / item.duration_secs) * 100)
								: 0;

							return (
								<li
									key={item.file_id}
									className="flex items-center gap-4 rounded-lg border border-gray-700 bg-gray-900 p-3"
								>
									<div className="aspect-2/3 w-14 shrink-0 overflow-hidden rounded bg-gray-800">
										{poster ? (
											<img
												src={poster}
												alt={title}
												className="h-full w-full object-cover"
											/>
										) : (
											<div className="flex h-full items-center justify-center p-1 text-center text-[10px] text-gray-500">
												{title}
											</div>
										)}
									</div>

									<div className="min-w-0 flex-1">
										<div className="flex flex-wrap items-center gap-2">
											<h3 className="truncate text-sm font-medium text-white">
												{title}
											</h3>
											{!isMovie && item.episode_id && (
												<span className="rounded bg-gray-700 px-1.5 py-0.5 text-[10px] font-medium text-gray-300">
													{episodeLabel(metadata.Show, item.episode_id)}
												</span>
											)}
										</div>
										<p className="mt-0.5 text-xs text-gray-500">
											Watched {formatRelative(item.updated_at)}
										</p>
										{item.completed ? (
											<span className="mt-1.5 inline-flex items-center gap-1 text-xs font-medium text-emerald-400">
												<CheckCircle2 size={12} />
												Completed
											</span>
										) : (
											<div className="mt-2 h-1 w-full max-w-48 rounded bg-gray-700">
												<div
													className="h-full rounded bg-cyan-500"
													style={{ width: `${progressPct}%` }}
												/>
											</div>
										)}
									</div>

									<div className="flex shrink-0 items-center gap-2">
										<Link
											to="/media/$id"
											params={{ id: item.media_id }}
											search={{ fileId: item.file_id }}
											className="inline-flex items-center gap-1.5 rounded-md bg-cyan-600 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-cyan-700"
										>
											<Play size={12} />
											Resume
										</Link>
										<button
											type="button"
											onClick={() => handleStartOver(item)}
											className="inline-flex items-center gap-1.5 rounded-md border border-gray-600 px-3 py-1.5 text-xs font-medium text-gray-300 transition-colors hover:bg-gray-700 hover:text-white"
										>
											<RotateCcw size={12} />
											Start over
										</button>
									</div>
								</li>
							);
						})}
					</ul>

					{totalPages > 1 && (
						<div className="mt-6 flex items-center justify-between">
							<button
								type="button"
								onClick={() => setPage((p) => Math.max(0, p - 1))}
								disabled={!hasPrev}
								className="inline-flex items-center gap-1 rounded-md border border-gray-700 bg-gray-800 px-3 py-1.5 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-700 hover:text-white disabled:cursor-not-allowed disabled:opacity-50"
							>
								<ChevronLeft size={14} />
								Prev
							</button>
							<span className="text-xs text-gray-500">
								Page {page + 1} of {totalPages}
							</span>
							<button
								type="button"
								onClick={() => setPage((p) => p + 1)}
								disabled={!hasNext}
								className="inline-flex items-center gap-1 rounded-md border border-gray-700 bg-gray-800 px-3 py-1.5 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-700 hover:text-white disabled:cursor-not-allowed disabled:opacity-50"
							>
								Next
								<ChevronRight size={14} />
							</button>
						</div>
					)}
				</>
			)}
		</div>
	);
}
