import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import {
	createFileRoute,
	Link,
	redirect,
	useNavigate,
} from "@tanstack/react-router";
import {
	ArrowDown,
	ArrowUp,
	Loader2,
	Search as SearchIcon,
	X,
} from "lucide-react";
import { useEffect, useState } from "react";
import type { components } from "@/api.gen";
import { useDebouncedValue } from "@/hooks/useDebouncedValue";
import { apiClient } from "@/lib/apiClient";
import { artworkSrc } from "@/lib/artwork";
import { apiError } from "@/lib/problem";

type MediaTypeFilter =
	components["schemas"]["beam_server.services.metadata.MediaTypeFilter"];
type SortOrder =
	components["schemas"]["beam_server.services.metadata.SortOrder"];

/** The subset of the API's `sort_by` fields exposed as explore controls. */
const SORT_BY_OPTIONS = ["title", "year", "rating"] as const;
type SortBy = (typeof SORT_BY_OPTIONS)[number];

/** Minimum-rating choices, on the familiar 0-10 scale shown to users. */
const MIN_RATING_OPTIONS = [6, 7, 8, 9] as const;

const PAGE_SIZE = 48;

export interface ExploreSearch {
	q?: string;
	mediaType?: MediaTypeFilter;
	genre?: string;
	yearFrom?: number;
	yearTo?: number;
	/** Minimum rating on a 0-10 scale (the API takes 0-100). */
	minRating?: number;
	sortBy?: SortBy;
	sortOrder?: SortOrder;
}

const MEDIA_TYPE_OPTIONS: { label: string; value?: MediaTypeFilter }[] = [
	{ label: "All", value: undefined },
	{ label: "Movies", value: "movie" },
	{ label: "Shows", value: "show" },
];

/** Coerces a search-param value to an integer within [min, max]; anything
 * else (missing, non-numeric, fractional, out of range) is dropped. */
function coerceInt(
	value: unknown,
	min: number,
	max: number,
): number | undefined {
	const n =
		typeof value === "number"
			? value
			: typeof value === "string" && value !== ""
				? Number(value)
				: Number.NaN;
	return Number.isInteger(n) && n >= min && n <= max ? n : undefined;
}

/** Parses a year text-input value; empty/invalid input means "no filter". */
function parseYearInput(value: string): number | undefined {
	return coerceInt(value.trim(), 1, 9999);
}

export const Route = createFileRoute("/explore")({
	validateSearch: (search: Record<string, unknown>): ExploreSearch => ({
		q: typeof search.q === "string" ? search.q : undefined,
		mediaType:
			search.mediaType === "movie" || search.mediaType === "show"
				? search.mediaType
				: undefined,
		genre:
			typeof search.genre === "string" && search.genre !== ""
				? search.genre
				: undefined,
		yearFrom: coerceInt(search.yearFrom, 1, 9999),
		yearTo: coerceInt(search.yearTo, 1, 9999),
		minRating: coerceInt(search.minRating, 1, 10),
		sortBy: SORT_BY_OPTIONS.includes(search.sortBy as SortBy)
			? (search.sortBy as SortBy)
			: undefined,
		sortOrder:
			search.sortOrder === "asc" || search.sortOrder === "desc"
				? search.sortOrder
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

/** Narrow navigation contract the page needs; satisfied by the router's
 * `useNavigate` and trivially fakeable in tests. */
export type ExploreNavigate = (opts: {
	search: ExploreSearch | ((prev: ExploreSearch) => ExploreSearch);
	replace?: boolean;
}) => void;

function RouteComponent() {
	const search = Route.useSearch();
	const navigate = useNavigate({ from: Route.fullPath });
	return (
		<ExplorePage
			search={search}
			navigate={(opts) =>
				navigate({ search: opts.search, replace: opts.replace })
			}
		/>
	);
}

const selectClass =
	"rounded-md border border-gray-700 bg-gray-800 px-2 py-1.5 text-xs text-gray-300 focus:border-cyan-500 focus:outline-none";

export function ExplorePage({
	search,
	navigate,
}: {
	search: ExploreSearch;
	navigate: ExploreNavigate;
}) {
	const {
		q: qParam,
		mediaType,
		genre,
		yearFrom,
		yearTo,
		minRating,
		sortBy,
		sortOrder,
	} = search;
	const [inputValue, setInputValue] = useState(qParam ?? "");
	const [yearFromInput, setYearFromInput] = useState(
		yearFrom !== undefined ? String(yearFrom) : "",
	);
	const [yearToInput, setYearToInput] = useState(
		yearTo !== undefined ? String(yearTo) : "",
	);
	const debouncedQuery = useDebouncedValue(inputValue, 300);
	const debouncedYearFrom = useDebouncedValue(yearFromInput, 300);
	const debouncedYearTo = useDebouncedValue(yearToInput, 300);

	// Keep the URL in sync with the settled text inputs so search state is
	// shareable/bookmarkable and survives back/forward navigation, without
	// firing a navigation (and a request) on every keystroke.
	useEffect(() => {
		navigate({
			search: (prev) => ({
				...prev,
				q: debouncedQuery || undefined,
				yearFrom: parseYearInput(debouncedYearFrom),
				yearTo: parseYearInput(debouncedYearTo),
			}),
			replace: true,
		});
	}, [debouncedQuery, debouncedYearFrom, debouncedYearTo, navigate]);

	// The genre catalog backing the genre <select>. The control is simply
	// hidden when the list is empty or fails to load -- browsing must not
	// break because one filter's vocabulary is unavailable.
	const genresQuery = useQuery({
		queryKey: ["genres"],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/genres", {
				credentials: "include",
			});
			if (error) throw apiError(error, "Failed to load genres");
			return data;
		},
		staleTime: 5 * 60 * 1000,
	});
	const genreOptions = genresQuery.data?.genres ?? [];

	// Without an explicit sort, a text search ranks by relevance (no
	// sort_by) while plain browsing stays alphabetical -- this page's
	// behavior before the sort controls existed.
	const effectiveSortBy = sortBy ?? (debouncedQuery ? undefined : "title");
	const effectiveSortOrder = sortOrder ?? "asc";

	const {
		data,
		isLoading,
		isFetching,
		isFetchingNextPage,
		hasNextPage,
		fetchNextPage,
		error,
	} = useInfiniteQuery({
		// Every filter participates in the key: changing any of them refetches
		// from the first page (pagination resets via the new key).
		queryKey: [
			"media",
			"search",
			debouncedQuery,
			mediaType,
			genre,
			yearFrom,
			yearTo,
			minRating,
			sortBy,
			sortOrder,
		],
		queryFn: async ({ pageParam }) => {
			const { data, error } = await apiClient.GET("/v1/media", {
				params: {
					query: {
						first: PAGE_SIZE,
						after: pageParam,
						sort_by: effectiveSortBy,
						sort_order: effectiveSortOrder,
						query: debouncedQuery || undefined,
						media_type: mediaType,
						genre,
						year_from: yearFrom,
						year_to: yearTo,
						min_rating: minRating !== undefined ? minRating * 10 : undefined,
					},
				},
				credentials: "include",
			});
			if (error || data === undefined) {
				throw apiError(error, "Failed to search media");
			}
			return data;
		},
		initialPageParam: undefined as string | undefined,
		getNextPageParam: (lastPage) =>
			lastPage.page_info.has_next_page
				? (lastPage.page_info.end_cursor ?? undefined)
				: undefined,
	});

	const edges = data?.pages.flatMap((page) => page.edges) ?? [];

	const hasActiveFilters =
		mediaType !== undefined ||
		genre !== undefined ||
		yearFrom !== undefined ||
		yearTo !== undefined ||
		minRating !== undefined ||
		sortBy !== undefined ||
		sortOrder !== undefined;

	const clearFilters = () => {
		setYearFromInput("");
		setYearToInput("");
		navigate({
			search: (prev) => ({ q: prev.q || undefined }),
			replace: true,
		});
	};

	return (
		<div className="container mx-auto p-4">
			<h1 className="mb-6 text-2xl font-bold">Explore</h1>

			<div className="mb-3 flex flex-col gap-3 sm:flex-row sm:items-center">
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

			<div className="mb-6 flex flex-wrap items-center gap-2">
				{genreOptions.length > 0 && (
					<select
						aria-label="Genre"
						value={genre ?? ""}
						onChange={(e) => {
							const value = e.target.value || undefined;
							navigate({
								search: (prev) => ({ ...prev, genre: value }),
								replace: true,
							});
						}}
						className={selectClass}
					>
						<option value="">All genres</option>
						{genreOptions.map((name) => (
							<option key={name} value={name}>
								{name}
							</option>
						))}
					</select>
				)}
				<input
					type="number"
					aria-label="Year from"
					value={yearFromInput}
					onChange={(e) => setYearFromInput(e.target.value)}
					placeholder="Year from"
					min={1}
					max={9999}
					className={`w-24 ${selectClass} placeholder-gray-500`}
				/>
				<input
					type="number"
					aria-label="Year to"
					value={yearToInput}
					onChange={(e) => setYearToInput(e.target.value)}
					placeholder="Year to"
					min={1}
					max={9999}
					className={`w-24 ${selectClass} placeholder-gray-500`}
				/>
				<select
					aria-label="Minimum rating"
					value={minRating ?? ""}
					onChange={(e) => {
						const value = coerceInt(e.target.value, 1, 10);
						navigate({
							search: (prev) => ({ ...prev, minRating: value }),
							replace: true,
						});
					}}
					className={selectClass}
				>
					<option value="">Any rating</option>
					{MIN_RATING_OPTIONS.map((rating) => (
						<option key={rating} value={rating}>
							{rating}+
						</option>
					))}
				</select>
				<select
					aria-label="Sort by"
					value={sortBy ?? ""}
					onChange={(e) => {
						const value = SORT_BY_OPTIONS.includes(e.target.value as SortBy)
							? (e.target.value as SortBy)
							: undefined;
						navigate({
							search: (prev) => ({ ...prev, sortBy: value }),
							replace: true,
						});
					}}
					className={selectClass}
				>
					<option value="">Default sort</option>
					<option value="title">Title</option>
					<option value="year">Year</option>
					<option value="rating">Rating</option>
				</select>
				<button
					type="button"
					aria-label="Sort order"
					title={effectiveSortOrder === "asc" ? "Ascending" : "Descending"}
					onClick={() =>
						navigate({
							search: (prev) => ({
								...prev,
								sortOrder: effectiveSortOrder === "asc" ? "desc" : "asc",
							}),
							replace: true,
						})
					}
					className={`${selectClass} inline-flex items-center gap-1 hover:bg-gray-700 hover:text-white`}
				>
					{effectiveSortOrder === "asc" ? (
						<ArrowUp size={12} />
					) : (
						<ArrowDown size={12} />
					)}
					{effectiveSortOrder === "asc" ? "Asc" : "Desc"}
				</button>
				{hasActiveFilters && (
					<button
						type="button"
						onClick={clearFilters}
						className="inline-flex items-center gap-1 rounded-md px-2 py-1.5 text-xs font-medium text-gray-400 transition-colors hover:bg-gray-800 hover:text-white"
					>
						<X size={12} />
						Clear filters
					</button>
				)}
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
						: hasActiveFilters
							? "No media matches the current filters."
							: "No media indexed yet. Create a library and scan it from the Libraries page."}
				</p>
			) : (
				<>
					<ul className="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">
						{edges.map((edge) => {
							const node = edge.node;
							const isMovie = "Movie" in node;
							const media = isMovie ? node.Movie : node.Show;
							const title = media.title.original;
							const year = media.year ?? null;
							// A show's first season can lack art (specials,
							// season-less shows), so take the first season that
							// actually has a poster.
							const poster = artworkSrc(
								isMovie
									? node.Movie.poster_url
									: (node.Show.seasons.find((s) => s.poster_url)?.poster_url ??
											null),
							);
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
					{hasNextPage && (
						<div className="mt-8 flex justify-center">
							<button
								type="button"
								onClick={() => fetchNextPage()}
								disabled={isFetchingNextPage}
								className="inline-flex items-center gap-2 rounded-md border border-gray-700 bg-gray-800 px-4 py-2 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-700 hover:text-white disabled:cursor-not-allowed disabled:opacity-60"
							>
								{isFetchingNextPage ? (
									<>
										<Loader2 className="animate-spin" size={16} />
										Loading...
									</>
								) : (
									"Load more"
								)}
							</button>
						</div>
					)}
				</>
			)}
		</div>
	);
}
