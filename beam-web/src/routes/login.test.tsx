import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockLogin, mockUseSearch } = vi.hoisted(() => ({
	mockLogin: vi.fn(),
	mockUseSearch: vi.fn<() => { redirect?: string }>(() => ({
		redirect: undefined,
	})),
}));

vi.mock("@tanstack/react-router", () => ({
	createFileRoute: (_path: string) => (opts: Record<string, unknown>) => ({
		...opts,
		useSearch: mockUseSearch,
	}),
}));

vi.mock("../hooks/auth", () => ({
	useAuth: () => ({ login: mockLogin }),
}));

import { LoginPage } from "./login";

describe("LoginPage", () => {
	beforeEach(() => {
		mockLogin.mockReset();
		mockUseSearch.mockReturnValue({ redirect: undefined });
	});

	it("renders a sign-in button", () => {
		render(<LoginPage />);
		expect(
			screen.getByRole("button", { name: /sign in with sso/i }),
		).toBeInTheDocument();
	});

	it("clicking the button calls login() with the redirect search param", async () => {
		mockUseSearch.mockReturnValue({ redirect: "/libraries" });
		const user = userEvent.setup();
		render(<LoginPage />);

		await user.click(screen.getByRole("button", { name: /sign in with sso/i }));

		expect(mockLogin).toHaveBeenCalledWith("/libraries");
	});

	it("clicking the button calls login() with undefined when no redirect param is present", async () => {
		const user = userEvent.setup();
		render(<LoginPage />);

		await user.click(screen.getByRole("button", { name: /sign in with sso/i }));

		expect(mockLogin).toHaveBeenCalledWith(undefined);
	});
});
