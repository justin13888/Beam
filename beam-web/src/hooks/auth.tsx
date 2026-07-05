import {
	createContext,
	type ReactNode,
	useCallback,
	useContext,
	useEffect,
	useState,
} from "react";
import type { components } from "@/api.gen";
import { env } from "@/env";
import { apiClient } from "@/lib/apiClient";

export type User =
	components["schemas"]["beam_auth.server.oidc_routes.MeResponse"];

export interface AuthContextType {
	user: User | null;
	isAuthenticated: boolean;
	/** True until the initial `GET /v1/me` check resolves. */
	isLoading: boolean;
	/** Redirects the browser into the server's OIDC login flow. `redirectTo`
	 * must be a same-origin-relative path (e.g. `/libraries`); defaults to the
	 * current location. */
	login: (redirectTo?: string) => void;
	logout: () => Promise<void>;
	/** Re-checks `GET /v1/me`; useful right after the OIDC callback redirects
	 * back into the app. */
	refresh: () => Promise<void>;
}

const AuthContext = createContext<AuthContextType | undefined>(undefined);

export function AuthProvider({ children }: { children: ReactNode }) {
	const [user, setUser] = useState<User | null>(null);
	const [isLoading, setIsLoading] = useState(true);

	const refresh = useCallback(async () => {
		const { data, response } = await apiClient.GET("/v1/me", {
			credentials: "include",
		});
		setUser(response.ok && data ? data : null);
	}, []);

	useEffect(() => {
		refresh().finally(() => setIsLoading(false));
	}, [refresh]);

	const login = (redirectTo?: string) => {
		const target =
			redirectTo ?? `${window.location.pathname}${window.location.search}`;
		const url = new URL("/v1/auth/login", env.C_STREAM_SERVER_URL);
		url.searchParams.set("redirect", target);
		window.location.assign(url.toString());
	};

	const logout = async () => {
		await apiClient
			.POST("/v1/logout", { credentials: "include" })
			.catch(console.error);
		setUser(null);
	};

	return (
		<AuthContext.Provider
			value={{
				user,
				isAuthenticated: !!user,
				isLoading,
				login,
				logout,
				refresh,
			}}
		>
			{children}
		</AuthContext.Provider>
	);
}

export function useAuth() {
	const context = useContext(AuthContext);
	if (context === undefined) {
		throw new Error("useAuth must be used within an AuthProvider");
	}
	return context;
}
