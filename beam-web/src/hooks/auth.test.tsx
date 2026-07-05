import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AuthProvider, useAuth } from "./auth";

const { mockGet, mockPost } = vi.hoisted(() => ({
	mockGet: vi.fn(),
	mockPost: vi.fn(),
}));

vi.mock("@/lib/apiClient", () => ({
	apiClient: {
		GET: mockGet,
		POST: mockPost,
	},
}));

const mockUser = {
	id: "user-1",
	email: "test@example.com",
	is_admin: false,
	display_name: "Test User",
	avatar_url: null,
};

function wrapper({ children }: { children: ReactNode }) {
	return <AuthProvider>{children}</AuthProvider>;
}

describe("useAuth", () => {
	beforeEach(() => {
		mockGet.mockReset();
		mockPost.mockReset();
	});

	it("throws when used outside AuthProvider", () => {
		expect(() => renderHook(() => useAuth())).toThrow(
			"useAuth must be used within an AuthProvider",
		);
	});

	it("starts loading, then resolves to unauthenticated when GET /v1/me is unauthorized", async () => {
		mockGet.mockResolvedValue({ data: undefined, response: { ok: false } });

		const { result } = renderHook(() => useAuth(), { wrapper });
		expect(result.current.isLoading).toBe(true);
		expect(result.current.isAuthenticated).toBe(false);

		await waitFor(() => expect(result.current.isLoading).toBe(false));
		expect(result.current.user).toBeNull();
		expect(result.current.isAuthenticated).toBe(false);
		expect(mockGet).toHaveBeenCalledWith("/v1/me", { credentials: "include" });
	});

	it("resolves the user from GET /v1/me when a valid session cookie exists", async () => {
		mockGet.mockResolvedValue({ data: mockUser, response: { ok: true } });

		const { result } = renderHook(() => useAuth(), { wrapper });
		await waitFor(() => expect(result.current.isLoading).toBe(false));

		expect(result.current.user).toEqual(mockUser);
		expect(result.current.isAuthenticated).toBe(true);
	});

	it("logout() calls POST /v1/logout and clears the user", async () => {
		mockGet.mockResolvedValue({ data: mockUser, response: { ok: true } });
		mockPost.mockResolvedValue({ data: undefined, response: { ok: true } });

		const { result } = renderHook(() => useAuth(), { wrapper });
		await waitFor(() => expect(result.current.isLoading).toBe(false));
		expect(result.current.isAuthenticated).toBe(true);

		await act(async () => {
			await result.current.logout();
		});

		expect(mockPost).toHaveBeenCalledWith("/v1/logout", {
			credentials: "include",
		});
		expect(result.current.user).toBeNull();
		expect(result.current.isAuthenticated).toBe(false);
	});

	it("login() redirects the browser to the server's OIDC login endpoint with a redirect param", async () => {
		mockGet.mockResolvedValue({ data: undefined, response: { ok: false } });
		const assignSpy = vi.fn();
		vi.stubGlobal("location", {
			...window.location,
			pathname: "/libraries",
			search: "",
			assign: assignSpy,
		});

		const { result } = renderHook(() => useAuth(), { wrapper });
		await waitFor(() => expect(result.current.isLoading).toBe(false));

		act(() => {
			result.current.login();
		});

		expect(assignSpy).toHaveBeenCalledWith(
			"http://localhost:8000/v1/auth/login?redirect=%2Flibraries",
		);

		vi.unstubAllGlobals();
	});

	it("login(redirectTo) uses the explicit redirect target over the current location", async () => {
		mockGet.mockResolvedValue({ data: undefined, response: { ok: false } });
		const assignSpy = vi.fn();
		vi.stubGlobal("location", { ...window.location, assign: assignSpy });

		const { result } = renderHook(() => useAuth(), { wrapper });
		await waitFor(() => expect(result.current.isLoading).toBe(false));

		act(() => {
			result.current.login("/media/abc");
		});

		expect(assignSpy).toHaveBeenCalledWith(
			"http://localhost:8000/v1/auth/login?redirect=%2Fmedia%2Fabc",
		);

		vi.unstubAllGlobals();
	});
});
