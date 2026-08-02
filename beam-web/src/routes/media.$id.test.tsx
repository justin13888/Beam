import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/lib/apiClient", () => ({
	apiClient: {
		GET: vi.fn(),
		POST: vi.fn(),
	},
}));

vi.mock("@tanstack/react-router", () => ({
	createFileRoute: (_path: string) => (opts: Record<string, unknown>) => opts,
	ErrorComponent: () => null,
}));

vi.mock("@/hooks/auth", () => ({
	useAuth: () => ({ isAuthenticated: true, user: null }),
}));

// Vidstack pulls in player CSS/runtime that jsdom has no business loading --
// the player itself is not under test here.
vi.mock("@/components/VideoPlayer", () => ({
	VideoPlayer: () => null,
}));

import { EpisodeList } from "./media.$id";

const playableEpisode = {
	id: "ep-1",
	episode_number: 1,
	title: "Pilot",
	streams: [],
	file_id: "file-1",
};

const filelessEpisode = {
	id: "ep-2",
	episode_number: 2,
	title: "Lost Media",
	streams: [],
	file_id: null,
};

const show = {
	id: "show-1",
	title: { original: "Test Show" },
	seasons: [
		{
			season_number: 1,
			dates: {},
			genres: [],
			episodes: [playableEpisode, filelessEpisode],
		},
	],
};

describe("EpisodeList", () => {
	it("renders a file-less episode as a disabled row labelled 'No file'", () => {
		render(
			<EpisodeList
				show={show}
				activeEpisodeId={null}
				activeFileId={null}
				onSelect={vi.fn()}
			/>,
		);

		const row = screen.getByRole("button", { name: /Lost Media/ });
		expect(row).toBeDisabled();
		expect(row).toHaveTextContent("No file");
	});

	it("does not label playable episodes and reports the selected episode", async () => {
		const onSelect = vi.fn();
		const user = userEvent.setup();
		render(
			<EpisodeList
				show={show}
				activeEpisodeId={null}
				activeFileId={null}
				onSelect={onSelect}
			/>,
		);

		const row = screen.getByRole("button", { name: /Pilot/ });
		expect(row).toBeEnabled();
		expect(row).not.toHaveTextContent("No file");

		await user.click(row);
		expect(onSelect).toHaveBeenCalledTimes(1);
		expect(onSelect).toHaveBeenCalledWith(playableEpisode);
	});

	it("highlights the active episode by id", () => {
		render(
			<EpisodeList
				show={show}
				activeEpisodeId="ep-1"
				activeFileId={null}
				onSelect={vi.fn()}
			/>,
		);

		expect(screen.getByRole("button", { name: /Pilot/ })).toHaveClass(
			"bg-gray-800",
		);
	});
});
