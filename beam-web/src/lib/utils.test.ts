import { describe, expect, it } from "vitest";
import { formatDuration } from "./utils";

describe("formatDuration", () => {
	it("formats seconds under a minute as M:SS", () => {
		expect(formatDuration(45)).toBe("0:45");
	});

	it("formats minutes and seconds under an hour as M:SS", () => {
		expect(formatDuration(125)).toBe("2:05");
	});

	it("formats an hour or more as H:MM:SS", () => {
		expect(formatDuration(3725)).toBe("1:02:05");
	});

	it("clamps negative durations to zero", () => {
		expect(formatDuration(-10)).toBe("0:00");
	});

	it("rounds fractional seconds", () => {
		expect(formatDuration(59.6)).toBe("1:00");
	});
});
