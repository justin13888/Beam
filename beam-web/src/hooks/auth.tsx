import {
	createContext,
	type ReactNode,
	useContext,
	useEffect,
	useState,
} from "react";
import { env } from "@/env";

export interface User {
	id: string;
	username: string;
	email: string;
	is_admin: boolean;
}

export interface AuthResponse {
	token: string;
	user: User;
}

export interface AuthContextType {
	user: User | null;
	token: string | null;
	isAuthenticated: boolean;
	login: (data: AuthResponse) => void;
	logout: () => void;
}

const AuthContext = createContext<AuthContextType | undefined>(undefined);

export function AuthProvider({ children }: { children: ReactNode }) {
	const [user, setUser] = useState<User | null>(null);
	const [token, setToken] = useState<string | null>(null);

	useEffect(() => {
		// Initialize from localStorage
		const storedToken = localStorage.getItem("token");
		const storedUser = localStorage.getItem("user");

		if (storedToken && storedUser) {
			setToken(storedToken);
			try {
				setUser(JSON.parse(storedUser));
			} catch (error) {
				console.error("Failed to parse user from local storage:", error);
				localStorage.removeItem("user");
				localStorage.removeItem("token");
			}
		}
	}, []);

	const login = (data: AuthResponse) => {
		setToken(data.token);
		setUser(data.user);

		localStorage.setItem("token", data.token);
		localStorage.setItem("user", JSON.stringify(data.user));
	};

	const logout = () => {
		// Call API to revoke session (cookie)
		fetch(`${env.C_STREAM_SERVER_URL}/v1/auth/logout`, {
			method: "POST",
			// usage of 'include' ensures cookies are sent
			credentials: "include",
		}).catch(console.error);

		setToken(null);
		setUser(null);

		localStorage.removeItem("token");
		localStorage.removeItem("user");
	};

	const isAuthenticated = !!token;

	return (
		<AuthContext.Provider
			value={{ user, token, isAuthenticated, login, logout }}
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
