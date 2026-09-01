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

type SessionSummary =
	components["schemas"]["beam_auth.server.oidc_routes.SessionSummary"];

const sessionA: SessionSummary = {
	id: "sess-a",
	device_hash: "a".repeat(64),
	ip: "203.0.113.7",
	created_at: 1700000000,
	last_active: 1700003600,
};

const sessionB: SessionSummary = {
	id: "sess-b",
	device_hash: "b".repeat(64),
	ip: "198.51.100.4",
	created_at: 1699000000,
	last_active: 1699003600,
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
				? HttpResponse.json([sessionA, sessionB])
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

	it("shows an error state when the sessions request fails", async () => {
		serveSessions(500);
		renderRoute("/profile");

		expect(
			await screen.findByText(/Failed to load active sessions/),
		).toBeInTheDocument();
	});
});
