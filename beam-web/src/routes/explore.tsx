import type { ApolloClient } from "@apollo/client";
import { gql, type TypedDocumentNode } from "@apollo/client";
import { queryOptions } from "@tanstack/react-query";
import { createFileRoute, ErrorComponent } from "@tanstack/react-router";
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
							title {
								original
								localized
								alternatives
							}
							description
							year
							posterUrl
							backdropUrl
							genres
							ratings {
								tmdb
							}
							identifiers {
								imdbId
								tmdbId
								tvdbId
							}
						}
						... on ShowMetadata {
							title {
								original
								localized
								alternatives
							}
							description
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
		// Default search parameters - fetch first 20 items
		const variables: SearchMediaQueryVariables = {
			first: 20,
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

	if (!data) {
		return <div>No data...</div>;
	}

	return <pre>{JSON.stringify(data, null, 2)}</pre>; // TODO: Replace with actual UI
}
