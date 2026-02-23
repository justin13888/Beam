import type { ApolloClient } from "@apollo/client";
import { gql, type TypedDocumentNode } from "@apollo/client";
import { queryOptions } from "@tanstack/react-query";
import { createFileRoute, ErrorComponent } from "@tanstack/react-router";
import { env } from "@/env";
import type {
	GetMediaMetadataByIdQuery,
	GetMediaMetadataByIdQueryVariables,
} from "@/gql";

const GET_METADATA_BY_ID: TypedDocumentNode<
	GetMediaMetadataByIdQuery,
	GetMediaMetadataByIdQueryVariables
> = gql`
  	query GetMediaMetadataById($mediaId: ID!) {
		media {
			metadata(id: $mediaId) {
			__typename
			... on ShowMetadata {
				title {
				original
				localized
				alternatives
				}
				description
				year
				seasons {
					seasonNumber
					dates {
						firstAired
						lastAired
					}
					episodeRuntime
					episodes {
						episodeNumber
						title
						description
						airDate
						thumbnailUrl
						duration
						streams {
							videoTracks {
								codec
								maxRate
								bitRate
								resolution {
									width
									height
								}
								frameRate
							}
							audioTracks {
								codec
								language
								title
								channelLayout
								isDefault
								isAutoselect
							}
							subtitleTracks {
								codec
								language
								title
								isDefault
								isAutoselect
								isForced
							}
						}
					}
					posterUrl
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
			}
			... on MovieMetadata {
				title {
					original
					localized
					alternatives
				}
				description
				year
				releaseDate
				runtime
				duration
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
				streams {
					videoTracks {
						codec
						maxRate
						bitRate
						resolution {
						width
						height
						}
						frameRate
					}
					audioTracks {
						codec
						language
						title
						channelLayout
						isDefault
						isAutoselect
					}
					subtitleTracks {
						codec
						language
						title
						isDefault
						isAutoselect
						isForced
					}
				}
			}
			}
		}
	}
`;

const mediaQueryOptions = (mediaId: string, apolloClient: ApolloClient) =>
	queryOptions({
		queryKey: ["media", mediaId],
		queryFn: async () => {
			const result = await apolloClient.query({
				query: GET_METADATA_BY_ID,
				variables: { mediaId },
			});
			return result.data;
		},
	});

export const Route = createFileRoute("/media/$id")({
	loader: async ({
		context: { queryClient, apolloClient },
		params: { id },
	}) => {
		return queryClient.ensureQueryData(mediaQueryOptions(id, apolloClient));
	},
	errorComponent: ({ error }) => <ErrorComponent error={error} />,
	component: RouteComponent,
});

function RouteComponent() {
	const { id } = Route.useParams();
	const data = Route.useLoaderData();

	if (!data) {
		return <div>No data...</div>;
	}

	const streamLink = `${env.C_STREAM_SERVER_URL}/v1/stream/mp4/${id}`;
	// TODO: Detect appropriate stream type later (MP4, HLS, DASH) depending on client capabilities in the future.
	return (
		<div className="container mx-auto p-4">
			<h1 className="text-2xl font-bold mb-4">
				{data.media.metadata?.title.original}
			</h1>
			<p className="mb-4">{data.media.metadata?.description}</p>
			{/* Watch Link */}
			<a
				href={streamLink}
				className="text-blue-600 hover:text-blue-800 underline"
			>
				{streamLink}
			</a>
			{/* Render additional metadata as needed */}
			<h2 className="text-xl font-semibold mb-2">Full Data:</h2>
			<pre>{JSON.stringify(data, null, 2)}</pre>
		</div>
	);
}
