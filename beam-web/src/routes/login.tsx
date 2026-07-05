import { createFileRoute } from "@tanstack/react-router";
import { LogIn } from "lucide-react";
import { Button } from "../components/ui/button";
import { useAuth } from "../hooks/auth";

export const Route = createFileRoute("/login")({
	validateSearch: (search: Record<string, unknown>): { redirect?: string } => ({
		redirect: typeof search.redirect === "string" ? search.redirect : undefined,
	}),
	component: LoginPage,
});

export function LoginPage() {
	const { login } = useAuth();
	const { redirect } = Route.useSearch();

	return (
		<div className="flex min-h-[calc(100vh-4rem)] items-center justify-center bg-gray-900 px-4 py-12 sm:px-6 lg:px-8">
			<div className="w-full max-w-md space-y-8 rounded-xl bg-gray-800 p-8 shadow-2xl border border-gray-700 text-center">
				<div>
					<h2 className="mt-6 text-center text-3xl font-extrabold text-white tracking-tight">
						Sign in to Beam
					</h2>
					<p className="mt-2 text-center text-sm text-gray-400">
						Beam uses single sign-on -- you'll be redirected to your identity
						provider to continue.
					</p>
				</div>
				<Button
					type="button"
					onClick={() => login(redirect)}
					className="group relative flex w-full justify-center bg-cyan-600 py-2 px-4 text-sm font-medium text-white hover:bg-cyan-700 focus:outline-none focus:ring-2 focus:ring-cyan-500 focus:ring-offset-2 focus:ring-offset-gray-900 transition-all"
				>
					<LogIn size={18} className="mr-2" />
					Sign in with SSO
				</Button>
			</div>
		</div>
	);
}
