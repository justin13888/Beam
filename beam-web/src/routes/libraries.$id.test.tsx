import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import type { components } from "@/api.gen";
import * as factory from "@/test/factories";
import { BASE_URL } from "@/test/handlers";
import { renderRoute } from "@/test/harness";
import { problem } from "@/test/problem";
import { server } from "@/test/server";

type LibraryFile =
	components["schemas"]["beam_server.models.library.file.LibraryFile"];

const testLibrary = factory.library({ id: "lib-1", name: "Movies", size: 3 });

function libraryFile(overrides: Partial<LibraryFile> = {}): LibraryFile {
	return {
		id: "file-x",
		library_id: "lib-1",
		path: "/media/movies/Untitled.mkv",
		hash: "0",
		size_bytes: 1024,
		status: "Known",
		content_type: "Movie",
		mime_type: "video/x-matroska",
		container_format: "mkv",
		scanned_at: "2024-01-01T00:00:00Z",
		updated_at: "2024-01-01T00:00:00Z",
		...overrides,
	};
}

const files: LibraryFile[] = [
	libraryFile({
		id: "f-a",
		path: "/media/movies/Alpha.mkv",
		size_bytes: 1024,
		status: "Known",
		content_type: "Movie",
		updated_at: "2024-01-01T00:00:00Z",
	}),
	libraryFile({
		id: "f-b",
		path: "/media/movies/Beta.mp4",
		size_bytes: 2048,
		status: "Changed",
		content_type: "Episode",
		updated_at: "2024-02-01T00:00:00Z",
	}),
	libraryFile({
		id: "f-c",
		path: "/media/movies/Gamma.avi",
		size_bytes: 512,
		status: "Unknown",
		content_type: "Unclassified",
		updated_at: "2024-03-01T00:00:00Z",
	}),
];

/** Serve the library and its files; `filesStatus` drives the failure branch. */
function serveLibrary({ filesStatus = 200 }: { filesStatus?: number } = {}) {
	server.use(
		http.get(`${BASE_URL}/v1/libraries/:id`, () =>
			HttpResponse.json(testLibrary),
		),
		http.get(`${BASE_URL}/v1/libraries/:id/files`, () =>
			filesStatus === 200
				? HttpResponse.json(files)
				: problem(filesStatus, "boom", "#internal"),
		),
	);
}

function renderPage() {
	return renderRoute("/libraries/lib-1");
}

/** The rendered file rows' filenames, in DOM (i.e. sorted) order. */
function filenameOrder(): string[] {
	return screen
		.getAllByText(/\.(mkv|mp4|avi)$/)
		.map((el) => el.textContent ?? "");
}

describe("/libraries/$id", () => {
	it("renders the library name and a row per file with size and status", async () => {
		serveLibrary();
		renderPage();

		expect(
			await screen.findByRole("heading", { name: "Movies" }),
		).toBeInTheDocument();
		expect(screen.getByText("Alpha.mkv")).toBeInTheDocument();
		expect(screen.getByText("Beta.mp4")).toBeInTheDocument();
		expect(screen.getByText("Gamma.avi")).toBeInTheDocument();
		// Human-readable size and the status badge label (a <span>, distinct
		// from the same-named "Indexed" status-filter button).
		expect(screen.getByText("1 KB")).toBeInTheDocument();
		expect(
			screen.getByText("Indexed", { selector: "span" }),
		).toBeInTheDocument();
		// Footer row count.
		expect(screen.getByText(/Showing 3 of 3 files/)).toBeInTheDocument();
	});

	it("narrows the table with the client-side search filter", async () => {
		serveLibrary();
		const user = userEvent.setup();
		renderPage();

		await screen.findByText("Alpha.mkv");
		await user.type(screen.getByPlaceholderText(/Search files/), "Beta");

		await waitFor(() =>
			expect(screen.queryByText("Alpha.mkv")).not.toBeInTheDocument(),
		);
		expect(screen.getByText("Beta.mp4")).toBeInTheDocument();
		expect(screen.queryByText("Gamma.avi")).not.toBeInTheDocument();
	});

	it("filters by index status", async () => {
		serveLibrary();
		const user = userEvent.setup();
		renderPage();

		await screen.findByText("Alpha.mkv");
		await user.click(screen.getByRole("button", { name: "Changed" }));

		await waitFor(() =>
			expect(screen.queryByText("Alpha.mkv")).not.toBeInTheDocument(),
		);
		// Beta is the only "Changed" file.
		expect(screen.getByText("Beta.mp4")).toBeInTheDocument();
		expect(screen.queryByText("Gamma.avi")).not.toBeInTheDocument();
	});

	it("sorts by size and toggles direction on repeated header clicks", async () => {
		serveLibrary();
		const user = userEvent.setup();
		renderPage();

		await screen.findByText("Alpha.mkv");
		// Default sort is path ascending.
		expect(filenameOrder()).toEqual(["Alpha.mkv", "Beta.mp4", "Gamma.avi"]);

		// Size ascending: 512 (Gamma) < 1024 (Alpha) < 2048 (Beta).
		await user.click(screen.getByRole("button", { name: "Size" }));
		await waitFor(() =>
			expect(filenameOrder()).toEqual(["Gamma.avi", "Alpha.mkv", "Beta.mp4"]),
		);

		// Second click flips to descending.
		await user.click(screen.getByRole("button", { name: "Size" }));
		await waitFor(() =>
			expect(filenameOrder()).toEqual(["Beta.mp4", "Alpha.mkv", "Gamma.avi"]),
		);
	});

	it("shows an error state with a retry affordance when files fail to load", async () => {
		serveLibrary({ filesStatus: 500 });
		renderPage();

		expect(
			await screen.findByText(/Error: Failed to load library files/),
		).toBeInTheDocument();
		expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
	});
});
