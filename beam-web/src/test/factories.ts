import type { components } from "@/api.gen";
import type { Connection, PageInfo } from "@/lib/apiClient";

/**
 * Test data builders, every one typed as the schema the server actually
 * publishes.
 *
 * The point of the type annotation is not convenience: an untyped object
 * literal standing in for a response can drift from the spec -- a renamed
 * field, a nullable that became required -- with the whole suite still green,
 * because nothing ever compared the double to the contract. These are checked
 * by `tsc` against `api.gen.ts`, which is generated from the same OpenAPI
 * document the server serves, so a breaking backend change fails typecheck.
 */
type Schemas = components["schemas"];

export type User = Schemas["CurrentUser"];
export type Library = Schemas["Library"];
export type LibraryFile = Schemas["LibraryFile"];
export type MediaMetadata = Schemas["MediaMetadata"];
export type MovieMetadata = Schemas["MovieMetadata"];
export type ShowMetadata = Schemas["ShowMetadata"];
export type MediaSource = Schemas["MediaSource"];
export type HistoryItem = Schemas["HistoryItem"];
export type ContinueWatchingItem = Schemas["ContinueWatchingItem"];
export type AdminUser = Schemas["AdminUser"];
export type AdminLogEntry = Schemas["AdminLogEntry"];
export type AdminEvent = Schemas["AdminEvent"];
export type AdminStatus = Schemas["AdminStatus"];
export type SessionSummary = Schemas["SessionSummary"];

export type SeasonMetadata = Schemas["SeasonMetadata"];
export type EpisodeMetadata = Schemas["EpisodeMetadata"];
export type UserTitleState = Schemas["UserTitleState"];
export type ScanJob = Schemas["ScanJob"];

/**
 * A `/v1` list response: the items as one page, the whole list unless
 * `page_info` says a page follows. Every connection schema in `api.gen.ts` is
 * this shape, so the result is assignable to whichever one a handler names.
 */
export function connection<T>(
	items: T[],
	pageInfo: Partial<PageInfo> = {},
): Connection<T> {
	return {
		items,
		page_info: {
			has_next_page: false,
			has_previous_page: false,
			start_cursor: null,
			end_cursor: null,
			...pageInfo,
		},
	};
}

/**
 * A connection that also reports `total`, the size of the whole list (the
 * history, admin users, admin logs and enrichment lists). It defaults to the
 * page's own length, which is right whenever no further page follows.
 */
export function countedConnection<T>(
	items: T[],
	pageInfo: Partial<PageInfo> = {},
	total: number = items.length,
): Connection<T> & { total: number } {
	return { ...connection(items, pageInfo), total };
}

export function user(overrides: Partial<User> = {}): User {
	return {
		id: "user-1",
		display_name: "Test User",
		email: "test@example.com",
		is_admin: false,
		avatar_url: null,
		...overrides,
	};
}

export function adminUser(overrides: Partial<AdminUser> = {}): AdminUser {
	return {
		id: "user-1",
		display_name: "Test User",
		email: "test@example.com",
		is_admin: false,
		disabled: false,
		avatar_url: null,
		created_at: "2026-01-01T00:00:00Z",
		...overrides,
	};
}

export function library(overrides: Partial<Library> = {}): Library {
	return {
		id: "11111111-1111-1111-1111-111111111111",
		name: "Movies",
		description: null,
		file_count: 0,
		last_scan_started_at: null,
		last_scan_finished_at: null,
		last_scan_file_count: null,
		...overrides,
	};
}

/** A viewer who has never played the title. */
export function userTitleState(
	overrides: Partial<UserTitleState> = {},
): UserTitleState {
	return {
		played: false,
		position_secs: 0,
		duration_secs: null,
		play_count: 0,
		last_played_at: null,
		last_file_id: null,
		...overrides,
	};
}

export function movie(overrides: Partial<MovieMetadata> = {}): MovieMetadata {
	return {
		id: "22222222-2222-2222-2222-222222222222",
		title: { original: "Arrival", localized: null, alternatives: [] },
		genres: [],
		description: null,
		year: 2016,
		release_date: null,
		runtime_mins: null,
		duration_secs: null,
		poster_url: null,
		backdrop_url: null,
		file_id: null,
		source_count: null,
		identifiers: null,
		ratings: null,
		user_state: userTitleState(),
		...overrides,
	};
}

export function episode(
	overrides: Partial<EpisodeMetadata> = {},
): EpisodeMetadata {
	return {
		id: "55555555-5555-5555-5555-555555555555",
		episode_number: 1,
		title: "Good News About Hell",
		description: null,
		air_date: null,
		thumbnail_url: null,
		duration_secs: null,
		file_id: null,
		source_count: 0,
		user_state: userTitleState(),
		...overrides,
	};
}

export function season(
	overrides: Partial<SeasonMetadata> = {},
): SeasonMetadata {
	return {
		id: "66666666-6666-6666-6666-666666666666",
		season_number: 1,
		dates: { first_aired: null, last_aired: null },
		episode_runtime_mins: null,
		episodes: [],
		poster_url: null,
		genres: [],
		ratings: null,
		identifiers: null,
		user_state: null,
		...overrides,
	};
}

/**
 * A show. `season_count` and `episode_count` are derived from `seasons` unless
 * an override names them, so a test that hands in seasons gets counts that
 * agree with them.
 */
export function show(overrides: Partial<ShowMetadata> = {}): ShowMetadata {
	const seasons = overrides.seasons ?? [];
	return {
		id: "33333333-3333-3333-3333-333333333333",
		title: { original: "Severance", localized: null, alternatives: [] },
		description: null,
		year: 2022,
		poster_url: null,
		backdrop_url: null,
		genres: [],
		ratings: null,
		identifiers: null,
		season_count: seasons.length,
		episode_count: seasons.reduce((n, s) => n + s.episodes.length, 0),
		seasons,
		user_state: null,
		...overrides,
	};
}

/** A source with one file, no tracks, and the URLs the server derives. */
export function mediaSource(overrides: Partial<MediaSource> = {}): MediaSource {
	const fileId = overrides.file_id ?? "44444444-4444-4444-4444-444444444444";
	return {
		file_id: fileId,
		is_primary: true,
		edition: null,
		episode_span: null,
		disc_structure: null,
		parts: [],
		size_bytes: 1_000_000,
		mime_type: "video/mp4",
		container_format: "mp4",
		duration_secs: null,
		video_tracks: [],
		audio_tracks: [],
		subtitle_tracks: [],
		stream_url: `/v1/files/${fileId}/stream`,
		download_url: `/v1/files/${fileId}/download`,
		...overrides,
	};
}

export function historyItem(overrides: Partial<HistoryItem> = {}): HistoryItem {
	return {
		file_id: "44444444-4444-4444-4444-444444444444",
		media_id: "22222222-2222-2222-2222-222222222222",
		media_type: "movie",
		episode_id: null,
		position_secs: 600,
		duration_secs: 6000,
		played: false,
		play_count: 0,
		last_played_at: "2026-01-01T00:00:00Z",
		title: "Arrival",
		poster_url: null,
		backdrop_url: null,
		season_number: null,
		episode_number: null,
		episode_title: null,
		thumbnail_url: null,
		...overrides,
	};
}

export function continueWatchingItem(
	overrides: Partial<ContinueWatchingItem> = {},
): ContinueWatchingItem {
	return {
		reason: "resume",
		file_id: "44444444-4444-4444-4444-444444444444",
		media_id: "22222222-2222-2222-2222-222222222222",
		media_type: "movie",
		episode_id: null,
		position_secs: 600,
		duration_secs: 6000,
		last_played_at: "2026-01-01T00:00:00Z",
		title: "Arrival",
		poster_url: null,
		backdrop_url: null,
		season_number: null,
		episode_number: null,
		episode_title: null,
		thumbnail_url: null,
		...overrides,
	};
}

export function adminStatus(overrides: Partial<AdminStatus> = {}): AdminStatus {
	return {
		uptime_secs: 3600,
		version: "0.1.0",
		counts: { users: 1, libraries: 1, files: 0 },
		enrichment: { pending: 0, enriched: 0, unmatched: 0, failed: 0 },
		enrichment_providers: [],
		metadata_providers: [],
		recent_scans: [],
		watcher: {
			enabled: true,
			watch_limit_reached: false,
			watch_limit_count: null,
			libraries: [],
		},
		...overrides,
	};
}

/** A manual scan the server has just queued. */
export function scanJob(overrides: Partial<ScanJob> = {}): ScanJob {
	return {
		id: "77777777-7777-7777-7777-777777777777",
		library_id: "11111111-1111-1111-1111-111111111111",
		trigger: "manual",
		state: "queued",
		queued_at: "2026-01-01T00:00:00Z",
		started_at: null,
		finished_at: null,
		progress: {
			total_count: null,
			processed_count: 0,
			added_count: 0,
			changed_count: 0,
			unchanged_count: 0,
			deferred_count: 0,
			failed_count: 0,
			marked_missing_count: 0,
			restored_count: 0,
			relinked_count: 0,
			purged_count: 0,
		},
		failure: null,
		...overrides,
	};
}
