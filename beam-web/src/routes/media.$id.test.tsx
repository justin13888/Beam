import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster } from "sonner";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockGet, mockPost, mockPut, videoPlayerCalls } = vi.hoisted(() => ({
	mockGet: vi.fn(),
	mockPost: vi.fn(),
	mockPut: vi.fn(),
	videoPlayerCalls: [] as Record<string, unknown>[],
}));

vi.mock("@/lib/apiClient", () => ({
	apiClient: {
		GET: mockGet,
		POST: mockPost,
		PUT: mockPut,
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
// the player itself is not under test here. The stub records the props each
// render so tests can assert which stream URL the page selected.
vi.mock("@/components/VideoPlayer", () => ({
	VideoPlayer: (props: Record<string, unknown>) => {
		videoPlayerCalls.push(props);
		return (
			<div data-testid="video-player" data-src={props.src as string}>
				{props.title as string}
			</div>
		);
	},
}));

import { EpisodeList, MediaDetailPage } from "./media.$id";

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

// ---------------------------------------------------------------------------
// Page-level tests: movie vs show rendering + source selection.
// ---------------------------------------------------------------------------

type Source = {
	file_id: string;
	stream_url: string;
	download_url: string;
	mime_type?: string | null;
	video?: { height: number; codec: string } | null;
};

function source(fileId: string, height: number, codec: string): Source {
	return {
		file_id: fileId,
		stream_url: `/v1/files/${fileId}/stream`,
		download_url: `/v1/files/${fileId}/download`,
		mime_type: "video/mp4",
		video: { height, codec },
	};
}

const movieMetadata = {
	Movie: {
		id: "movie-1",
		title: { original: "Blade Runner" },
		year: 1982,
		description: "A blade runner must pursue replicants.",
		poster_url: "https://posters.test/blade-runner.jpg",
		file_id: "file-hd",
		genres: [],
		streams: [],
	},
};

/** Routes mocked GET calls by path. `sources` is served per playable-id
 * request; `continueWatching` backs the resume banner. */
function mockApi({
	sources = [] as Source[],
	continueWatching = [] as unknown[],
} = {}) {
	mockGet.mockImplementation(async (path: string) => {
		switch (path) {
			case "/v1/media/{id}/sources":
				return { data: sources, error: undefined, response: { status: 200 } };
			case "/v1/continue-watching":
				return { data: continueWatching, error: undefined };
			default:
				return {
					data: undefined,
					error: { message: `unexpected ${path}` },
					response: { status: 200 },
				};
		}
	});
}

function renderPage(
	metadata: unknown,
	fileIdParam: string | undefined = undefined,
) {
	const queryClient = new QueryClient({
		defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
	});
	return render(
		<QueryClientProvider client={queryClient}>
			{/* biome-ignore lint/suspicious/noExplicitAny: test passes structural fixtures */}
			<MediaDetailPage metadata={metadata as any} fileIdParam={fileIdParam} />
			<Toaster />
		</QueryClientProvider>,
	);
}

function currentStreamSrc(): string | null {
	return screen.getByTestId("video-player").getAttribute("data-src");
}

describe("MediaDetailPage (movie)", () => {
	beforeEach(() => {
		mockGet.mockReset();
		videoPlayerCalls.length = 0;
	});

	it("renders the movie title, year, and description", async () => {
		mockApi({ sources: [source("file-hd", 1080, "h264")] });
		renderPage(movieMetadata);

		expect(
			screen.getByRole("heading", { name: "Blade Runner" }),
		).toBeInTheDocument();
		expect(screen.getByText("1982")).toBeInTheDocument();
		expect(
			screen.getByText("A blade runner must pursue replicants."),
		).toBeInTheDocument();
		// The player streams the movie's primary file.
		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-hd/stream",
			),
		);
	});

	it("shows the source picker only when 2+ sources exist", async () => {
		mockApi({
			sources: [
				source("file-hd", 1080, "h264"),
				source("file-sd", 480, "h264"),
			],
		});
		renderPage(movieMetadata);

		// Two quality labels means a <select> with two <option>s.
		const select = await screen.findByRole("combobox");
		const options = Array.from(select.querySelectorAll("option")).map(
			(o) => o.textContent,
		);
		expect(options).toEqual(["1080p · h264", "480p · h264"]);
	});

	it("hides the source picker when there is only one source", async () => {
		mockApi({ sources: [source("file-hd", 1080, "h264")] });
		renderPage(movieMetadata);

		await waitFor(() => expect(currentStreamSrc()).toContain("file-hd"));
		expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
	});

	it("switches the stream URL passed to the player when a source is selected", async () => {
		mockApi({
			sources: [
				source("file-hd", 1080, "h264"),
				source("file-sd", 480, "h264"),
			],
		});
		const user = userEvent.setup();
		renderPage(movieMetadata);

		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-hd/stream",
			),
		);

		await user.selectOptions(await screen.findByRole("combobox"), "file-sd");

		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-sd/stream",
			),
		);
	});

	it("deep-links to the file named by ?fileId= instead of the primary file", async () => {
		mockApi({
			sources: [
				source("file-hd", 1080, "h264"),
				source("file-sd", 480, "h264"),
			],
		});
		renderPage(movieMetadata, "file-sd");

		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-sd/stream",
			),
		);
	});
});

describe("MediaDetailPage (show)", () => {
	beforeEach(() => {
		mockGet.mockReset();
		videoPlayerCalls.length = 0;
	});

	it("renders the season/episode structure and prompts to pick an episode", async () => {
		mockApi();
		renderPage({ Show: show });

		expect(
			screen.getByRole("heading", { name: "Test Show" }),
		).toBeInTheDocument();
		expect(screen.getByText("Season 1")).toBeInTheDocument();
		expect(screen.getByRole("button", { name: /Pilot/ })).toBeInTheDocument();
		// No episode picked yet -> no player, just a prompt.
		expect(screen.getByText("Select an episode to play.")).toBeInTheDocument();
		expect(screen.queryByTestId("video-player")).not.toBeInTheDocument();
	});

	it("fetches the episode's sources and plays it when an episode is selected", async () => {
		mockApi({ sources: [source("file-1", 720, "h264")] });
		const user = userEvent.setup();
		renderPage({ Show: show });

		await user.click(screen.getByRole("button", { name: /Pilot/ }));

		// The sources query fires for the episode's playable id.
		await waitFor(() =>
			expect(mockGet).toHaveBeenCalledWith(
				"/v1/media/{id}/sources",
				expect.objectContaining({ params: { path: { id: "ep-1" } } }),
			),
		);
		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-1/stream",
			),
		);
	});
});
