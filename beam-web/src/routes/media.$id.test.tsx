import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { describe, expect, it, vi } from "vitest";
import type { components } from "@/api.gen";
import * as factory from "@/test/factories";
import { BASE_URL } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

// Vidstack needs a real media element, which jsdom does not provide. This is a
// stand-in for a *collaborator*, not for the subject: `VideoPlayer`'s own
// behaviour is tested directly in `src/components/VideoPlayer.test.ts`. The
// stub records the `src` so these tests can assert which stream the page chose.
vi.mock("@/components/VideoPlayer", () => ({
	VideoPlayer: (props: Record<string, unknown>) => (
		<div data-testid="video-player" data-src={props.src as string}>
			{props.title as string}
		</div>
	),
}));

const { EpisodeList } = await import("./media.$id");

type EpisodeMetadata =
	components["schemas"]["beam_server.models.media.show.EpisodeMetadata"];
type MediaSource =
	components["schemas"]["beam_server.models.media.source.MediaSource"];
type ShowMetadata =
	components["schemas"]["beam_server.models.media.show.ShowMetadata"];

const playableEpisode: EpisodeMetadata = {
	id: "ep-1",
	episode_number: 1,
	title: "Pilot",
	streams: [],
	file_id: "file-1",
};

const filelessEpisode: EpisodeMetadata = {
	id: "ep-2",
	episode_number: 2,
	title: "Lost Media",
	streams: [],
	file_id: null,
};

const show: ShowMetadata = factory.show({
	id: "show-1",
	title: { original: "Test Show", localized: null, alternatives: [] },
	seasons: [
		{
			season_number: 1,
			dates: {},
			genres: [],
			episodes: [playableEpisode, filelessEpisode],
		},
	],
});

const movie = factory.movie({
	id: "movie-1",
	title: { original: "Blade Runner", localized: null, alternatives: [] },
	year: 1982,
	description: "A blade runner must pursue replicants.",
	poster_url: "https://posters.test/blade-runner.jpg",
	file_id: "file-hd",
});

function source(fileId: string, height: number, codec: string): MediaSource {
	return {
		file_id: fileId,
		stream_url: `/v1/files/${fileId}/stream`,
		download_url: `/v1/files/${fileId}/download`,
		mime_type: "video/mp4",
		video: {
			height,
			codec,
			width: height * 2,
			bit_rate: null,
			hdr_format: null,
		},
		size_bytes: 1024,
		audio_tracks: [],
	};
}

/** Serve the media document and its sources. */
function serveMedia(
	metadata:
		| components["schemas"]["beam_server.models.media.MediaMetadata"]
		| null,
	sources: MediaSource[] = [],
) {
	server.use(
		http.get(`${BASE_URL}/v1/media/:id`, () =>
			metadata
				? HttpResponse.json(metadata)
				: HttpResponse.json(
						{ message: "Not found", code: "not_found" },
						{ status: 404 },
					),
		),
		http.get(`${BASE_URL}/v1/media/:id/sources`, () =>
			HttpResponse.json(sources),
		),
	);
}

function currentStreamSrc(): string | null {
	return screen.getByTestId("video-player").getAttribute("data-src");
}

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

describe("/media/$id (movie)", () => {
	it("loads the media document through the route loader and renders it", async () => {
		const requests = recordRequests();
		serveMedia({ Movie: movie }, [source("file-hd", 1080, "h264")]);
		renderRoute("/media/movie-1");

		expect(
			await screen.findByRole("heading", { name: "Blade Runner" }),
		).toBeInTheDocument();
		expect(screen.getByText("1982")).toBeInTheDocument();
		expect(
			screen.getByText("A blade runner must pursue replicants."),
		).toBeInTheDocument();
		// The loader fetched by id, not by some other path.
		expect(requests.matching("GET", "/v1/media/movie-1")).toHaveLength(1);
		// The player streams the movie's primary file.
		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-hd/stream",
			),
		);
	});

	it("shows the source picker only when 2+ sources exist", async () => {
		serveMedia({ Movie: movie }, [
			source("file-hd", 1080, "h264"),
			source("file-sd", 480, "h264"),
		]);
		renderRoute("/media/movie-1");

		const select = await screen.findByRole("combobox");
		const options = Array.from(select.querySelectorAll("option")).map(
			(o) => o.textContent,
		);
		expect(options).toEqual(["1080p · h264", "480p · h264"]);
	});

	it("hides the source picker when there is only one source", async () => {
		serveMedia({ Movie: movie }, [source("file-hd", 1080, "h264")]);
		renderRoute("/media/movie-1");

		await waitFor(() => expect(currentStreamSrc()).toContain("file-hd"));
		expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
	});

	it("switches the stream URL passed to the player when a source is selected", async () => {
		serveMedia({ Movie: movie }, [
			source("file-hd", 1080, "h264"),
			source("file-sd", 480, "h264"),
		]);
		const user = userEvent.setup();
		renderRoute("/media/movie-1");

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
		serveMedia({ Movie: movie }, [
			source("file-hd", 1080, "h264"),
			source("file-sd", 480, "h264"),
		]);
		renderRoute("/media/movie-1?fileId=file-sd");

		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-sd/stream",
			),
		);
	});

	it("ignores a non-string fileId rather than deep-linking to it", async () => {
		// `validateSearch` narrows the raw search object; a repeated parameter
		// arrives as an array and must fall back to the primary file.
		serveMedia({ Movie: movie }, [
			source("file-hd", 1080, "h264"),
			source("file-sd", 480, "h264"),
		]);
		renderRoute("/media/movie-1?fileId=a&fileId=b");

		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-hd/stream",
			),
		);
	});
});

describe("/media/$id (show)", () => {
	it("renders the season/episode structure and prompts to pick an episode", async () => {
		serveMedia({ Show: show });
		renderRoute("/media/show-1");

		expect(
			await screen.findByRole("heading", { name: "Test Show" }),
		).toBeInTheDocument();
		expect(screen.getByText("Season 1")).toBeInTheDocument();
		expect(screen.getByRole("button", { name: /Pilot/ })).toBeInTheDocument();
		// No episode picked yet -> no player, just a prompt.
		expect(screen.getByText("Select an episode to play.")).toBeInTheDocument();
		expect(screen.queryByTestId("video-player")).not.toBeInTheDocument();
	});

	it("fetches the episode's sources and plays it when an episode is selected", async () => {
		const requests = recordRequests();
		serveMedia({ Show: show }, [source("file-1", 720, "h264")]);
		const user = userEvent.setup();
		renderRoute("/media/show-1");

		await user.click(await screen.findByRole("button", { name: /Pilot/ }));

		// The sources query fires for the episode's playable id, not the show's.
		await waitFor(() =>
			expect(requests.matching("GET", "/v1/media/ep-1/sources")).toHaveLength(
				1,
			),
		);
		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-1/stream",
			),
		);
	});

	it("renders the route's error component when the media document fails to load", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/media/:id`, () =>
				HttpResponse.json(
					{ message: "boom", code: "internal" },
					{ status: 500 },
				),
			),
		);
		renderRoute("/media/movie-1");
		await waitForRouter();

		await waitFor(() =>
			expect(
				screen.getByText(/Failed to load media metadata/),
			).toBeInTheDocument(),
		);
	});
});
