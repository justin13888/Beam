import { describe, expect, it, vi } from "vitest";
import { type PlayerLike, playerHandlers } from "./VideoPlayer";

function player(currentTime = 0, duration = 100): PlayerLike {
	return { currentTime, duration };
}

describe("playerHandlers", () => {
	describe("canPlay", () => {
		it("seeks to the resume position", () => {
			const p = player();
			playerHandlers({ startTime: 42 }).canPlay(p);
			expect(p.currentTime).toBe(42);
		});

		it("does not seek when starting from the beginning", () => {
			const p = player(7);
			playerHandlers({ startTime: 0 }).canPlay(p);
			expect(p.currentTime).toBe(7);
		});

		it("does not seek for a negative resume position", () => {
			const p = player(7);
			playerHandlers({ startTime: -5 }).canPlay(p);
			expect(p.currentTime).toBe(7);
		});

		it("is a no-op before the player instance exists", () => {
			expect(() =>
				playerHandlers({ startTime: 42 }).canPlay(null),
			).not.toThrow();
		});
	});

	describe("timeUpdate", () => {
		it("reports the current position and duration", () => {
			const onProgress = vi.fn();
			playerHandlers({ onProgress }).timeUpdate(player(12.5, 3600));
			expect(onProgress).toHaveBeenCalledWith(12.5, 3600);
		});

		it("reports nothing when there is no player yet", () => {
			const onProgress = vi.fn();
			playerHandlers({ onProgress }).timeUpdate(null);
			expect(onProgress).not.toHaveBeenCalled();
		});

		it("is safe with no progress listener attached", () => {
			expect(() => playerHandlers({}).timeUpdate(player())).not.toThrow();
		});
	});

	describe("ended", () => {
		it("reports the final duration", () => {
			const onEnded = vi.fn();
			playerHandlers({ onEnded }).ended(player(3600, 3600));
			expect(onEnded).toHaveBeenCalledWith(3600);
		});

		it("reports zero when the duration never resolved", () => {
			const onEnded = vi.fn();
			playerHandlers({ onEnded }).ended(null);
			expect(onEnded).toHaveBeenCalledWith(0);
		});
	});
});
