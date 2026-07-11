import { describe, expect, it } from "vitest";
import { nextPlayableEpisode, type UpNextSeason } from "./upNext";

function ep(id: string, episodeNumber: number, fileId: string | null) {
	return { id, episode_number: episodeNumber, file_id: fileId };
}

/** S1: e1 (file), e2 (file), e3 (no file) · S2: e4 (no file), e5 (file) */
const seasons: UpNextSeason[] = [
	{
		season_number: 1,
		episodes: [ep("e1", 1, "f1"), ep("e2", 2, "f2"), ep("e3", 3, null)],
	},
	{
		season_number: 2,
		episodes: [ep("e4", 1, null), ep("e5", 2, "f5")],
	},
];

describe("nextPlayableEpisode", () => {
	it("returns the next episode in the same season", () => {
		expect(nextPlayableEpisode(seasons, "e1")?.id).toBe("e2");
	});

	it("skips file-less episodes, crossing the season boundary", () => {
		// After e2 comes e3 (no file) and e4 (no file) -- both skipped.
		expect(nextPlayableEpisode(seasons, "e2")?.id).toBe("e5");
	});

	it("advances from a file-less current episode too", () => {
		expect(nextPlayableEpisode(seasons, "e3")?.id).toBe("e5");
	});

	it("returns null after the last playable episode", () => {
		expect(nextPlayableEpisode(seasons, "e5")).toBeNull();
	});

	it("returns null when only file-less episodes remain", () => {
		const tail: UpNextSeason[] = [
			{
				season_number: 1,
				episodes: [ep("e1", 1, "f1"), ep("e2", 2, null), ep("e3", 3, null)],
			},
		];
		expect(nextPlayableEpisode(tail, "e1")).toBeNull();
	});

	it("returns null when the current episode id is not found", () => {
		expect(nextPlayableEpisode(seasons, "missing")).toBeNull();
	});

	it("returns null for a null or undefined current episode id", () => {
		expect(nextPlayableEpisode(seasons, null)).toBeNull();
		expect(nextPlayableEpisode(seasons, undefined)).toBeNull();
	});

	it("returns null when there are no seasons", () => {
		expect(nextPlayableEpisode([], "e1")).toBeNull();
	});

	it("orders by season/episode number, not input order", () => {
		const shuffled: UpNextSeason[] = [
			{
				season_number: 2,
				episodes: [ep("s2e2", 2, "s2e2-file"), ep("s2e1", 1, "s2e1-file")],
			},
			{
				season_number: 1,
				episodes: [ep("s1e2", 2, "s1e2-file"), ep("s1e1", 1, "s1e1-file")],
			},
		];
		expect(nextPlayableEpisode(shuffled, "s1e1")?.id).toBe("s1e2");
		expect(nextPlayableEpisode(shuffled, "s1e2")?.id).toBe("s2e1");
		expect(nextPlayableEpisode(shuffled, "s2e1")?.id).toBe("s2e2");
		expect(nextPlayableEpisode(shuffled, "s2e2")).toBeNull();
	});

	it("does not treat an empty-string file_id as playable", () => {
		const empty: UpNextSeason[] = [
			{
				season_number: 1,
				episodes: [ep("e1", 1, "f1"), ep("e2", 2, "")],
			},
		];
		expect(nextPlayableEpisode(empty, "e1")).toBeNull();
	});
});
