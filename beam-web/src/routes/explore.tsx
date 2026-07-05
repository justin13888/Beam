import { queryOptions } from "@tanstack/react-query";
import {
	createFileRoute,
	ErrorComponent,
	Link,
	redirect,
} from "@tanstack/react-router";
import { apiClient } from "@/lib/apiClient";

interface SearchParams {
	first?: number;
	sortBy?: "title" | "year" | "rating" | "date_added" | "runtime";
	sortOrder?: "asc" | "desc";
}

const searchQueryOptions = (params: SearchParams) =>
	queryOptions({
		queryKey: ["media", "search", params],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/media", {
				params: {
					query: {
						first: params.first,
						sort_by: params.sortBy,
						sort_order: params.sortOrder,
					},
				},
				credentials: "include",
			});
			if (error) throw new Error("Failed to search media");
			return data;
		},
	});

export const Route = createFileRoute("/explore")({
	beforeLoad: ({ context, location }) => {
		if (!context.auth.isAuthenticated) {
			throw redirect({
				to: "/login",
				search: { redirect: location.href },
			});
		}
	},
	loader: async ({ context: { queryClient } }) => {
		const params: SearchParams = {
			first: 24,
			sortBy: "title",
			sortOrder: "asc",
		};
		return queryClient.ensureQueryData(searchQueryOptions(params));
	},
	errorComponent: ({ error }) => <ErrorComponent error={error} />,
	component: RouteComponent,
});

function RouteComponent() {
	const data = Route.useLoaderData();
	const edges = data?.edges ?? [];

	if (edges.length === 0) {
		return (
			<div className="container mx-auto p-4">
				<h1 className="mb-4 text-2xl font-bold">Explore</h1>
				<p className="text-gray-500">
					No media indexed yet. Create a library and scan it from the Libraries
					page.
				</p>
			</div>
		);
	}

	return (
		<div className="container mx-auto p-4">
			<h1 className="mb-6 text-2xl font-bold">Explore</h1>
			<ul className="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">
				{edges.map((edge) => {
					const node = edge.node;
					const isMovie = "Movie" in node;
					const media = isMovie ? node.Movie : node.Show;
					const title = media.title.original;
					const year = media.year ?? null;
					const poster = isMovie ? node.Movie.poster_url : null;
					const typeLabel = isMovie ? "Movie" : "Show";
					return (
						<li key={media.id}>
							<Link
								to="/media/$id"
								params={{ id: media.id }}
								className="group block"
							>
								<div className="aspect-[2/3] w-full overflow-hidden rounded-md bg-gray-800">
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
		</div>
	);
}
