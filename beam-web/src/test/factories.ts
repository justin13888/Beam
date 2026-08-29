import type { components } from "@/api.gen";

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

export type User = Schemas["beam_auth.server.oidc_routes.MeResponse"];
export type Library = Schemas["beam_server.models.library.Library"];
export type LibraryFile =
	Schemas["beam_server.models.library.file.LibraryFile"];
export type MediaMetadata = Schemas["beam_server.models.media.MediaMetadata"];
export type MovieMetadata =
	Schemas["beam_server.models.media.movie.MovieMetadata"];
export type ShowMetadata =
	Schemas["beam_server.models.media.show.ShowMetadata"];
export type MediaSource =
	Schemas["beam_server.models.media.source.MediaSource"];
export type HistoryItem = Schemas["beam_server.services.playback.HistoryItem"];
export type ContinueWatchingItem =
	Schemas["beam_server.services.playback.ContinueWatchingItem"];
export type AdminUser = Schemas["beam_server.models.admin.AdminUserDto"];
export type AdminLogEntry =
	Schemas["beam_server.models.admin.AdminLogEntryDto"];
export type AdminEvent = Schemas["beam_server.models.admin.AdminEventDto"];
export type AdminStatus =
	Schemas["beam_server.models.admin.AdminStatusResponse"];
export type SessionSummary =
	Schemas["beam_auth.server.oidc_routes.SessionSummary"];

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
		size: 0,
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
