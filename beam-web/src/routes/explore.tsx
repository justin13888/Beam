import type { ApolloClient } from "@apollo/client";
import { gql, type TypedDocumentNode } from "@apollo/client";
import { queryOptions } from "@tanstack/react-query";
import { createFileRoute, ErrorComponent, Link } from "@tanstack/react-router";
import {
	MediaSortField,
	type SearchMediaQuery,
	type SearchMediaQueryVariables,
	SortOrder,
} from "@/gql";

const SEARCH_MEDIA: TypedDocumentNode<
	SearchMediaQuery,
	SearchMediaQueryVariables
> = gql`
	query SearchMedia(
		$first: Int
		$after: String
		$last: Int
		$before: String
		$sortBy: MediaSortField
		$sortOrder: SortOrder
		$mediaType: MediaTypeFilter
		$genre: String
		$year: Int
		$yearFrom: Int
		$yearTo: Int
		$query: String
		$minRating: Int
	) {
		search(
			first: $first
			after: $after
			last: $last
			before: $before
			sortBy: $sortBy
			sortOrder: $sortOrder
			mediaType: $mediaType
			genre: $genre
			year: $year
			yearFrom: $yearFrom
			yearTo: $yearTo
			query: $query
			minRating: $minRating
		) {
			edges {
				cursor
				node {
					__typename
					... on MovieMetadata {
						id
						title {
							original
						}
						year
						posterUrl
					}
					... on ShowMetadata {
						id
						title {
							original
						}
						year
					}
				}
			}
			pageInfo {
				hasNextPage
				hasPreviousPage
				startCursor
				endCursor
			}
		}
	}
`;

const searchQueryOptions = (
	variables: SearchMediaQueryVariables,
	apolloClient: ApolloClient,
) =>
	queryOptions({
		queryKey: ["media", "search", variables],
		queryFn: async () => {
			const result = await apolloClient.query({
				query: SEARCH_MEDIA,
				variables,
			});
			return result.data;
		},
	});

export const Route = createFileRoute("/explore")({
	loader: async ({ context: { queryClient, apolloClient } }) => {
		const variables: SearchMediaQueryVariables = {
			first: 24,
			sortBy: MediaSortField.Title,
			sortOrder: SortOrder.Asc,
		};
		return queryClient.ensureQueryData(
			searchQueryOptions(variables, apolloClient),
		);
	},
	errorComponent: ({ error }) => <ErrorComponent error={error} />,
	component: RouteComponent,
});

function RouteComponent() {
	const data = Route.useLoaderData();
	const edges = data?.search?.edges ?? [];

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
					const title = node.title.original;
					const year = node.year ?? null;
					const poster =
						node.__typename === "MovieMetadata" ? node.posterUrl : null;
					const typeLabel =
						node.__typename === "MovieMetadata" ? "Movie" : "Show";
					return (
						<li key={node.id}>
							<Link
								to="/media/$id"
								params={{ id: node.id }}
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
