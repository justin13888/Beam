import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { useId, useState } from "react";
import { apiClient } from "@/lib/apiClient";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { Label } from "../components/ui/label";
import { useAuth } from "../hooks/auth";

export const Route = createFileRoute("/login")({
	component: LoginPage,
});

function LoginPage() {
	const navigate = useNavigate();
	const { login } = useAuth();
	const [error, setError] = useState<string | null>(null);
	const [isLoading, setIsLoading] = useState(false);
	const usernameId = useId();
	const passwordId = useId();

	async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
		event.preventDefault();
		setError(null);
		setIsLoading(true);

		const formData = new FormData(event.currentTarget);
		const username_or_email = formData.get("username") as string;
		const password = formData.get("password") as string;

		try {
			const {
				data,
				error: apiError,
				response,
			} = await apiClient.POST("/v1/auth/login", {
				body: { username_or_email, password },
				credentials: "include",
			});

			if (!response.ok || !data) {
				throw new Error(apiError ? String(apiError) : "Login failed");
			}

			login(data);
			navigate({ to: "/" });
		} catch (err) {
			setError(
				err instanceof Error ? err.message : "An unknown error occurred",
			);
		} finally {
			setIsLoading(false);
		}
	}

	return (
		<div className="flex min-h-[calc(100vh-4rem)] items-center justify-center bg-gray-900 px-4 py-12 sm:px-6 lg:px-8">
			<div className="w-full max-w-md space-y-8 rounded-xl bg-gray-800 p-8 shadow-2xl border border-gray-700">
				<div>
					<h2 className="mt-6 text-center text-3xl font-extrabold text-white tracking-tight">
						Sign in to your account
					</h2>
					<p className="mt-2 text-center text-sm text-gray-400">
						Or{" "}
						<Link
							to="/register"
							className="font-medium text-cyan-500 hover:text-cyan-400 transition-colors"
						>
							create a new account
						</Link>
					</p>
				</div>
				<form className="mt-8 space-y-6" onSubmit={handleSubmit}>
					{error && (
						<div className="rounded-md bg-red-500/10 p-4 border border-red-500/20">
							<div className="text-sm text-red-400">{error}</div>
						</div>
					)}
					<div className="space-y-4 rounded-md shadow-sm">
						<div>
							<Label htmlFor={usernameId} className="text-gray-300">
								Username or Email
							</Label>
							<Input
								id={usernameId}
								name="username"
								type="text"
								autoComplete="username"
								required
								className="mt-1 bg-gray-900 border-gray-600 text-white placeholder-gray-500 focus:border-cyan-500 focus:ring-cyan-500"
								placeholder="Enter your username"
							/>
						</div>
						<div>
							<Label htmlFor={passwordId} className="text-gray-300">
								Password
							</Label>
							<Input
								id={passwordId}
								name="password"
								type="password"
								autoComplete="current-password"
								required
								className="mt-1 bg-gray-900 border-gray-600 text-white placeholder-gray-500 focus:border-cyan-500 focus:ring-cyan-500"
								placeholder="Enter your password"
							/>
						</div>
					</div>

					<div>
						<Button
							type="submit"
							disabled={isLoading}
							className="group relative flex w-full justify-center bg-cyan-600 py-2 px-4 text-sm font-medium text-white hover:bg-cyan-700 focus:outline-none focus:ring-2 focus:ring-cyan-500 focus:ring-offset-2 focus:ring-offset-gray-900 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
						>
							{isLoading ? "Signing in..." : "Sign in"}
						</Button>
					</div>
				</form>
			</div>
		</div>
	);
}
