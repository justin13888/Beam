import { act, renderHook, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import type { ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import * as factory from "@/test/factories";
import { meUnauthenticatedHandler } from "@/test/handlers";
import { recordRequests } from "@/test/requests";
import { server } from "@/test/server";
import { AuthProvider, useAuth } from "./auth";

const mockUser = factory.user();

function wrapper({ children }: { children: ReactNode }) {
	return <AuthProvider>{children}</AuthProvider>;
}

/** Replace `window.location` with a recording stand-in. jsdom's own
 * `assign` is a no-op, and the assertion here is on the URL the hook builds. */
function captureNavigation(pathname = "/", search = "") {
	const assign = vi.fn();
	Object.defineProperty(window, "location", {
		configurable: true,
		value: { ...window.location, pathname, search, assign },
	});
	return assign;
}

describe("useAuth", () => {
	it("throws when used outside AuthProvider", () => {
		expect(() => renderHook(() => useAuth())).toThrow(
			"useAuth must be used within an AuthProvider",
		);
	});

	it("starts loading, then resolves to unauthenticated when GET /v1/me is unauthorized", async () => {
		const requests = recordRequests();
		server.use(meUnauthenticatedHandler);

		const { result } = renderHook(() => useAuth(), { wrapper });
		expect(result.current.isLoading).toBe(true);
		expect(result.current.isAuthenticated).toBe(false);

		await waitFor(() => expect(result.current.isLoading).toBe(false));
		expect(result.current.user).toBeNull();
		expect(result.current.isAuthenticated).toBe(false);
		expect(requests.matching("GET", "/v1/me")).toHaveLength(1);
	});

	it("resolves the user from GET /v1/me when a valid session cookie exists", async () => {
		const { result } = renderHook(() => useAuth(), { wrapper });
		await waitFor(() => expect(result.current.isLoading).toBe(false));

		expect(result.current.user).toEqual(mockUser);
		expect(result.current.isAuthenticated).toBe(true);
	});

	it("logout() calls POST /v1/logout and clears the user", async () => {
		const requests = recordRequests();
		const { result } = renderHook(() => useAuth(), { wrapper });
		await waitFor(() => expect(result.current.isLoading).toBe(false));
		expect(result.current.isAuthenticated).toBe(true);

		await act(async () => {
			await result.current.logout();
		});

		expect(requests.matching("POST", "/v1/logout")).toHaveLength(1);
		expect(result.current.user).toBeNull();
		expect(result.current.isAuthenticated).toBe(false);
	});

	it("clears the user even when the logout request fails", async () => {
		// The cookie may already be gone server-side. Leaving a stale user in
		// memory would keep the UI showing a session the server has forgotten.
		server.use(
			meUnauthenticatedHandler,
			http.post("http://localhost:8000/v1/logout", () => HttpResponse.error()),
		);
		const { result } = renderHook(() => useAuth(), { wrapper });
		await waitFor(() => expect(result.current.isLoading).toBe(false));

		await act(async () => {
			await result.current.logout();
		});

		expect(result.current.user).toBeNull();
	});

	it("login() redirects the browser to the server's OIDC login endpoint with a redirect param", async () => {
		server.use(meUnauthenticatedHandler);
		const assign = captureNavigation("/libraries", "");

		const { result } = renderHook(() => useAuth(), { wrapper });
		await waitFor(() => expect(result.current.isLoading).toBe(false));

		act(() => {
			result.current.login();
		});

		expect(assign).toHaveBeenCalledWith(
			"http://localhost:8000/v1/auth/login?redirect=%2Flibraries",
		);
	});

	it("login(redirectTo) uses the explicit redirect target over the current location", async () => {
		server.use(meUnauthenticatedHandler);
		const assign = captureNavigation("/libraries", "");

		const { result } = renderHook(() => useAuth(), { wrapper });
		await waitFor(() => expect(result.current.isLoading).toBe(false));

		act(() => {
			result.current.login("/media/abc");
		});

		expect(assign).toHaveBeenCalledWith(
			"http://localhost:8000/v1/auth/login?redirect=%2Fmedia%2Fabc",
		);
	});
});
