import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import { LogOut, User as UserIcon } from "lucide-react";
import { Button } from "../components/ui/button";
import { useAuth } from "../hooks/auth";

export const Route = createFileRoute("/profile")({
	beforeLoad: ({ context, location }) => {
		if (!context.auth.isAuthenticated) {
			throw redirect({
				to: "/login",
				search: {
					redirect: location.href,
				},
			});
		}
	},
	component: ProfilePage,
});

function ProfilePage() {
	const { user, logout } = useAuth();
	const navigate = useNavigate();

	const handleLogout = () => {
		logout();
		navigate({ to: "/login" });
	};

	return (
		<div className="container mx-auto max-w-2xl py-12 px-4">
			<div className="rounded-xl bg-gray-800 p-8 shadow-2xl border border-gray-700">
				<div className="flex items-center space-x-4 mb-8">
					<div className="h-16 w-16 rounded-full bg-cyan-600 flex items-center justify-center">
						<UserIcon size={32} className="text-white" />
					</div>
					<div>
						<h1 className="text-2xl font-bold text-white">{user?.username}</h1>
						<p className="text-gray-400">{user?.email}</p>
					</div>
				</div>

				<div className="space-y-6">
					<div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
						<div className="p-4 rounded-lg bg-gray-900 border border-gray-700">
							<p className="text-sm text-gray-400 mb-1">User ID</p>
							<p className="font-mono text-sm text-white truncate">
								{user?.id}
							</p>
						</div>
						{/* Add more stats or info here */}
					</div>

					<div className="pt-6 border-t border-gray-700">
						<Button
							variant="destructive"
							onClick={handleLogout}
							className="w-full sm:w-auto"
						>
							<LogOut size={18} className="mr-2" />
							Sign Out
						</Button>
					</div>
				</div>
			</div>
		</div>
	);
}
