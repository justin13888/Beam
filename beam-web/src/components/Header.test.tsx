import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import * as factory from "@/test/factories";
import { meAdminHandler, meUnauthenticatedHandler } from "@/test/handlers";
import { renderRoute, waitForRouter } from "@/test/harness";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";

/** The slide-out navigation drawer. It is always in the DOM -- open/closed is
 * a CSS transform -- so every assertion about it is scoped rather than global. */
function drawer(): HTMLElement {
	return screen.getByRole("complementary");
}

/** The top bar. */
function banner(): HTMLElement {
	return screen.getByRole("banner");
}

describe("Header", () => {
	it("greets a signed-in user and offers logout", async () => {
		renderRoute("/");
		await waitForRouter();

		expect(
			await screen.findByText(`Hello, ${factory.user().display_name}`),
		).toBeInTheDocument();
		expect(
			within(banner()).getByRole("button", { name: /logout/i }),
		).toBeInTheDocument();
		expect(
			within(banner()).queryByRole("button", { name: /sign in/i }),
		).not.toBeInTheDocument();
	});

	it("offers sign-in instead when there is no session", async () => {
		server.use(meUnauthenticatedHandler);
		renderRoute("/");
		await waitForRouter();

		await waitFor(() =>
			expect(
				within(banner()).getByRole("button", { name: /sign in/i }),
			).toBeInTheDocument(),
		);
		expect(
			within(banner()).queryByRole("button", { name: /logout/i }),
		).not.toBeInTheDocument();
	});

	it("hides the admin link from a non-admin", async () => {
		const user = userEvent.setup();
		renderRoute("/");
		await waitForRouter();

		await user.click(await screen.findByRole("button", { name: "Open menu" }));

		expect(
			within(drawer()).queryByRole("link", { name: "Admin" }),
		).not.toBeInTheDocument();
		// The ordinary destinations are still there.
		expect(
			within(drawer()).getByRole("link", { name: "Libraries" }),
		).toBeInTheDocument();
	});

	it("shows the admin link to an admin, pointing at the logs tab", async () => {
		server.use(meAdminHandler);
		const user = userEvent.setup();
		renderRoute("/");
		await waitForRouter();

		await user.click(await screen.findByRole("button", { name: "Open menu" }));

		expect(
			within(drawer()).getByRole("link", { name: "Admin" }),
		).toHaveAttribute("href", "/admin?tab=logs");
	});

	it("logging out from the drawer clears the session and lands on login", async () => {
		const requests = recordRequests();
		const user = userEvent.setup();
		const { getLocation } = renderRoute("/");
		await waitForRouter();

		await user.click(await screen.findByRole("button", { name: "Open menu" }));
		await user.click(within(drawer()).getByRole("button", { name: /logout/i }));

		await waitFor(() =>
			expect(requests.matching("POST", "/v1/logout")).toHaveLength(1),
		);
		await waitFor(() => expect(getLocation()).toBe("/login"));
	});

	it("closing the drawer leaves the page in place", async () => {
		const user = userEvent.setup();
		const { getLocation } = renderRoute("/");
		await waitForRouter();

		await user.click(await screen.findByRole("button", { name: "Open menu" }));
		await user.click(screen.getByRole("button", { name: "Close menu" }));

		expect(getLocation()).toBe("/");
	});
});
