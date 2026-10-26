import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { components } from "@/api.gen";
import * as factory from "@/test/factories";
import { BASE_URL } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { problem } from "@/test/problem";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

// Vidstack needs a real media element, which jsdom does not provide. This is a
// stand-in for a *collaborator*, not for the subject: `VideoPlayer`'s own
// behaviour is tested directly in `src/components/VideoPlayer.test.ts`. The
// stub records the `src` so these tests can assert which stream the page chose,
// and offers the end of playback as a button, which is the event the page
// reacts to.
vi.mock("@/components/VideoPlayer", () => ({
	VideoPlayer: (props: Record<string, unknown>) => (
		<div
			data-testid="video-player"
			data-src={props.src as string}
			data-autoplay={String(props.autoPlay)}
			data-start={String(props.startTime)}
		>
			{props.title as string}
			<button
				type="button"
				onClick={() => (props.onEnded as (duration: number) => void)(100)}
			>
				Finish playback
			</button>
			<button
				type="button"
				onClick={() =>
					(props.onProgress as (time: number, duration: number) => void)(5, 100)
				}
			>
				Advance playback
			</button>
		</div>
	),
}));

/**
 * What this "browser" decodes. jsdom's `canPlayType` answers "" for every
 * type, so a page that asks the browser would find nothing playable; this
 * fills the same environment gap `setup.ts` fills for `matchMedia`, answering
 * for the containers and codec-string prefixes given.
 */
function browserDecodes({
	containers,
	codecs,
}: {
	containers: string[];
	codecs: string[];
}) {
	vi.spyOn(HTMLMediaElement.prototype, "canPlayType").mockImplementation(
		(type: string) => {
			const [mime, params = ""] = type.split(";");
			const listed =
				/codecs="([^"]*)"/
					.exec(params)?.[1]
					.split(",")
					.map((c) => c.trim()) ?? [];
			const decodes =
				containers.includes(mime.trim()) &&
				listed.every((c) => codecs.some((prefix) => c.startsWith(prefix)));
			return decodes ? "probably" : "";
		},
	);
}

beforeEach(() => {
	browserDecodes({ containers: ["video/mp4"], codecs: ["avc1", "mp4a"] });
});

afterEach(() => {
	vi.restoreAllMocks();
});

const { EpisodeList } = await import("./media.$id");

type EpisodeMetadata = components["schemas"]["EpisodeMetadata"];
type MediaSource = components["schemas"]["MediaSource"];
type ShowMetadata = components["schemas"]["ShowMetadata"];

const playableEpisode: EpisodeMetadata = factory.episode({
	id: "ep-1",
	episode_number: 1,
	title: "Pilot",
	file_id: "file-1",
	source_count: 1,
});

const filelessEpisode: EpisodeMetadata = factory.episode({
	id: "ep-2",
	episode_number: 2,
	title: "Lost Media",
	file_id: null,
});

const show: ShowMetadata = factory.show({
	id: "show-1",
	title: { original: "Test Show", localized: null, alternatives: [] },
	seasons: [
		factory.season({
			season_number: 1,
			episodes: [playableEpisode, filelessEpisode],
		}),
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
	return factory.mediaSource({
		file_id: fileId,
		size_bytes: 1024,
		video_tracks: [
			{
				index: 0,
				codec,
				width: height * 2,
				height,
				frame_rate: 24,
				bit_rate: 5_000_000,
				hdr_format: null,
			},
		],
	});
}

/** Serve the media document and its sources. */
function serveMedia(
	metadata: components["schemas"]["MediaMetadata"] | null,
	sources: MediaSource[] = [],
) {
	server.use(
		http.get(`${BASE_URL}/v1/media/:id`, () =>
			metadata
				? HttpResponse.json(metadata)
				: problem(404, "Not found", "#media-not-found"),
		),
		http.get(`${BASE_URL}/v1/media/:id/sources`, () =>
			HttpResponse.json<components["schemas"]["MediaSourceConnection"]>(
				factory.connection(sources),
			),
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

describe("/media/$id (browser capability)", () => {
	it("plays the best source this browser decodes, not the first one listed", async () => {
		// The server lists the 4K HEVC primary first; this browser has no HEVC.
		serveMedia({ Movie: movie }, [
			source("file-4k", 2160, "hevc"),
			source("file-hd", 1080, "h264"),
			source("file-sd", 480, "h264"),
		]);
		renderRoute("/media/movie-1");

		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-hd/stream",
			),
		);
		// The unplayable source stays in the picker, disabled, with its reason.
		const select = await screen.findByRole("combobox");
		expect(select).toHaveValue("file-hd");
		const hevc = screen.getByRole("option", { name: /2160p · hevc/ });
		expect(hevc).toBeDisabled();
		expect(hevc).toHaveTextContent("This browser cannot decode HEVC video");
		expect(screen.getByRole("option", { name: "480p · h264" })).toBeEnabled();
	});

	it("does not follow a deep link to a source this browser cannot play", async () => {
		serveMedia({ Movie: movie }, [
			source("file-4k", 2160, "hevc"),
			source("file-hd", 1080, "h264"),
		]);
		renderRoute("/media/movie-1?fileId=file-4k");

		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-hd/stream",
			),
		);
	});

	it("passes over a transport-stream source for one the browser opens", async () => {
		serveMedia({ Movie: movie }, [
			{ ...source("file-m2ts", 1080, "h264"), container_format: "mpegts" },
			source("file-mp4", 720, "h264"),
		]);
		renderRoute("/media/movie-1");

		await waitFor(() => expect(currentStreamSrc()).toContain("file-mp4"));
		expect(
			screen.getByRole("option", { name: /1080p · h264/ }),
		).toHaveTextContent("Browsers cannot open MPEG transport streams");
	});

	it("says a disc-only title cannot play here, with the reason, and offers the download", async () => {
		serveMedia({ Movie: movie }, [
			{
				...source("file-vob", 480, "mpeg2video"),
				container_format: "mpeg",
				disc_structure: "dvd",
			},
		]);
		renderRoute("/media/movie-1");

		expect(
			await screen.findByRole("heading", {
				name: "This title can't play in this browser",
			}),
		).toBeInTheDocument();
		expect(screen.queryByTestId("video-player")).not.toBeInTheDocument();
		expect(screen.getByText(/a DVD folder rip/)).toBeInTheDocument();
		expect(
			screen.getByRole("link", { name: /Download to play it/ }),
		).toHaveAttribute(
			"href",
			"http://localhost:8000/v1/files/file-vob/download",
		);
	});
});

describe("/media/$id (multi-part)", () => {
	function part(fileId: string, partNumber: number) {
		return {
			file_id: fileId,
			part_number: partNumber,
			size_bytes: 512,
			duration_secs: 100,
			subtitle_tracks: [],
			stream_url: `/v1/files/${fileId}/stream`,
			download_url: `/v1/files/${fileId}/download`,
		};
	}

	const twoParts: MediaSource = {
		...source("cd1", 480, "h264"),
		parts: [part("cd1", 1), part("cd2", 2)],
	};

	it("plays the parts in order, advancing when one ends", async () => {
		const requests = recordRequests();
		serveMedia({ Movie: movie }, [twoParts]);
		const user = userEvent.setup();
		renderRoute("/media/movie-1");

		await waitFor(() => expect(currentStreamSrc()).toContain("/cd1/stream"));
		expect(screen.getByText("Part 1 of 2")).toBeInTheDocument();

		await user.click(screen.getByRole("button", { name: "Finish playback" }));

		await waitFor(() => expect(currentStreamSrc()).toContain("/cd2/stream"));
		const player = screen.getByTestId("video-player");
		expect(player).toHaveAttribute("data-autoplay", "true");
		expect(player).toHaveAttribute("data-start", "0");
		expect(screen.getByText("Part 2 of 2")).toBeInTheDocument();
		// The end of an earlier part marks nothing played.
		expect(requests.matching("PUT", "/v1/files/cd1/progress")).toHaveLength(0);

		// The end of the last part is reported, against the last part's file.
		await user.click(screen.getByRole("button", { name: "Finish playback" }));
		await waitFor(() =>
			expect(requests.matching("PUT", "/v1/files/cd2/progress")).toHaveLength(
				1,
			),
		);
		expect(currentStreamSrc()).toContain("/cd2/stream");
	});

	it("resumes the part that was playing, at its position", async () => {
		serveMedia({ Movie: movie }, [twoParts]);
		server.use(
			http.get(`${BASE_URL}/v1/continue-watching`, () =>
				HttpResponse.json<components["schemas"]["ContinueWatchingConnection"]>(
					factory.connection([
						factory.continueWatchingItem({ file_id: "cd2", position_secs: 42 }),
					]),
				),
			),
		);
		renderRoute("/media/movie-1");

		await waitFor(() => expect(currentStreamSrc()).toContain("/cd2/stream"));
		expect(screen.getByTestId("video-player")).toHaveAttribute(
			"data-start",
			"42",
		);
	});

	it("starts at the part a deep link names when no saved progress names one", async () => {
		serveMedia({ Movie: movie }, [twoParts]);
		server.use(
			http.get(`${BASE_URL}/v1/continue-watching`, () =>
				HttpResponse.json<components["schemas"]["ContinueWatchingConnection"]>(
					factory.connection([]),
				),
			),
		);
		renderRoute("/media/movie-1?fileId=cd2");

		await waitFor(() => expect(currentStreamSrc()).toContain("/cd2/stream"));
		expect(screen.getByTestId("video-player")).toHaveAttribute(
			"data-start",
			"0",
		);
		expect(screen.getByText("Part 2 of 2")).toBeInTheDocument();
	});

	it("starts at the deep-linked part when the progress lookup fails", async () => {
		serveMedia({ Movie: movie }, [twoParts]);
		server.use(
			http.get(`${BASE_URL}/v1/continue-watching`, () =>
				problem(500, "boom", "#internal"),
			),
		);
		renderRoute("/media/movie-1?fileId=cd2");

		await waitFor(() => expect(currentStreamSrc()).toContain("/cd2/stream"));
		expect(screen.getByText("Part 2 of 2")).toBeInTheDocument();
	});
});

describe("/media/$id (sources fail to load)", () => {
	it("says the sources failed to load and retries on request", async () => {
		serveMedia({ Movie: movie });
		let fail = true;
		server.use(
			http.get(`${BASE_URL}/v1/media/:id/sources`, () =>
				fail
					? problem(500, "Sources unavailable", "#internal")
					: HttpResponse.json<components["schemas"]["MediaSourceConnection"]>(
							factory.connection([source("file-hd", 1080, "h264")]),
						),
			),
		);
		const user = userEvent.setup();
		renderRoute("/media/movie-1");

		const retry = await screen.findByRole("button", { name: "Retry" });
		// The failure is stated, not an empty page, and the file is not
		// streamed blind.
		expect(
			screen.getByText("Failed to load media sources"),
		).toBeInTheDocument();
		expect(screen.queryByTestId("video-player")).not.toBeInTheDocument();
		expect(screen.queryByText("No streamable file.")).not.toBeInTheDocument();

		fail = false;
		await user.click(retry);

		await waitFor(() =>
			expect(currentStreamSrc()).toBe(
				"http://localhost:8000/v1/files/file-hd/stream",
			),
		);
		expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
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
				problem(500, "boom", "#internal"),
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

describe("/media/$id (decoding quality)", () => {
	type DecodingInfo = (
		configuration: MediaDecodingConfiguration,
	) => Promise<MediaCapabilitiesDecodingInfo>;

	/** Give jsdom the `navigator.mediaCapabilities` it lacks, answering with
	 * `decodingInfo`. Only these tests have it; every other test runs as the
	 * browsers without the API do. */
	function browserReports(decodingInfo: DecodingInfo) {
		Object.defineProperty(navigator, "mediaCapabilities", {
			configurable: true,
			value: { decodingInfo },
		});
	}

	/** Pictures up to `maxHeight` decode smoothly and power-efficiently. */
	function hardwareUpTo(maxHeight: number): DecodingInfo {
		return async (configuration) => {
			const hardware = (configuration.video?.height ?? 0) <= maxHeight;
			return {
				supported: true,
				smooth: hardware,
				powerEfficient: hardware,
				keySystemAccess: null,
			} as MediaCapabilitiesDecodingInfo;
		};
	}

	afterEach(() => {
		Reflect.deleteProperty(navigator, "mediaCapabilities");
	});

	it("prefers a source the browser decodes in hardware over a software-only one listed first", async () => {
		browserReports(hardwareUpTo(1080));
		serveMedia({ Movie: movie }, [
			source("file-4k", 2160, "h264"),
			source("file-hd", 1080, "h264"),
		]);
		renderRoute("/media/movie-1");

		await waitFor(() => expect(currentStreamSrc()).toContain("file-hd"));
		// The software-only source is still offered, marked as such.
		const fourK = screen.getByRole("option", { name: /2160p · h264/ });
		expect(fourK).toBeEnabled();
		expect(fourK).toHaveTextContent("decodes in software here");
		expect(screen.queryByText(/may stutter/)).not.toBeInTheDocument();
	});

	it("plays the best source anyway when every one decodes in software, and says so", async () => {
		browserReports(hardwareUpTo(0));
		serveMedia({ Movie: movie }, [
			source("file-4k", 2160, "h264"),
			source("file-hd", 1080, "h264"),
		]);
		renderRoute("/media/movie-1");

		expect(
			await screen.findByText("H264 decodes in software here and may stutter"),
		).toBeInTheDocument();
		expect(currentStreamSrc()).toContain("file-4k");
	});

	it("does not swap out a source that has started playing when the answer arrives", async () => {
		const user = userEvent.setup();
		let answer: () => void = () => {};
		const decide = hardwareUpTo(1080);
		browserReports(
			(configuration) =>
				new Promise((resolve) => {
					const previous = answer;
					answer = () => {
						previous();
						resolve(decide(configuration));
					};
				}),
		);
		serveMedia({ Movie: movie }, [
			source("file-4k", 2160, "h264"),
			source("file-hd", 1080, "h264"),
		]);
		renderRoute("/media/movie-1");

		// Chosen from the synchronous verdicts while the answer is pending.
		await waitFor(() => expect(currentStreamSrc()).toContain("file-4k"));
		await user.click(screen.getByRole("button", { name: "Advance playback" }));
		answer();

		// The answer arrives and marks the playing source software-only; the
		// player keeps it.
		expect(
			await screen.findByText("H264 decodes in software here and may stutter"),
		).toBeInTheDocument();
		expect(currentStreamSrc()).toContain("file-4k");
	});

	it("refines the choice before playback has started", async () => {
		let answer: () => void = () => {};
		const decide = hardwareUpTo(1080);
		browserReports(
			(configuration) =>
				new Promise((resolve) => {
					const previous = answer;
					answer = () => {
						previous();
						resolve(decide(configuration));
					};
				}),
		);
		serveMedia({ Movie: movie }, [
			source("file-4k", 2160, "h264"),
			source("file-hd", 1080, "h264"),
		]);
		renderRoute("/media/movie-1");

		await waitFor(() => expect(currentStreamSrc()).toContain("file-4k"));
		answer();
		await waitFor(() => expect(currentStreamSrc()).toContain("file-hd"));
	});
});
