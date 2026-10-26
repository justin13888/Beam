import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import type { components } from "@/api.gen";
import * as factory from "@/test/factories";
import { BASE_URL, meUnauthenticatedHandler } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { problem } from "@/test/problem";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

type MediaMetadata = components["schemas"]["MediaMetadata"];

const movieInProgress = factory.historyItem({
	file_id: "file-1",
	media_id: "movie-1",
	last_played_at: new Date(Date.now() - 5 * 60_000).toISOString(),
});

const movieCompleted = factory.historyItem({
	file_id: "file-2",
	media_id: "movie-2",
	position_secs: 5900,
	played: true,
	last_played_at: new Date(Date.now() - 3 * 3600_000).toISOString(),
});

const showEpisode = factory.historyItem({
	file_id: "file-3",
	media_id: "show-1",
	media_type: "show",
	episode_id: "ep-1",
	position_secs: 300,
	duration_secs: 1500,
	last_played_at: new Date(Date.now() - 2 * 86400_000).toISOString(),
});

const mediaById: Record<string, MediaMetadata> = {
	"movie-1": {
		Movie: factory.movie({
			id: "movie-1",
			title: { original: "Inception", localized: null, alternatives: [] },
			poster_url: "https://img.example/inception.jpg",
		}),
	},
	"movie-2": {
		Movie: factory.movie({
			id: "movie-2",
			title: { original: "Arrival", localized: null, alternatives: [] },
			poster_url: null,
		}),
	},
	"show-1": {
		Show: factory.show({
			id: "show-1",
			title: { original: "Severance", localized: null, alternatives: [] },
			seasons: [
				factory.season({
					season_number: 1,
					poster_url: "https://img.example/severance-s1.jpg",
					episodes: [
						factory.episode({
							id: "ep-1",
							episode_number: 2,
							title: "Half Loop",
						}),
					],
				}),
			],
		}),
	},
};

/**
 * Serve `/v1/history` and the media lookups the page fans out to. When `total`
 * exceeds the page, the page reports a next one after the cursor `cursor-1`.
 */
function serveHistory(
	items: components["schemas"]["HistoryItem"][],
	total: number,
) {
	const more = total > items.length;
	server.use(
		http.get(`${BASE_URL}/v1/history`, () =>
			HttpResponse.json<components["schemas"]["HistoryConnection"]>(
				factory.countedConnection(
					items,
					{ has_next_page: more, end_cursor: more ? "cursor-1" : null },
					total,
				),
			),
		),
		http.get(`${BASE_URL}/v1/media/:id`, ({ params }) => {
			const media = mediaById[String(params.id)];
			return media
				? HttpResponse.json(media)
				: problem(404, "Not found", "#media-not-found");
		}),
	);
}

describe("/history", () => {
	it("redirects to login when the session is not authenticated", async () => {
		server.use(meUnauthenticatedHandler);

		const { getLocation } = renderRoute("/history");
		await waitForRouter();

		await waitFor(() =>
			expect(getLocation()).toBe("/login?redirect=%2Fhistory"),
		);
	});

	it("renders a row per history item with title, completed badge and episode tag", async () => {
		serveHistory([movieInProgress, movieCompleted, showEpisode], 3);
		renderRoute("/history");

		expect(await screen.findByText("Inception")).toBeInTheDocument();
		// "Arrival" has no poster, so its title also renders inside the
		// poster-fallback tile — hence at least one match, not exactly one.
		expect(screen.getAllByText("Arrival").length).toBeGreaterThan(0);
		expect(screen.getByText("Severance")).toBeInTheDocument();
		expect(screen.getAllByText("Completed")).toHaveLength(1);
		expect(screen.getByText("S01E02")).toBeInTheDocument();
		expect(screen.getByText("Watched 5m ago")).toBeInTheDocument();
		expect(screen.getByText("Watched 3h ago")).toBeInTheDocument();
	});

	it("pages forward after the end cursor and back to the first page", async () => {
		const requests = recordRequests();
		serveHistory([movieInProgress], 120);
		const user = userEvent.setup();
		renderRoute("/history");

		await screen.findByText("Inception");
		const [first] = requests.matching("GET", "/v1/history");
		expect(first.query.get("first")).toBe("50");
		expect(first.query.has("after")).toBe(false);
		expect(screen.getByText("Page 1 of 3")).toBeInTheDocument();
		expect(screen.getByRole("button", { name: /prev/i })).toBeDisabled();

		await user.click(screen.getByRole("button", { name: /next/i }));

		await waitFor(() => {
			const afters = requests
				.matching("GET", "/v1/history")
				.map((r) => r.query.get("after"));
			expect(afters).toContain("cursor-1");
		});
		expect(await screen.findByText("Page 2 of 3")).toBeInTheDocument();

		await user.click(screen.getByRole("button", { name: /prev/i }));

		expect(await screen.findByText("Page 1 of 3")).toBeInTheDocument();
	});

	it("links Resume to the media page with the file deep-link", async () => {
		serveHistory([movieInProgress], 1);
		renderRoute("/history");

		const resume = await screen.findByRole("link", { name: /resume/i });
		expect(resume).toHaveAttribute("href", "/media/movie-1?fileId=file-1");
	});

	it("Start over zeroes progress for the right file then navigates to it", async () => {
		const requests = recordRequests();
		serveHistory([movieInProgress], 1);
		const user = userEvent.setup();
		const { getLocation } = renderRoute("/history");

		await user.click(
			await screen.findByRole("button", { name: /start over/i }),
		);

		await waitFor(() => {
			const puts = requests.matching("PUT", "/v1/files/file-1/progress");
			expect(puts).toHaveLength(1);
			expect(puts[0].body).toEqual({ position_secs: 0, duration_secs: 6000 });
		});
		await waitFor(() =>
			expect(getLocation()).toBe("/media/movie-1?fileId=file-1"),
		);
	});

	it("shows an empty state when nothing has been watched", async () => {
		serveHistory([], 0);
		renderRoute("/history");

		expect(await screen.findByText(/Nothing watched yet/)).toBeInTheDocument();
	});
});
