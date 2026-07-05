import { useQuery } from "@tanstack/react-query";
import {
	createFileRoute,
	Link,
	redirect,
	useNavigate,
} from "@tanstack/react-router";
import { Loader2, Search as SearchIcon } from "lucide-react";
import { useEffect, useState } from "react";
import type { components } from "@/api.gen";
import { useDebouncedValue } from "@/hooks/useDebouncedValue";
import { apiClient } from "@/lib/apiClient";

type MediaTypeFilter =
	components["schemas"]["beam_server.services.metadata.MediaTypeFilter"];

interface ExploreSearch {
	q?: string;
	mediaType?: MediaTypeFilter;
}

const MEDIA_TYPE_OPTIONS: { label: string; value?: MediaTypeFilter }[] = [
	{ label: "All", value: undefined },
	{ label: "Movies", value: "movie" },
	{ label: "Shows", value: "show" },
];

export const Route = createFileRoute("/explore")({
	validateSearch: (search: Record<string, unknown>): ExploreSearch => ({
		q: typeof search.q === "string" ? search.q : undefined,
		mediaType:
			search.mediaType === "movie" || search.mediaType === "show"
				? search.mediaType
				: undefined,
	}),
	beforeLoad: ({ context, location }) => {
		if (!context.auth.isAuthenticated) {
			throw redirect({
				to: "/login",
				search: { redirect: location.href },
			});
		}
	},
	component: RouteComponent,
});

function RouteComponent() {
	const { q: qParam, mediaType } = Route.useSearch();
	const navigate = useNavigate({ from: Route.fullPath });
	const [inputValue, setInputValue] = useState(qParam ?? "");
	const debouncedQuery = useDebouncedValue(inputValue, 300);

	// Keep the URL in sync with the settled query so search state is
	// shareable/bookmarkable and survives back/forward navigation, without
	// firing a navigation (and a request) on every keystroke.
	useEffect(() => {
		navigate({
			search: (prev) => ({ ...prev, q: debouncedQuery || undefined }),
			replace: true,
		});
	}, [debouncedQuery, navigate]);

	const { data, isLoading, isFetching, error } = useQuery({
		queryKey: ["media", "search", debouncedQuery, mediaType],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/media", {
				params: {
					query: {
						first: 48,
						sort_by: debouncedQuery ? undefined : "title",
						sort_order: "asc",
						query: debouncedQuery || undefined,
						media_type: mediaType,
					},
				},
				credentials: "include",
			});
			if (error) throw new Error("Failed to search media");
			return data;
		},
	});

	const edges = data?.edges ?? [];

	return (
		<div className="container mx-auto p-4">
			<h1 className="mb-6 text-2xl font-bold">Explore</h1>

			<div className="mb-6 flex flex-col gap-3 sm:flex-row sm:items-center">
				<div className="relative flex-1">
					<SearchIcon
						size={16}
						className="absolute top-1/2 left-3 -translate-y-1/2 text-gray-500"
					/>
					<input
						type="text"
						value={inputValue}
						onChange={(e) => setInputValue(e.target.value)}
						placeholder="Search titles..."
						className="w-full rounded-md border border-gray-700 bg-gray-800 py-2 pr-10 pl-10 text-sm text-white placeholder-gray-500 focus:border-cyan-500 focus:outline-none"
					/>
					{isFetching && (
						<Loader2
							size={16}
							className="absolute top-1/2 right-3 -translate-y-1/2 animate-spin text-gray-500"
						/>
					)}
				</div>
				<div className="flex overflow-hidden rounded-lg border border-gray-700">
					{MEDIA_TYPE_OPTIONS.map((opt) => (
						<button
							key={opt.label}
							type="button"
							onClick={() =>
								navigate({
									search: (prev) => ({ ...prev, mediaType: opt.value }),
									replace: true,
								})
							}
							className={`px-3 py-1.5 text-xs font-medium transition-colors ${
								mediaType === opt.value
									? "bg-cyan-600 text-white"
									: "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-white"
							}`}
						>
							{opt.label}
						</button>
					))}
				</div>
			</div>

			{isLoading ? (
				<div className="flex items-center justify-center py-12 text-gray-400">
					<Loader2 className="mr-2 animate-spin" size={18} />
					Loading...
				</div>
			) : error ? (
				<p className="text-red-400">{error.message}</p>
			) : edges.length === 0 ? (
				<p className="text-gray-500">
					{debouncedQuery
						? `No results for "${debouncedQuery}".`
						: "No media indexed yet. Create a library and scan it from the Libraries page."}
				</p>
			) : (
				<ul className="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">
					{edges.map((edge) => {
						const node = edge.node;
						const isMovie = "Movie" in node;
						const media = isMovie ? node.Movie : node.Show;
						const title = media.title.original;
						const year = media.year ?? null;
						const poster = isMovie
							? node.Movie.poster_url
							: (node.Show.seasons[0]?.poster_url ?? null);
						const typeLabel = isMovie ? "Movie" : "Show";
						return (
							<li key={media.id}>
								<Link
									to="/media/$id"
									params={{ id: media.id }}
									className="group block"
								>
									<div className="aspect-2/3 w-full overflow-hidden rounded-md bg-gray-800">
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
									</div>
									<h3 className="mt-2 line-clamp-2 text-sm font-medium">
										{title}
									</h3>
									<p className="text-xs text-gray-500">
										{typeLabel}
										{year ? ` · ${year}` : ""}
									</p>
								</Link>
							</li>
						);
					})}
				</ul>
			)}
		</div>
	);
}
