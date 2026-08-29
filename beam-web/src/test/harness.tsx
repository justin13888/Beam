import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
	createMemoryHistory,
	createRouter,
	RouterProvider,
} from "@tanstack/react-router";
import {
	type RenderResult,
	render,
	screen,
	waitFor,
} from "@testing-library/react";
import { type AuthContextType, AuthProvider, useAuth } from "@/hooks/auth";
import { routeTree } from "@/routeTree.gen";

/**
 * Render the application at `path` with a **real** router.
 *
 * The route tests used to `vi.mock("@tanstack/react-router")` wholesale, which
 * replaced `createFileRoute` with a function returning its own options object.
 * Everything the router is responsible for therefore never ran: `beforeLoad`
 * guards, `validateSearch` parsers, loaders, and `errorComponent` wiring were
 * all dead code as far as the suite was concerned, while the tests still read
 * as if they covered the routes.
 *
 * This mounts the generated route tree over `createMemoryHistory`, so
 * navigating to a guarded path really runs its guard and a redirect really
 * lands somewhere. HTTP goes through MSW (see `src/test/handlers.ts`) rather
 * than a mocked `apiClient`, so URL construction, query serialization, and
 * error-body shapes are exercised too.
 */
export function renderRoute(
	path: string,
	options: { queryClient?: QueryClient } = {},
): RenderResult & { getLocation: () => string } {
	const queryClient =
		options.queryClient ??
		new QueryClient({
			defaultOptions: {
				// A test asserting an error state should see it on the first
				// response, not after three silent retries.
				queries: { retry: false },
				mutations: { retry: false },
			},
		});

	const router = createRouter({
		routeTree,
		history: createMemoryHistory({ initialEntries: [path] }),
		context: {
			queryClient,
			// Overwritten by `RouterProvider`'s `context` prop below, exactly as
			// `main.tsx` does it.
			auth: undefined as unknown as AuthContextType,
		},
		defaultPendingMinMs: 0,
	});

	/** Mirrors `main.tsx`: the router must not mount until `GET /v1/me` has
	 * resolved, or a guard reads a transient logged-out state. */
	function App() {
		const auth = useAuth();
		if (auth.isLoading) {
			return <div data-testid="auth-loading" />;
		}
		return <RouterProvider router={router} context={{ auth }} />;
	}

	const result = render(
		<AuthProvider>
			<QueryClientProvider client={queryClient}>
				<App />
			</QueryClientProvider>
		</AuthProvider>,
	);

	return {
		...result,
		getLocation: () => router.state.location.href,
	};
}

/** Wait until the initial auth check has resolved and the router has mounted. */
export async function waitForRouter(): Promise<void> {
	await waitFor(() => {
		if (screen.queryByTestId("auth-loading") !== null) {
			throw new Error("still waiting for the initial GET /v1/me");
		}
	});
}
