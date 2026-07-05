import { HttpResponse, http } from "msw";

const BASE_URL = "http://localhost:8000";

export const mockUser = {
	id: "user-1",
	email: "test@example.com",
	is_admin: false,
	display_name: "Test User",
	avatar_url: null,
};

export const handlers = [
	// Default: an authenticated session cookie resolves to mockUser.
	http.get(`${BASE_URL}/v1/me`, () => {
		return HttpResponse.json(mockUser);
	}),

	http.post(`${BASE_URL}/v1/logout`, () => {
		return new HttpResponse(null, { status: 200 });
	}),
];

// Reusable override for an unauthenticated / expired session.
export const meUnauthenticatedHandler = http.get(`${BASE_URL}/v1/me`, () => {
	return HttpResponse.json(
		{ message: "Missing session cookie", code: "unauthorized" },
		{ status: 401 },
	);
});
