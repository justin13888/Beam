import type { ApolloClient } from "@apollo/client";
import { gql, type TypedDocumentNode } from "@apollo/client";
import { queryOptions } from "@tanstack/react-query";
import { createFileRoute, ErrorComponent } from "@tanstack/react-router";
import { useEffect, useState } from "react";
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
			... on MovieMetadata {
				id
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
				fileId
			}
			... on ShowMetadata {
				id
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
						id
						episodeNumber
						title
						description
						airDate
						thumbnailUrl
						duration
						fileId
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

type ShowMeta = Extract<
	NonNullable<GetMediaMetadataByIdQuery["metadata"]>,
	{ __typename: "ShowMetadata" }
>;

function RouteComponent() {
	const data = Route.useLoaderData();
	const metadata = data?.metadata;
	const { token } = useAuth();

	// For a movie the file is its primary file; for a show the user picks an
	// episode and that drives playback.
	const initialFileId =
		metadata?.__typename === "MovieMetadata" ? (metadata.fileId ?? null) : null;
	const [activeFileId, setActiveFileId] = useState<string | null>(
		initialFileId,
	);
	const [streamUrl, setStreamUrl] = useState<string | null>(null);
	const [error, setError] = useState<string | null>(null);

	useEffect(() => {
		if (!token || !activeFileId) {
			setStreamUrl(null);
			return;
		}
		let cancelled = false;
		setError(null);
		(async () => {
			const tokenRes = await apiClient.POST(
				"/v1/files/{file_id}/stream-token",
				{
					params: {
						path: { file_id: activeFileId },
						header: { Authorization: `Bearer ${token}` },
					},
				},
			);
			if (cancelled) return;
			if (tokenRes.error || !tokenRes.data) {
				setError("Failed to obtain stream token.");
				return;
			}
			const streamToken = tokenRes.data.token;
			// <video src> can't set headers, so the short-lived stream token
			// rides in the query string; the browser then performs range
			// requests directly against the streaming endpoint.
			setStreamUrl(
				`${env.C_STREAM_SERVER_URL}/v1/files/${activeFileId}/stream?token=${encodeURIComponent(streamToken)}`,
			);
		})().catch((err) => {
			if (!cancelled) setError(`Error loading video: ${err}`);
		});
		return () => {
			cancelled = true;
		};
	}, [token, activeFileId]);

	if (!metadata) {
		return (
			<div className="container mx-auto p-4">
				<p>Media not found.</p>
			</div>
		);
	}

	const title = metadata.title.original;
	const year = metadata.year;
	const description = metadata.description ?? null;
	const posterUrl =
		metadata.__typename === "MovieMetadata"
			? metadata.posterUrl
			: (metadata.seasons[0]?.posterUrl ?? null);

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
					className="mb-6 w-full max-w-4xl"
				/>
			) : (
				<p className="mb-6 text-gray-500">
					{metadata.__typename === "ShowMetadata"
						? "Select an episode to play."
						: "No streamable file."}
				</p>
			)}

			{metadata.__typename === "ShowMetadata" && (
				<EpisodeList
					show={metadata}
					activeFileId={activeFileId}
					onSelect={setActiveFileId}
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
	show: ShowMeta;
	activeFileId: string | null;
	onSelect: (fileId: string) => void;
}) {
	return (
		<div className="space-y-6">
			{show.seasons.map((season) => (
				<section key={season.seasonNumber}>
					<h2 className="mb-3 text-xl font-semibold">
						Season {season.seasonNumber}
					</h2>
					<ul className="divide-y divide-gray-700">
						{season.episodes.map((episode) => {
							const isActive =
								!!episode.fileId && episode.fileId === activeFileId;
							return (
								<li key={episode.id}>
									<button
										type="button"
										disabled={!episode.fileId}
										onClick={() => {
											if (episode.fileId) onSelect(episode.fileId);
										}}
										className={`w-full px-2 py-3 text-left hover:bg-gray-800 disabled:opacity-50 ${isActive ? "bg-gray-800" : ""}`}
									>
										<span className="mr-2 text-gray-400">
											{episode.episodeNumber}.
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
