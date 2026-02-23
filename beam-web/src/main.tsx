import { createRouter, RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import ReactDOM from "react-dom/client";
import { env } from "@/env";

import * as TanStackQueryProvider from "./integrations/tanstack-query/root-provider.tsx";

// Import the generated route tree
import { routeTree } from "./routeTree.gen";

import "./styles.css";

import { ApolloClient, HttpLink, InMemoryCache } from "@apollo/client";
import { ApolloProvider } from "@apollo/client/react";
import reportWebVitals from "./reportWebVitals.ts";

import { setContext } from "@apollo/client/link/context";
import { AuthProvider, useAuth } from "./hooks/auth";

const httpLink = new HttpLink({ uri: `${env.C_STREAM_SERVER_URL}/v1/graphql` });

const authLink = setContext((_, { headers }) => {
	const token = localStorage.getItem("token");
	return {
		headers: {
			...headers,
			authorization: token ? `Bearer ${token}` : "",
		},
	};
});

const client = new ApolloClient({
	link: authLink.concat(httpLink),
	cache: new InMemoryCache(),
	defaultOptions: {
		watchQuery: {
			fetchPolicy: "cache-and-network",
			errorPolicy: "none",
		},
		query: {
			fetchPolicy: "network-only",
			errorPolicy: "none",
		},
	},
});

// Create a new router instance
const TanStackQueryProviderContext = TanStackQueryProvider.getContext();
const router = createRouter({
	routeTree,
	context: {
		...TanStackQueryProviderContext,
		apolloClient: client,
		auth: undefined!, // Initial value, will be overwritten by RouterProvider
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
	return <RouterProvider router={router} context={{ auth }} />;
}

// Render the app
const rootElement = document.getElementById("app");
if (rootElement && !rootElement.innerHTML) {
	const root = ReactDOM.createRoot(rootElement);
	root.render(
		<StrictMode>
			<AuthProvider>
				<ApolloProvider client={client}>
					<TanStackQueryProvider.Provider {...TanStackQueryProviderContext}>
						<App />
					</TanStackQueryProvider.Provider>
				</ApolloProvider>
			</AuthProvider>
		</StrictMode>,
	);
}

// If you want to start measuring performance in your app, pass a function
// to log results (for example: reportWebVitals(console.log))
// or send to an analytics endpoint. Learn more: https://bit.ly/CRA-vitals
reportWebVitals();
