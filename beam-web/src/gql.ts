/**
 * Stub for GraphQL-generated types.
 *
 * This file is checked into source control as a placeholder so that
 * `tsc --noEmit` can run in CI without requiring the codegen step.
 *
 * Run `bun run codegen:full` to regenerate this file from the live schema.
 * The generated output will replace this stub entirely.
 */

// ── Enums ──────────────────────────────────────────────────────────────────

export enum FileIndexStatus {
	Known = "KNOWN",
	Changed = "CHANGED",
	Unknown = "UNKNOWN",
}

export enum FileContentType {
	Movie = "MOVIE",
	Episode = "EPISODE",
	Unclassified = "UNCLASSIFIED",
}

export enum MediaSortField {
	Title = "TITLE",
	CreatedAt = "CREATED_AT",
	UpdatedAt = "UPDATED_AT",
}

export enum SortOrder {
	Asc = "ASC",
	Desc = "DESC",
}

// ── Domain types ───────────────────────────────────────────────────────────

export interface Library {
	id: string;
	name: string;
	description?: string | null;
	size: number;
	lastScanStartedAt?: string | null;
	lastScanFinishedAt?: string | null;
	lastScanFileCount?: number | null;
	[key: string]: unknown;
}

export interface LibraryFile {
	id: string;
	path: string;
	sizeBytes: number;
	status: FileIndexStatus;
	contentType: FileContentType;
	updatedAt: string;
	mimeType?: string | null;
	containerFormat?: string | null;
	[key: string]: unknown;
}

// ── Root types ─────────────────────────────────────────────────────────────

export interface QueryRoot {
	library?: {
		libraries: Library[];
		libraryById?: Library | null;
		libraryFiles: LibraryFile[];
		[key: string]: unknown;
	} | null;
	media?: {
		metadata?: {
			title: { original: string; [key: string]: unknown };
			description?: string | null;
			[key: string]: unknown;
		} | null;
		[key: string]: unknown;
	} | null;
	[key: string]: unknown;
}

export interface MutationRoot {
	[key: string]: unknown;
}

// ── Mutation argument types ────────────────────────────────────────────────

export interface LibraryMutationCreateLibraryArgs {
	[key: string]: unknown;
}
export interface LibraryMutationScanLibraryArgs {
	[key: string]: unknown;
}
export interface LibraryMutationDeleteLibraryArgs {
	[key: string]: unknown;
}

// ── Query/mutation operation types ────────────────────────────────────────

export interface SearchMediaQuery {
	[key: string]: unknown;
}

export interface SearchMediaQueryVariables {
	first?: number | null;
	after?: string | null;
	query?: string | null;
	sortBy?: MediaSortField | null;
	sortOrder?: SortOrder | null;
}

export interface GetMediaMetadataByIdQuery {
	media: {
		metadata?: {
			title: { original: string; [key: string]: unknown };
			description?: string | null;
			[key: string]: unknown;
		} | null;
		[key: string]: unknown;
	};
}

export interface GetMediaMetadataByIdQueryVariables {
	mediaId: string;
}
