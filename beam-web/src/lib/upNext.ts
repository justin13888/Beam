/**
 * Pure up-next resolution for series playback (auto-advance on player end).
 *
 * These are minimal structural shapes -- the generated `SeasonMetadata` /
 * `EpisodeMetadata` API types satisfy them, so callers can pass
 * `show.seasons` straight in while tests stay free of generated types.
 */
export interface UpNextEpisode {
	id: string;
	episode_number: number;
	/** Identifier of the streamable file backing this episode, if any. */
	file_id?: string | null;
}

export interface UpNextSeason<E extends UpNextEpisode = UpNextEpisode> {
	season_number: number;
	episodes: readonly E[];
}

/**
 * Resolve the episode to auto-advance to once `currentEpisodeId` finishes:
 * the next episode in season/episode order that has a playable file,
 * crossing season boundaries and skipping file-less episodes.
 *
 * Returns `null` when there is nothing to advance to: the current episode
 * is the last one, every later episode is file-less, or the current episode
 * id isn't found in `seasons` at all.
 */
export function nextPlayableEpisode<E extends UpNextEpisode>(
	seasons: readonly UpNextSeason<E>[],
	currentEpisodeId: string | null | undefined,
): E | null {
	if (!currentEpisodeId) return null;

	// Sort defensively rather than trusting input order -- the helper is
	// pure and cheap, and a mis-ordered payload must not skip episodes.
	const ordered = [...seasons]
		.sort((a, b) => a.season_number - b.season_number)
		.flatMap((season) =>
			[...season.episodes].sort((a, b) => a.episode_number - b.episode_number),
		);

	const currentIndex = ordered.findIndex((e) => e.id === currentEpisodeId);
	if (currentIndex === -1) return null;

	for (let i = currentIndex + 1; i < ordered.length; i++) {
		const candidate = ordered[i];
		if (candidate?.file_id) return candidate;
	}
	return null;
}
