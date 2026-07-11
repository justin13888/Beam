import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import {
	LogOut,
	MapPin,
	Monitor,
	RefreshCw,
	ShieldOff,
	User as UserIcon,
} from "lucide-react";
import { toast } from "sonner";
import type { components } from "@/api.gen";
import { Button } from "@/components/ui/button";
import { useAuth } from "@/hooks/auth";
import { apiClient } from "@/lib/apiClient";

type Session =
	components["schemas"]["beam_auth.server.oidc_routes.SessionSummary"];

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

/** `created_at`/`last_active` arrive as int64 Unix *seconds* from the backend. */
function formatDateTime(unixSeconds: number): string {
	return new Date(unixSeconds * 1000).toLocaleString(undefined, {
		dateStyle: "medium",
		timeStyle: "short",
	});
}

/** Short relative label for a Unix-seconds timestamp (falls back to a date). */
function formatRelative(unixSeconds: number): string {
	const diffSecs = Math.floor(Date.now() / 1000 - unixSeconds);
	if (diffSecs < 60) return "Just now";
	const diffMins = Math.floor(diffSecs / 60);
	if (diffMins < 60) return `${diffMins}m ago`;
	const diffHrs = Math.floor(diffMins / 60);
	if (diffHrs < 24) return `${diffHrs}h ago`;
	const diffDays = Math.floor(diffHrs / 24);
	if (diffDays < 30) return `${diffDays}d ago`;
	return new Date(unixSeconds * 1000).toLocaleDateString();
}

function SessionRow({
	session,
	onRevoke,
	revoking,
}: {
	session: Session;
	onRevoke: (id: string) => void;
	revoking: boolean;
}) {
	return (
		<li className="flex flex-col gap-3 rounded-lg bg-gray-900 border border-gray-700 p-4 sm:flex-row sm:items-center sm:justify-between">
			<div className="min-w-0 space-y-1">
				<div className="flex items-center gap-2 text-white">
					<Monitor size={16} className="text-cyan-400 shrink-0" />
					{/* device_hash is an opaque SHA-256 of the user-agent, not a
					    human-readable device name -- show a truncated fingerprint. */}
					<span
						className="font-mono text-sm truncate"
						title={session.device_hash}
					>
						{session.device_hash.slice(0, 12)}…
					</span>
				</div>
				<div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-gray-400">
					<span className="flex items-center gap-1.5">
						<MapPin size={12} className="text-gray-500" />
						{session.ip}
					</span>
					<span title={formatDateTime(session.created_at)}>
						Created {formatDateTime(session.created_at)}
					</span>
					<span title={formatDateTime(session.last_active)}>
						Last active {formatRelative(session.last_active)}
					</span>
				</div>
			</div>
			<Button
				variant="outline"
				onClick={() => onRevoke(session.id)}
				disabled={revoking}
				className="shrink-0 border-gray-600 text-gray-300 hover:bg-red-600/20 hover:text-red-400 hover:border-red-500/40"
			>
				{revoking ? <RefreshCw size={16} className="animate-spin" /> : "Revoke"}
			</Button>
		</li>
	);
}

export function ProfilePage() {
	const { user, isAuthenticated, logout, refresh } = useAuth();
	const navigate = useNavigate();
	const queryClient = useQueryClient();

	const {
		data,
		isLoading: sessionsLoading,
		error: sessionsError,
		refetch: refetchSessions,
	} = useQuery({
		queryKey: ["sessions"],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/sessions", {
				credentials: "include",
			});
			if (error) throw new Error("Failed to load active sessions");
			return data;
		},
		enabled: isAuthenticated,
	});

	const handleLogout = async () => {
		await logout();
		navigate({ to: "/login" });
	};

	const handleRevoke = async (id: string) => {
		try {
			const { error, response } = await apiClient.DELETE("/v1/sessions/{id}", {
				params: { path: { id } },
				credentials: "include",
			});
			if (error || !response.ok) throw new Error("Failed to revoke session");
			toast.success("Session revoked");
			await queryClient.invalidateQueries({ queryKey: ["sessions"] });
			// The current session is not identifiable from the client (the list
			// id is a row id, not the httpOnly session token), so a revoke may
			// have targeted this very session. Re-check /me; if the cookie is now
			// gone, drop back to the login flow rather than sitting half-signed-in.
			await refresh();
		} catch (err) {
			toast.error(
				`Failed to revoke session: ${err instanceof Error ? err.message : "unknown error"}`,
			);
		}
	};

	const handleSignOutAll = async () => {
		if (
			!confirm(
				"Sign out of every session on all devices? You will be signed out here too and will need to sign in again.",
			)
		) {
			return;
		}
		try {
			const { error, response } = await apiClient.POST("/v1/logout-all", {
				credentials: "include",
			});
			if (error || !response.ok) throw new Error("Failed to sign out");
			toast.success("Signed out of all sessions");
			// logout-all revokes *all* sessions including the current one and
			// clears the cookie, so route through the local logout flow to clear
			// auth state and land on /login.
			await logout();
			navigate({ to: "/login" });
		} catch (err) {
			toast.error(
				`Failed to sign out of all sessions: ${err instanceof Error ? err.message : "unknown error"}`,
			);
		}
	};

	const sessions = data ?? [];

	return (
		<div className="container mx-auto max-w-2xl py-12 px-4">
			<div className="rounded-xl bg-gray-800 p-8 shadow-2xl border border-gray-700">
				<div className="flex items-center space-x-4 mb-8">
					<div className="h-16 w-16 rounded-full bg-cyan-600 flex items-center justify-center">
						<UserIcon size={32} className="text-white" />
					</div>
					<div>
						<h1 className="text-2xl font-bold text-white">
							{user?.display_name}
						</h1>
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

			{/* Active sessions */}
			<div className="mt-8 rounded-xl bg-gray-800 p-8 shadow-2xl border border-gray-700">
				<div className="flex items-center justify-between mb-2">
					<h2 className="text-xl font-bold text-white">Active sessions</h2>
					<Button
						onClick={() => refetchSessions()}
						variant="outline"
						className="border-gray-700 text-gray-300 hover:bg-gray-700 hover:text-white"
					>
						<RefreshCw size={16} className="mr-2" />
						Refresh
					</Button>
				</div>
				<p className="text-sm text-gray-400 mb-6">
					Devices currently signed in to your account.
				</p>

				{sessionsLoading ? (
					<div className="flex items-center gap-3 text-gray-400 py-8">
						<RefreshCw className="animate-spin" size={18} />
						<span>Loading sessions...</span>
					</div>
				) : sessionsError ? (
					<div className="space-y-4 py-4">
						<p className="text-red-400">Error: {sessionsError.message}</p>
						<Button
							onClick={() => refetchSessions()}
							variant="outline"
							className="border-gray-600 text-gray-300 hover:bg-gray-700"
						>
							Retry
						</Button>
					</div>
				) : sessions.length === 0 ? (
					<p className="text-gray-500 py-4">No active sessions.</p>
				) : (
					<>
						<ul className="space-y-3">
							{sessions.map((session) => (
								<SessionRow
									key={session.id}
									session={session}
									onRevoke={handleRevoke}
									revoking={false}
								/>
							))}
						</ul>
						<div className="pt-6 mt-6 border-t border-gray-700">
							<Button
								variant="destructive"
								onClick={handleSignOutAll}
								className="w-full sm:w-auto"
							>
								<ShieldOff size={18} className="mr-2" />
								Sign out all sessions
							</Button>
							<p className="text-xs text-gray-500 mt-2">
								Signs out every device, including this one.
							</p>
						</div>
					</>
				)}
			</div>
		</div>
	);
}
