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

/**
 * A `/v1` list response: the items as one page, the whole list unless
 * `page_info` says a page follows. Typed by hand, not from `api.gen.ts`, which
 * predates the envelope (TODO(#146)).
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

export function movie(overrides: Partial<MovieMetadata> = {}): MovieMetadata {
	return {
		id: "22222222-2222-2222-2222-222222222222",
		title: { original: "Arrival", localized: null, alternatives: [] },
		genres: [],
		streams: [],
		description: null,
		year: 2016,
		release_date: null,
		runtime: null,
		duration: null,
		poster_url: null,
		backdrop_url: null,
		file_id: null,
		identifiers: null,
		ratings: null,
		...overrides,
	};
}

export function show(overrides: Partial<ShowMetadata> = {}): ShowMetadata {
	return {
		id: "33333333-3333-3333-3333-333333333333",
		title: { original: "Severance", localized: null, alternatives: [] },
		seasons: [],
		description: null,
		year: 2022,
		...overrides,
	};
}

export function historyItem(overrides: Partial<HistoryItem> = {}): HistoryItem {
	return {
		file_id: "44444444-4444-4444-4444-444444444444",
		media_id: "22222222-2222-2222-2222-222222222222",
		media_type: "movie",
		position_secs: 600,
		duration_secs: 6000,
		completed: false,
		episode_id: null,
		updated_at: "2026-01-01T00:00:00Z",
		...overrides,
	};
}

export function continueWatchingItem(
	overrides: Partial<ContinueWatchingItem> = {},
): ContinueWatchingItem {
	return {
		file_id: "44444444-4444-4444-4444-444444444444",
		media_id: "22222222-2222-2222-2222-222222222222",
		media_type: "movie",
		position_secs: 600,
		duration_secs: 6000,
		episode_id: null,
		updated_at: "2026-01-01T00:00:00Z",
		...overrides,
	};
}
