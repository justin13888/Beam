import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { meUnauthenticatedHandler } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { server } from "@/test/server";

/**
 * `login` navigates the browser out of the SPA into the server's OIDC flow.
 * jsdom's `location.assign` is a no-op that logs "Not implemented", so it is
 * replaced with a spy -- the assertion is on the URL the app builds, which is
 * the actual behaviour here.
 */
function captureNavigation(): { assigned: () => string | undefined } {
	const assign = vi.fn();
	Object.defineProperty(window, "location", {
		configurable: true,
		value: { ...window.location, assign, pathname: "/login", search: "" },
	});
	return { assigned: () => assign.mock.calls[0]?.[0] as string | undefined };
}

describe("/login", () => {
	afterEach(() => {
		vi.restoreAllMocks();
	});

	it("renders a sign-in button for a signed-out visitor", async () => {
		server.use(meUnauthenticatedHandler);
		renderRoute("/login");
		await waitForRouter();

		expect(
			await screen.findByRole("button", { name: /sign in with sso/i }),
		).toBeInTheDocument();
	});

	it("sends the redirect search param through to the identity provider", async () => {
		server.use(meUnauthenticatedHandler);
		const navigation = captureNavigation();
		const user = userEvent.setup();

		renderRoute("/login?redirect=%2Flibraries");
		await user.click(
			await screen.findByRole("button", { name: /sign in with sso/i }),
		);

		await waitFor(() => {
			const url = new URL(navigation.assigned() ?? "");
			expect(url.pathname).toBe("/v1/auth/login");
			expect(url.searchParams.get("redirect")).toBe("/libraries");
		});
	});

	it("falls back to the current location when no redirect param is present", async () => {
		server.use(meUnauthenticatedHandler);
		const navigation = captureNavigation();
		const user = userEvent.setup();

		renderRoute("/login");
		await user.click(
			await screen.findByRole("button", { name: /sign in with sso/i }),
		);

		await waitFor(() => {
			const url = new URL(navigation.assigned() ?? "");
			expect(url.searchParams.get("redirect")).toBe("/login");
		});
	});

	it("ignores a non-string redirect rather than passing it on", async () => {
		// `validateSearch` narrows the raw search object; anything that is not a
		// string must not reach `login()`, or an attacker-controlled array or
		// object would decide where the user lands after authenticating.
		server.use(meUnauthenticatedHandler);
		const navigation = captureNavigation();
		const user = userEvent.setup();

		renderRoute("/login?redirect=a&redirect=b");
		await user.click(
			await screen.findByRole("button", { name: /sign in with sso/i }),
		);

		await waitFor(() => {
			const url = new URL(navigation.assigned() ?? "");
			expect(url.searchParams.get("redirect")).toBe("/login");
		});
	});
});
