import { createRouter, RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import ReactDOM from "react-dom/client";

import * as TanStackQueryProvider from "./integrations/tanstack-query/root-provider.tsx";

// Import the generated route tree
import { routeTree } from "./routeTree.gen";

import "./styles.css";

import { AuthProvider, useAuth } from "./hooks/auth";

// Create a new router instance
const TanStackQueryProviderContext = TanStackQueryProvider.getContext();
const router = createRouter({
	routeTree,
	context: {
		...TanStackQueryProviderContext,
		// biome-ignore lint/style/noNonNullAssertion: intentional placeholder overwritten by RouterProvider
		auth: undefined!,
	},
	defaultPreload: "intent",
	scrollRestoration: true,
	defaultStructuralSharing: true,
	defaultPreloadStaleTime: 0,
});

// Register the router instance for type safety
declare module "@tanstack/react-router" {
	interface Register {
		router: typeof router;
	}
}

function App() {
	const auth = useAuth();
	// Block router rendering until the initial `GET /v1/me` check resolves --
	// route `beforeLoad` guards read `context.auth.isAuthenticated`
	// synchronously and must never see a transient "logged out" state while
	// the session cookie is still being verified.
	if (auth.isLoading) {
		return null;
	}
	return <RouterProvider router={router} context={{ auth }} />;
}

// Render the app
const rootElement = document.getElementById("app");
if (rootElement && !rootElement.innerHTML) {
	const root = ReactDOM.createRoot(rootElement);
	root.render(
		<StrictMode>
			<AuthProvider>
				<TanStackQueryProvider.Provider {...TanStackQueryProviderContext}>
					<App />
				</TanStackQueryProvider.Provider>
			</AuthProvider>
		</StrictMode>,
	);
}
