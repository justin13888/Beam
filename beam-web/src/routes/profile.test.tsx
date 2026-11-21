import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { components } from "@/api.gen";
import * as factory from "@/test/factories";
import { BASE_URL, meUnauthenticatedHandler } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { problem } from "@/test/problem";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

type SessionSummary = components["schemas"]["SessionSummary"];

const sessionA: SessionSummary = {
	id: "sess-a",
	device_hash: "a".repeat(64),
	ip: "203.0.113.7",
	created_at: "2023-11-14T22:13:20Z",
	last_active_at: "2023-11-14T23:13:20Z",
};

const sessionB: SessionSummary = {
	id: "sess-b",
	device_hash: "b".repeat(64),
	ip: "198.51.100.4",
	created_at: "2023-11-03T08:26:40Z",
	last_active_at: "2023-11-03T09:26:40Z",
};

function serveSessions(status = 200) {
	server.use(
		http.get(`${BASE_URL}/v1/me`, () =>
			HttpResponse.json(
				factory.user({
					display_name: "Ada Lovelace",
					email: "ada@example.com",
				}),
			),
		),
		http.get(`${BASE_URL}/v1/sessions`, () =>
			status === 200
				? HttpResponse.json<components["schemas"]["SessionConnection"]>(
						factory.connection([sessionA, sessionB]),
					)
				: problem(status, "boom", "#internal"),
		),
	);
}

describe("/profile", () => {
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it("redirects to login when the session is not authenticated", async () => {
		server.use(meUnauthenticatedHandler);

		const { getLocation } = renderRoute("/profile");
		await waitForRouter();

		await waitFor(() =>
			expect(getLocation()).toBe("/login?redirect=%2Fprofile"),
		);
	});

	it("renders a row per active session with device fingerprint and IP", async () => {
		serveSessions();
		renderRoute("/profile");

		expect(await screen.findByText("203.0.113.7")).toBeInTheDocument();
		expect(screen.getByText("198.51.100.4")).toBeInTheDocument();
		// Truncated device fingerprint (first 12 chars of the hash).
		expect(screen.getByText(`${"a".repeat(12)}…`)).toBeInTheDocument();
		expect(screen.getAllByRole("button", { name: "Revoke" })).toHaveLength(2);
	});

	it("revokes a session by id and refetches the list", async () => {
		const requests = recordRequests();
		serveSessions();
		const user = userEvent.setup();
		renderRoute("/profile");

		const revokeButtons = await screen.findAllByRole("button", {
			name: "Revoke",
		});
		await user.click(revokeButtons[0]);

		await waitFor(() =>
			expect(requests.matching("DELETE", "/v1/sessions/sess-a")).toHaveLength(
				1,
			),
		);
		// invalidateQueries triggers a refetch: the sessions GET runs again.
		await waitFor(() =>
			expect(requests.matching("GET", "/v1/sessions").length).toBeGreaterThan(
				1,
			),
		);
	});

	it("signs out of all sessions and routes through the login flow", async () => {
		const requests = recordRequests();
		serveSessions();
		vi.stubGlobal(
			"confirm",
			vi.fn(() => true),
		);
		const user = userEvent.setup();
		const { getLocation } = renderRoute("/profile");

		await user.click(
			await screen.findByRole("button", { name: /sign out all sessions/i }),
		);

		await waitFor(() =>
			expect(requests.matching("POST", "/v1/logout-all")).toHaveLength(1),
		);
		await waitFor(() => expect(getLocation()).toBe("/login"));
	});

	it("does not fire logout-all when the confirmation is dismissed", async () => {
		const requests = recordRequests();
		serveSessions();
		vi.stubGlobal(
			"confirm",
			vi.fn(() => false),
		);
		const user = userEvent.setup();
		const { getLocation } = renderRoute("/profile");

		await user.click(
			await screen.findByRole("button", { name: /sign out all sessions/i }),
		);

		await waitFor(() =>
			expect(screen.getByText("203.0.113.7")).toBeInTheDocument(),
		);
		expect(requests.matching("POST", "/v1/logout-all")).toEqual([]);
		expect(getLocation()).toBe("/profile");
	});

	describe("playback languages", () => {
		function serveLanguages(
			stored: components["schemas"]["LanguagePreferenceSettings"],
		) {
			server.use(
				http.get(`${BASE_URL}/v1/me/language-preferences`, () =>
					HttpResponse.json(stored),
				),
			);
		}

		it("shows the stored preferences and saves edits as the server stores them", async () => {
			const requests = recordRequests();
			serveSessions();
			serveLanguages({
				audio_languages: ["eng"],
				subtitle_languages: [],
				subtitle_mode: "forced_only",
			});
			// The server answers each language as the code tracks carry.
			server.use(
				http.put(`${BASE_URL}/v1/me/language-preferences`, () =>
					HttpResponse.json<
						components["schemas"]["LanguagePreferenceSettings"]
					>({
						audio_languages: ["jpn", "eng"],
						subtitle_languages: ["eng"],
						subtitle_mode: "on",
					}),
				),
			);
			const user = userEvent.setup();
			renderRoute("/profile");

			const audio = await screen.findByLabelText("Audio languages");
			expect(audio).toHaveValue("eng");
			expect(screen.getByLabelText("Subtitles")).toHaveValue("forced_only");

			await user.clear(audio);
			await user.type(audio, "ja, English");
			await user.type(screen.getByLabelText("Subtitle languages"), "en,");
			await user.selectOptions(screen.getByLabelText("Subtitles"), "on");
			await user.click(screen.getByRole("button", { name: "Save languages" }));

			await waitFor(() =>
				expect(
					requests.matching("PUT", "/v1/me/language-preferences")[0]?.body,
				).toEqual({
					audio_languages: ["ja", "English"],
					subtitle_languages: ["en"],
					subtitle_mode: "on",
				}),
			);
			await waitFor(() =>
				expect(screen.getByLabelText("Audio languages")).toHaveValue(
					"jpn, eng",
				),
			);
		});

		it("offers a retry when the preferences cannot be read", async () => {
			const requests = recordRequests();
			serveSessions();
			server.use(
				http.get(`${BASE_URL}/v1/me/language-preferences`, () =>
					problem(500, "boom", "#internal"),
				),
			);
			const user = userEvent.setup();
			renderRoute("/profile");

			expect(
				await screen.findByText(/Failed to load language preferences/),
			).toBeInTheDocument();
			serveLanguages({
				audio_languages: ["fre"],
				subtitle_languages: [],
				subtitle_mode: "off",
			});
			await user.click(screen.getByRole("button", { name: "Retry" }));

			expect(await screen.findByLabelText("Audio languages")).toHaveValue(
				"fre",
			);
			expect(
				requests.matching("GET", "/v1/me/language-preferences").length,
			).toBeGreaterThan(1);
		});

		it("says why the server refused a language", async () => {
			serveSessions();
			server.use(
				http.put(`${BASE_URL}/v1/me/language-preferences`, () =>
					problem(422, '"klingon" is not a language', "#validation-failed"),
				),
			);
			const user = userEvent.setup();
			renderRoute("/profile");

			const audio = await screen.findByLabelText("Audio languages");
			await user.type(audio, "klingon");
			await user.click(screen.getByRole("button", { name: "Save languages" }));

			expect(
				await screen.findByText(/"klingon" is not a language/),
			).toBeInTheDocument();
			expect(screen.getByLabelText("Audio languages")).toHaveValue("klingon");
		});
	});

	it("shows an error state when the sessions request fails", async () => {
		serveSessions(500);
		renderRoute("/profile");

		expect(
			await screen.findByText(/Failed to load active sessions/),
		).toBeInTheDocument();
	});

	// The sessions list renders its query error inline rather than through the
	// route error boundary, so the taxonomy has to reach the screen by a
	// different path than the one RouteError.test.tsx covers. A 4xx problem
	// document's own sentence is shown; the fallback is not.
	it("shows the server's explanation when the sessions request is refused", async () => {
		server.use(
			http.get(`${BASE_URL}/v1/me`, () =>
				HttpResponse.json(factory.user({ display_name: "Ada Lovelace" })),
			),
			http.get(`${BASE_URL}/v1/sessions`, () =>
				problem(403, "This account has been removed", "#account-removed"),
			),
		);
		renderRoute("/profile");

		expect(
			await screen.findByText(/This account has been removed/),
		).toBeInTheDocument();
		expect(
			screen.queryByText(/Failed to load active sessions/),
		).not.toBeInTheDocument();
	});
});
