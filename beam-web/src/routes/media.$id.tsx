import type { ApolloClient } from "@apollo/client";
import { gql, type TypedDocumentNode } from "@apollo/client";
import { queryOptions } from "@tanstack/react-query";
import { createFileRoute, ErrorComponent } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { env } from "@/env";
import type {
	GetMediaMetadataByIdQuery,
	GetMediaMetadataByIdQueryVariables,
} from "@/gql";
import { useAuth } from "@/hooks/auth";
import { apiClient } from "@/lib/apiClient";

const GET_METADATA_BY_ID: TypedDocumentNode<
	GetMediaMetadataByIdQuery,
	GetMediaMetadataByIdQueryVariables
> = gql`
  	query GetMediaMetadataById($mediaId: ID!) {
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
	const { token } = useAuth();

	const [videoSrc, setVideoSrc] = useState<string | null>(null);
	const [videoError, setVideoError] = useState<string | null>(null);
	const objectUrlRef = useRef<string | null>(null);

	useEffect(() => {
		if (!token) return;

		let cancelled = false;

		const loadVideo = async () => {
			// 1. Exchange the user's auth token for a short-lived stream token.
			const tokenRes = await apiClient.POST("/v1/stream/{id}/token", {
				params: { path: { id } },
				headers: { Authorization: `Bearer ${token}` },
			});

			if (cancelled) return;

			if (tokenRes.error || !tokenRes.data) {
				setVideoError("Failed to obtain stream token.");
				return;
			}

			const streamToken = tokenRes.data.token;

			// 2. Fetch the video with the stream token in the Authorization header
			//    (never in the URL so it stays out of logs and browser history).
			const videoRes = await fetch(
				`${env.C_STREAM_SERVER_URL}/v1/stream/mp4/${id}`,
				{ headers: { Authorization: `Bearer ${streamToken}` } },
			);

			if (cancelled) return;

			if (!videoRes.ok) {
				setVideoError(`Failed to load video (HTTP ${videoRes.status}).`);
				return;
			}

			const blob = await videoRes.blob();
			if (cancelled) return;

			const url = URL.createObjectURL(blob);
			objectUrlRef.current = url;
			setVideoSrc(url);
		};

		loadVideo().catch((err) => {
			if (!cancelled) setVideoError(`Error loading video: ${err}`);
		});

		return () => {
			cancelled = true;
			if (objectUrlRef.current) {
				URL.revokeObjectURL(objectUrlRef.current);
				objectUrlRef.current = null;
			}
		};
	}, [token, id]);

	if (!data) {
		return <div>No data...</div>;
	}

	// TODO: Detect appropriate stream type later (MP4, HLS, DASH) depending on client capabilities in the future.
	return (
		<div className="container mx-auto p-4">
			<h1 className="text-2xl font-bold mb-4">
				{data.metadata?.title.original}
			</h1>
			<p className="mb-4">{data.metadata?.description}</p>
			{videoError && <p className="text-red-500 mb-4">{videoError}</p>}
			{videoSrc && (
				<video controls src={videoSrc} className="w-full max-w-4xl mb-4" />
			)}
			{/* Render additional metadata as needed */}
			<h2 className="text-xl font-semibold mb-2">Full Data:</h2>
			<pre>{JSON.stringify(data, null, 2)}</pre>
		</div>
	);
}
