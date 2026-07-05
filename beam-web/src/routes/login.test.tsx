import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AuthProvider } from "../hooks/auth";
import { LoginPage } from "./login";

// vi.hoisted ensures these are available when vi.mock factories run (which are hoisted)
const { mockNavigate, mockPost } = vi.hoisted(() => ({
	mockNavigate: vi.fn(),
	mockPost: vi.fn(),
}));

vi.mock("@tanstack/react-router", () => ({
	createFileRoute: (_path: string) => (opts: Record<string, unknown>) => opts,
	useNavigate: () => mockNavigate,
	Link: ({ children, to }: { children: React.ReactNode; to: string }) => (
		<a href={to}>{children}</a>
	),
}));

vi.mock("@/lib/apiClient", () => ({
	apiClient: {
		POST: mockPost,
		GET: vi.fn(),
		PUT: vi.fn(),
		DELETE: vi.fn(),
	},
}));

const successResponse = {
	data: {
		token: "test-jwt-token",
		session_id: "test-session-id",
		user: {
			id: "user-1",
			username: "testuser",
			email: "test@example.com",
			is_admin: false,
		},
	},
	error: undefined,
	response: { ok: true } as Response,
};

const failureResponse = {
	data: undefined,
	error: { message: "Invalid credentials", code: "invalid_credentials" },
	response: { ok: false } as Response,
};

function renderLoginPage() {
	return render(
		<AuthProvider>
			<LoginPage />
		</AuthProvider>,
	);
}

describe("LoginPage", () => {
	beforeEach(() => {
		mockNavigate.mockReset();
		mockPost.mockReset();
	});

	afterEach(() => {
		localStorage.clear();
	});

	it("renders username/email and password inputs", () => {
		renderLoginPage();
		expect(
			screen.getByRole("textbox", { name: /username or email/i }),
		).toBeInTheDocument();
		// password input is type="password" so it has no implicit role; query by label text
		expect(screen.getByLabelText(/^password$/i)).toBeInTheDocument();
	});

	it("renders the sign in button", () => {
		renderLoginPage();
		expect(
			screen.getByRole("button", { name: /sign in/i }),
		).toBeInTheDocument();
	});

	it("submit button is disabled while request is in flight", async () => {
		// Resolve only after the assertion to simulate in-flight state
		let resolvePost!: (value: typeof successResponse) => void;
		mockPost.mockReturnValue(
			new Promise<typeof successResponse>((resolve) => {
				resolvePost = resolve;
			}),
		);

		const user = userEvent.setup();
		renderLoginPage();

		await user.type(
			screen.getByRole("textbox", { name: /username or email/i }),
			"testuser",
		);
		await user.type(screen.getByLabelText(/^password$/i), "correctpassword");

		const button = screen.getByRole("button", { name: /sign in/i });
		await user.click(button);

		// Button is disabled immediately after click (request in flight)
		expect(button).toBeDisabled();

		// Resolve the pending request
		resolvePost(successResponse);

		// Button re-enables after request completes
		await waitFor(() => expect(button).not.toBeDisabled());
	});

	it("on valid credentials: calls POST /v1/auth/login and navigates to /", async () => {
		mockPost.mockResolvedValue(successResponse);

		const user = userEvent.setup();
		renderLoginPage();

		await user.type(
			screen.getByRole("textbox", { name: /username or email/i }),
			"testuser",
		);
		await user.type(screen.getByLabelText(/^password$/i), "correctpassword");
		await user.click(screen.getByRole("button", { name: /sign in/i }));

		await waitFor(() => {
			expect(mockPost).toHaveBeenCalledWith("/v1/auth/login", {
				body: {
					username_or_email: "testuser",
					password: "correctpassword",
				},
				credentials: "include",
			});
			expect(mockNavigate).toHaveBeenCalledWith({ to: "/" });
		});
	});

	it("on invalid credentials: shows error message and does not navigate", async () => {
		mockPost.mockResolvedValue(failureResponse);

		const user = userEvent.setup();
		renderLoginPage();

		await user.type(
			screen.getByRole("textbox", { name: /username or email/i }),
			"testuser",
		);
		await user.type(screen.getByLabelText(/^password$/i), "wrongpassword");
		await user.click(screen.getByRole("button", { name: /sign in/i }));

		// The error object's `message` field is surfaced, not the useless
		// "[object Object]" that `String(apiError)` would produce.
		await waitFor(() => {
			expect(screen.getByText("Invalid credentials")).toBeInTheDocument();
		});
		expect(mockNavigate).not.toHaveBeenCalled();
	});

	it("on a plain-string error body: shows the string directly", async () => {
		mockPost.mockResolvedValue({
			data: undefined,
			error: "Invalid or expired token",
			response: { ok: false } as Response,
		});

		const user = userEvent.setup();
		renderLoginPage();

		await user.type(
			screen.getByRole("textbox", { name: /username or email/i }),
			"testuser",
		);
		await user.type(screen.getByLabelText(/^password$/i), "wrongpassword");
		await user.click(screen.getByRole("button", { name: /sign in/i }));

		await waitFor(() => {
			expect(screen.getByText("Invalid or expired token")).toBeInTheDocument();
		});
		expect(mockNavigate).not.toHaveBeenCalled();
	});
});
