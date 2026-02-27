import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { useId, useState } from "react";
import { apiClient } from "@/lib/apiClient";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { Label } from "../components/ui/label";
import { useAuth } from "../hooks/auth";

export const Route = createFileRoute("/register")({
	component: RegisterPage,
});

function RegisterPage() {
	const navigate = useNavigate();
	const { login } = useAuth();
	const [error, setError] = useState<string | null>(null);
	const [isLoading, setIsLoading] = useState(false);
	const usernameId = useId();
	const emailId = useId();
	const passwordId = useId();
	const confirmPasswordId = useId();

	async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
		event.preventDefault();
		setError(null);
		setIsLoading(true);

		const formData = new FormData(event.currentTarget);
		const username = formData.get("username") as string;
		const email = formData.get("email") as string;
		const password = formData.get("password") as string;
		const confirmPassword = formData.get("confirmPassword") as string;

		if (password !== confirmPassword) {
			setError("Passwords do not match");
			setIsLoading(false);
			return;
		}

		try {
			const {
				data,
				error: apiError,
				response,
			} = await apiClient.POST("/v1/auth/register", {
				body: { username, email, password },
				credentials: "include",
			});

			if (!response.ok || !data) {
				throw new Error(apiError ? String(apiError) : "Registration failed");
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
						Create your account
					</h2>
					<p className="mt-2 text-center text-sm text-gray-400">
						Or{" "}
						<Link
							to="/login"
							className="font-medium text-cyan-500 hover:text-cyan-400 transition-colors"
						>
							sign in to your account
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
								Username
							</Label>
							<Input
								id={usernameId}
								name="username"
								type="text"
								autoComplete="username"
								required
								className="mt-1 bg-gray-900 border-gray-600 text-white placeholder-gray-500 focus:border-cyan-500 focus:ring-cyan-500"
								placeholder="Choose a username"
							/>
						</div>
						<div>
							<Label htmlFor={emailId} className="text-gray-300">
								Email address
							</Label>
							<Input
								id={emailId}
								name="email"
								type="email"
								autoComplete="email"
								required
								className="mt-1 bg-gray-900 border-gray-600 text-white placeholder-gray-500 focus:border-cyan-500 focus:ring-cyan-500"
								placeholder="Enter your email"
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
								autoComplete="new-password"
								required
								className="mt-1 bg-gray-900 border-gray-600 text-white placeholder-gray-500 focus:border-cyan-500 focus:ring-cyan-500"
								placeholder="Choose a password"
							/>
						</div>
						<div>
							<Label htmlFor={confirmPasswordId} className="text-gray-300">
								Confirm Password
							</Label>
							<Input
								id={confirmPasswordId}
								name="confirmPassword"
								type="password"
								autoComplete="new-password"
								required
								className="mt-1 bg-gray-900 border-gray-600 text-white placeholder-gray-500 focus:border-cyan-500 focus:ring-cyan-500"
								placeholder="Confirm your password"
							/>
						</div>
					</div>

					<div>
						<Button
							type="submit"
							disabled={isLoading}
							className="group relative flex w-full justify-center bg-cyan-600 py-2 px-4 text-sm font-medium text-white hover:bg-cyan-700 focus:outline-none focus:ring-2 focus:ring-cyan-500 focus:ring-offset-2 focus:ring-offset-gray-900 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
						>
							{isLoading ? "Creating account..." : "Sign up"}
						</Button>
					</div>
				</form>
			</div>
		</div>
	);
}
