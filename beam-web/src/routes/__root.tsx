import { TanStackDevtools } from "@tanstack/react-devtools";
import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, Outlet } from "@tanstack/react-router";
import { TanStackRouterDevtoolsPanel } from "@tanstack/react-router-devtools";
import { Toaster } from "sonner";
import Header from "../components/Header";
import { RouteError } from "../components/RouteError";
import type { AuthContextType } from "../hooks/auth";
import TanStackQueryDevtools from "../integrations/tanstack-query/devtools";

interface MyRouterContext {
	queryClient: QueryClient;
	auth: AuthContextType;
}

export const Route = createRootRouteWithContext<MyRouterContext>()({
	errorComponent: RouteError,
	component: () => (
		<>
			<Header />
			<Outlet />
			<Toaster theme="dark" position="top-right" richColors />
			<DevtoolsPanel />
		</>
	),
});

/**
 * Devtools, mounted only when running under the dev server.
 *
 * They were previously unconditional, so the panel and both plugins shipped in
 * the production bundle -- and, less visibly, they made the root route
 * unmountable outside a browser: the devtools core throws "Devtools is not
 * mounted" when React tears the tree down, which is the first thing any test
 * of a real router does.
 *
 * The condition is `MODE === "development"` rather than `DEV`, which is merely
 * "not a production build" and so is also true under the test runner.
 */
function DevtoolsPanel() {
	if (import.meta.env.MODE !== "development") {
		return null;
	}
	return (
		<TanStackDevtools
			config={{
				position: "bottom-right",
			}}
			plugins={[
				{
					name: "Tanstack Router",
					render: <TanStackRouterDevtoolsPanel />,
				},
				TanStackQueryDevtools,
			]}
		/>
	);
}
