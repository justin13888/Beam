import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import {
	Activity,
	AlertCircle,
	AlertTriangle,
	Ban,
	CheckCircle2,
	Info,
	Radio,
	RefreshCw,
	ScrollText,
	Shield,
	Users as UsersIcon,
} from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import type { components } from "@/api.gen";
import { Button } from "@/components/ui/button";
import { apiClient } from "@/lib/apiClient";
import { RouteError } from "../components/RouteError";
import { useAuth } from "../hooks/auth";
import { useAdminEventStream } from "../hooks/useAdminEventStream";

type AdminLogEntry =
	components["schemas"]["beam_server.models.admin.AdminLogEntryDto"];
type AdminUser = components["schemas"]["beam_server.models.admin.AdminUserDto"];
type AdminStatus =
	components["schemas"]["beam_server.models.admin.AdminStatusResponse"];
type RecentScan =
	components["schemas"]["beam_server.models.admin.RecentScanDto"];

const PAGE_SIZE = 50;

/** Which admin tab is active. Kept in the URL (`?tab=`) so a view is
 * shareable/bookmarkable, matching the repo's URL-state idiom. */
export type AdminTab = "users" | "status" | "logs";

export interface AdminSearch {
	tab: AdminTab;
}

const ADMIN_TABS: readonly AdminTab[] = ["users", "status", "logs"];

/** Narrow navigation contract the page needs; satisfied by the router's
 * `useNavigate` and trivially fakeable in tests. */
export type AdminNavigate = (opts: {
	search: AdminSearch | ((prev: AdminSearch) => AdminSearch);
	replace?: boolean;
}) => void;

export const Route = createFileRoute("/admin")({
	validateSearch: (search: Record<string, unknown>): AdminSearch => ({
		tab: ADMIN_TABS.includes(search.tab as AdminTab)
			? (search.tab as AdminTab)
			: "logs",
	}),
	beforeLoad: ({ context, location }) => {
		if (!context.auth.isAuthenticated) {
			throw redirect({ to: "/login", search: { redirect: location.href } });
		}
		if (!context.auth.user?.is_admin) {
			throw redirect({ to: "/" });
		}
	},
	errorComponent: RouteError,
	component: RouteComponent,
});

function RouteComponent() {
	const { tab } = Route.useSearch();
	const navigate = useNavigate({ from: Route.fullPath });
	return (
		<AdminPage
			tab={tab}
			navigate={(opts) =>
				navigate({ search: opts.search, replace: opts.replace })
			}
		/>
	);
}

function LevelBadge({ level }: { level: AdminLogEntry["level"] }) {
	if (level === "error") {
		return (
			<span className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium bg-red-900/40 text-red-300 border border-red-700/50">
				<AlertCircle size={12} />
				Error
			</span>
		);
	}
	if (level === "warning") {
		return (
			<span className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium bg-yellow-900/40 text-yellow-300 border border-yellow-700/50">
				<AlertTriangle size={12} />
				Warning
			</span>
		);
	}
	return (
		<span className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium bg-cyan-900/40 text-cyan-300 border border-cyan-700/50">
			<Info size={12} />
			Info
		</span>
	);
}

function CategoryBadge({ category }: { category: string }) {
	const labels: Record<string, string> = {
		library_scan: "Library Scan",
		system: "System",
		auth: "Auth",
	};
	return (
		<span className="inline-block px-2 py-0.5 rounded text-xs text-gray-400 bg-gray-700/50 border border-gray-600/30">
			{labels[category] ?? category}
		</span>
	);
}

function formatTimestamp(iso: string): string {
	const date = new Date(iso);
	return date.toLocaleString(undefined, {
		dateStyle: "medium",
		timeStyle: "medium",
	});
}

function formatDate(iso: string): string {
	return new Date(iso).toLocaleDateString(undefined, { dateStyle: "medium" });
}

/** Whole-seconds uptime rendered as a compact "3d 4h 12m" string. Seconds
 * are only shown for sub-hour uptimes so long-running processes stay tidy. */
function humanizeUptime(totalSecs: number): string {
	const secs = Math.max(0, Math.floor(totalSecs));
	const days = Math.floor(secs / 86400);
	const hours = Math.floor((secs % 86400) / 3600);
	const minutes = Math.floor((secs % 3600) / 60);
	const seconds = secs % 60;
	const parts: string[] = [];
	if (days) parts.push(`${days}d`);
	if (hours) parts.push(`${hours}h`);
	if (minutes) parts.push(`${minutes}m`);
	if (!days && !hours) parts.push(`${seconds}s`);
	return parts.join(" ") || "0s";
}

/** Coarse "X ago" relative time for recent-scan timestamps. */
function formatRelativeTime(iso: string): string {
	const diffMs = Date.now() - new Date(iso).getTime();
	const secs = Math.floor(diffMs / 1000);
	if (secs < 60) return "just now";
	const mins = Math.floor(secs / 60);
	if (mins < 60) return `${mins}m ago`;
	const hrs = Math.floor(mins / 60);
	if (hrs < 24) return `${hrs}h ago`;
	const days = Math.floor(hrs / 24);
	return `${days}d ago`;
}

const cardClass = "rounded-xl bg-gray-800/40 border border-gray-700/50 p-5";

function TabBar({ tab, navigate }: { tab: AdminTab; navigate: AdminNavigate }) {
	const tabs: { id: AdminTab; label: string; icon: typeof UsersIcon }[] = [
		{ id: "users", label: "Users", icon: UsersIcon },
		{ id: "status", label: "Status", icon: Activity },
		{ id: "logs", label: "Logs", icon: ScrollText },
	];
	return (
		<div className="mb-8 flex overflow-hidden rounded-lg border border-gray-700 w-fit">
			{tabs.map(({ id, label, icon: Icon }) => (
				<button
					key={id}
					type="button"
					aria-current={tab === id ? "page" : undefined}
					onClick={() => navigate({ search: (prev) => ({ ...prev, tab: id }) })}
					className={`inline-flex items-center gap-2 px-4 py-2 text-sm font-medium transition-colors ${
						tab === id
							? "bg-cyan-600 text-white"
							: "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-white"
					}`}
				>
					<Icon size={15} />
					{label}
				</button>
			))}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Users tab
// ---------------------------------------------------------------------------

function UsersTab() {
	const { user: currentUser } = useAuth();
	const queryClient = useQueryClient();
	const [page, setPage] = useState(0);
	const offset = page * PAGE_SIZE;

	const {
		data,
		isLoading: loading,
		error,
	} = useQuery({
		queryKey: ["admin", "users", offset],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/admin/users", {
				params: { query: { limit: PAGE_SIZE, offset } },
				credentials: "include",
			});
			if (error) throw new Error("Failed to load users");
			return data;
		},
	});

	const toggleDisabledMutation = useMutation({
		mutationFn: async (vars: { id: string; disabled: boolean }) => {
			const { error, response } = await apiClient.PATCH(
				"/v1/admin/users/{id}",
				{
					params: { path: { id: vars.id } },
					body: { disabled: vars.disabled },
					credentials: "include",
				},
			);
			if (error || !response.ok) throw new Error("Failed to update user");
		},
	});

	const handleToggle = async (target: AdminUser) => {
		const nextDisabled = !target.disabled;
		if (
			nextDisabled &&
			!confirm(
				`Disable "${target.display_name}"? This immediately revokes their active sessions and blocks future logins.`,
			)
		) {
			return;
		}
		try {
			await toggleDisabledMutation.mutateAsync({
				id: target.id,
				disabled: nextDisabled,
			});
			queryClient.invalidateQueries({ queryKey: ["admin", "users"] });
			toast.success(
				nextDisabled
					? `Disabled "${target.display_name}"`
					: `Enabled "${target.display_name}"`,
			);
		} catch (err) {
			toast.error(
				`Failed to update "${target.display_name}": ${
					err instanceof Error ? err.message : "unknown error"
				}`,
			);
		}
	};

	const users = data?.items ?? [];
	const total = data?.total ?? 0;
	const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE));

	return (
		<div className="rounded-xl bg-gray-800/30 border border-gray-700/50 overflow-hidden">
			<div className="px-6 py-4 border-b border-gray-700/50 flex items-center justify-between">
				<h2 className="text-lg font-semibold text-white">Users</h2>
				<span className="text-sm text-gray-500">{total} total</span>
			</div>

			{error && (
				<div className="px-6 py-8 text-center">
					<AlertCircle className="mx-auto text-red-400 mb-3" size={32} />
					<p className="text-red-400 font-medium">Failed to load users</p>
					<p className="text-gray-500 text-sm mt-1">{error.message}</p>
				</div>
			)}

			{loading && (
				<div className="px-6 py-12 text-center text-gray-500">
					Loading users...
				</div>
			)}

			{!loading && !error && users.length === 0 && (
				<div className="px-6 py-12 text-center">
					<UsersIcon className="mx-auto text-gray-600 mb-3" size={32} />
					<p className="text-gray-500">No users found.</p>
				</div>
			)}

			{!loading && !error && users.length > 0 && (
				// A real list: each account is one item. Previously nested
				// `<div>`s, which gave assistive technology (and a test trying
				// to scope an assertion to one account) nothing to work with.
				<ul className="divide-y divide-gray-700/30">
					{users.map((u) => {
						const isSelf = u.id === currentUser?.id;
						return (
							<li
								key={u.id}
								className="px-6 py-4 flex items-center gap-4 hover:bg-gray-700/20 transition-colors"
							>
								{u.avatar_url ? (
									<img
										src={u.avatar_url}
										alt={u.display_name}
										className="h-9 w-9 rounded-full object-cover shrink-0"
									/>
								) : (
									<div className="h-9 w-9 rounded-full bg-gray-700 flex items-center justify-center text-sm text-gray-300 shrink-0">
										{u.display_name.charAt(0).toUpperCase()}
									</div>
								)}
								<div className="flex-1 min-w-0">
									<div className="flex items-center gap-2 flex-wrap">
										<span className="text-gray-200 font-medium truncate">
											{u.display_name}
										</span>
										{u.is_admin && (
											<span
												title="Admin rights are granted via your identity provider and cannot be changed here."
												className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium bg-cyan-900/40 text-cyan-300 border border-cyan-700/50"
											>
												<Shield size={11} />
												Admin
											</span>
										)}
										{u.disabled && (
											<span className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium bg-red-900/40 text-red-300 border border-red-700/50">
												<Ban size={11} />
												Disabled
											</span>
										)}
									</div>
									<div className="flex items-center gap-2 text-xs text-gray-500 mt-0.5">
										<span className="truncate">{u.email ?? "No email"}</span>
										<span>·</span>
										<span>Joined {formatDate(u.created_at)}</span>
									</div>
									{u.is_admin && (
										<p className="text-[11px] text-gray-600 mt-0.5">
											Admin via identity provider (read-only)
										</p>
									)}
								</div>
								<div className="shrink-0">
									{isSelf ? (
										<span className="text-xs text-gray-500 italic">You</span>
									) : (
										<Button
											variant="outline"
											size="sm"
											onClick={() => handleToggle(u)}
											disabled={toggleDisabledMutation.isPending}
											className={
												u.disabled
													? "border-gray-600 text-emerald-300 hover:bg-emerald-600/20 hover:text-emerald-200 hover:border-emerald-500/40"
													: "border-gray-600 text-red-300 hover:bg-red-600/20 hover:text-red-200 hover:border-red-500/40"
											}
										>
											{u.disabled ? (
												<>
													<CheckCircle2 size={14} className="mr-1.5" />
													Enable
												</>
											) : (
												<>
													<Ban size={14} className="mr-1.5" />
													Disable
												</>
											)}
										</Button>
									)}
								</div>
							</li>
						);
					})}
				</ul>
			)}

			{totalPages > 1 && (
				<div className="px-6 py-4 border-t border-gray-700/50 flex items-center justify-between">
					<Button
						variant="outline"
						size="sm"
						onClick={() => setPage((p) => Math.max(0, p - 1))}
						disabled={page === 0 || loading}
						className="border-gray-600 text-gray-300 hover:text-white hover:bg-gray-700"
					>
						Previous
					</Button>
					<span className="text-sm text-gray-500">
						{page + 1} / {totalPages}
					</span>
					<Button
						variant="outline"
						size="sm"
						onClick={() => setPage((p) => Math.min(totalPages - 1, p + 1))}
						disabled={page >= totalPages - 1 || loading}
						className="border-gray-600 text-gray-300 hover:text-white hover:bg-gray-700"
					>
						Next
					</Button>
				</div>
			)}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Status tab
// ---------------------------------------------------------------------------

function StatBlock({
	label,
	value,
}: {
	label: string;
	value: string | number;
}) {
	return (
		<div className={`${cardClass} text-center`}>
			<div className="text-3xl font-bold text-white">{value}</div>
			<div className="text-sm text-gray-400 mt-1">{label}</div>
		</div>
	);
}

function RecentScanRow({ scan }: { scan: RecentScan }) {
	return (
		<div className="px-6 py-3">
			<div className="flex items-center gap-2 mb-1 flex-wrap">
				<LevelBadge level={scan.level} />
				<span
					className="text-xs text-gray-500 ml-auto"
					title={formatTimestamp(scan.timestamp)}
				>
					{formatRelativeTime(scan.timestamp)}
				</span>
			</div>
			<p className="text-gray-200 text-sm">{scan.message}</p>
		</div>
	);
}

function StatusTab() {
	const {
		data,
		isLoading: loading,
		error,
		refetch,
		isFetching,
	} = useQuery({
		queryKey: ["admin", "status"],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/admin/status", {
				credentials: "include",
			});
			if (error) throw new Error("Failed to load system status");
			return data as AdminStatus;
		},
		refetchInterval: 30_000,
	});

	if (error) {
		return (
			<div className={`${cardClass} text-center`}>
				<AlertCircle className="mx-auto text-red-400 mb-3" size={32} />
				<p className="text-red-400 font-medium">Failed to load system status</p>
				<p className="text-gray-500 text-sm mt-1">{error.message}</p>
			</div>
		);
	}

	if (loading || !data) {
		return (
			<div className="px-6 py-12 text-center text-gray-500">
				Loading system status...
			</div>
		);
	}

	const { uptime_secs, version, counts, enrichment, recent_scans } = data;

	return (
		<div className="space-y-8">
			<div className="flex items-center justify-end">
				<Button
					variant="outline"
					size="sm"
					onClick={() => refetch()}
					disabled={isFetching}
					className="border-gray-600 text-gray-300 hover:text-white hover:bg-gray-700"
				>
					<RefreshCw
						size={14}
						className={`mr-2 ${isFetching ? "animate-spin" : ""}`}
					/>
					Refresh
				</Button>
			</div>

			<div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
				<div className={cardClass}>
					<div className="text-sm text-gray-400">Uptime</div>
					<div className="text-2xl font-bold text-white mt-1">
						{humanizeUptime(uptime_secs)}
					</div>
				</div>
				<div className={cardClass}>
					<div className="text-sm text-gray-400">Version</div>
					<div className="text-2xl font-bold text-white mt-1">v{version}</div>
				</div>
			</div>

			<div>
				<h2 className="text-lg font-semibold text-white mb-3">Counts</h2>
				<div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
					<StatBlock label="Users" value={counts.users} />
					<StatBlock label="Libraries" value={counts.libraries} />
					<StatBlock label="Files" value={counts.files} />
				</div>
			</div>

			<div>
				<h2 className="text-lg font-semibold text-white mb-3">
					Enrichment queue
				</h2>
				<div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
					<StatBlock label="Pending" value={enrichment.pending} />
					<StatBlock label="Enriched" value={enrichment.enriched} />
					<StatBlock label="Unmatched" value={enrichment.unmatched} />
					<StatBlock label="Failed" value={enrichment.failed} />
				</div>
			</div>

			<div className="rounded-xl bg-gray-800/30 border border-gray-700/50 overflow-hidden">
				<div className="px-6 py-4 border-b border-gray-700/50">
					<h2 className="text-lg font-semibold text-white">Recent scans</h2>
				</div>
				{recent_scans.length === 0 ? (
					<div className="px-6 py-8 text-center text-gray-500 text-sm">
						No recent scan activity.
					</div>
				) : (
					<div className="divide-y divide-gray-700/30 max-h-96 overflow-y-auto">
						{recent_scans.map((scan) => (
							<RecentScanRow
								key={`${scan.timestamp}-${scan.message}`}
								scan={scan}
							/>
						))}
					</div>
				)}
			</div>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Logs tab (the original admin dashboard content, unchanged in behavior)
// ---------------------------------------------------------------------------

function LogsTab() {
	const { isAuthenticated } = useAuth();
	const { events: liveEvents, connected: liveConnected } =
		useAdminEventStream(isAuthenticated);
	const [page, setPage] = useState(0);
	const offset = page * PAGE_SIZE;

	const {
		data: logs,
		isLoading: loading,
		error,
		refetch: refetchLogs,
	} = useQuery({
		queryKey: ["admin", "logs", offset],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/admin/logs", {
				params: { query: { limit: PAGE_SIZE, offset } },
				credentials: "include",
			});
			if (error) throw new Error("Failed to load admin logs");
			return data;
		},
		enabled: isAuthenticated,
	});

	const { data: countData, refetch: refetchCount } = useQuery({
		queryKey: ["admin", "logs", "count"],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/admin/logs/count", {
				credentials: "include",
			});
			if (error) throw new Error("Failed to load admin log count");
			return data;
		},
		enabled: isAuthenticated,
	});

	const refetch = () => {
		refetchLogs();
		refetchCount();
	};

	const totalCount = countData?.count ?? 0;
	const totalPages = Math.max(1, Math.ceil(totalCount / PAGE_SIZE));

	return (
		<div className="space-y-8">
			{/* Stats */}
			<div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
				<div className="rounded-xl bg-gray-800/40 border border-gray-700/50 p-5 text-center">
					<div className="text-3xl font-bold text-white">{totalCount}</div>
					<div className="text-sm text-gray-400 mt-1">Total Log Entries</div>
				</div>
				<div className="rounded-xl bg-gray-800/40 border border-gray-700/50 p-5 text-center">
					<div className="text-3xl font-bold text-red-400">
						{(logs ?? []).filter((l) => l.level === "error").length}
					</div>
					<div className="text-sm text-gray-400 mt-1">Errors (this page)</div>
				</div>
				<div className="rounded-xl bg-gray-800/40 border border-gray-700/50 p-5 text-center">
					<div className="text-3xl font-bold text-yellow-400">
						{(logs ?? []).filter((l) => l.level === "warning").length}
					</div>
					<div className="text-sm text-gray-400 mt-1">Warnings (this page)</div>
				</div>
			</div>

			{/* Live Activity (SSE) */}
			<div className="rounded-xl bg-gray-800/30 border border-gray-700/50 overflow-hidden">
				<div className="px-6 py-4 border-b border-gray-700/50 flex items-center justify-between">
					<h2 className="text-lg font-semibold text-white flex items-center gap-2">
						<Radio
							size={16}
							className={liveConnected ? "text-emerald-400" : "text-gray-600"}
						/>
						Live Activity
					</h2>
					<div className="flex items-center gap-3">
						<span className="text-xs text-gray-500">
							{liveConnected ? "Connected" : "Connecting…"}
						</span>
						<Button
							variant="outline"
							size="sm"
							onClick={() => refetch()}
							disabled={loading}
							className="border-gray-600 text-gray-300 hover:text-white hover:bg-gray-700"
						>
							<RefreshCw
								size={14}
								className={`mr-2 ${loading ? "animate-spin" : ""}`}
							/>
							Refresh
						</Button>
					</div>
				</div>
				{liveEvents.length === 0 ? (
					<div className="px-6 py-8 text-center text-gray-500 text-sm">
						Waiting for scans and other admin events…
					</div>
				) : (
					<div className="divide-y divide-gray-700/30 max-h-80 overflow-y-auto">
						{liveEvents.map((event) => (
							<div key={event.id} className="px-6 py-3">
								<div className="flex items-center gap-2 mb-1 flex-wrap">
									<LevelBadge level={event.level} />
									<CategoryBadge category={event.category} />
									{event.library_name && (
										<span className="text-xs text-gray-500">
											{event.library_name}
										</span>
									)}
									<span className="text-xs text-gray-500 ml-auto">
										{formatTimestamp(event.timestamp)}
									</span>
								</div>
								<p className="text-gray-200 text-sm">{event.message}</p>
							</div>
						))}
					</div>
				)}
			</div>

			{/* Logs Table */}
			<div className="rounded-xl bg-gray-800/30 border border-gray-700/50 overflow-hidden">
				<div className="px-6 py-4 border-b border-gray-700/50 flex items-center justify-between">
					<h2 className="text-lg font-semibold text-white">System Logs</h2>
					<span className="text-sm text-gray-500">
						Page {page + 1} of {totalPages} ({totalCount} total)
					</span>
				</div>

				{error && (
					<div className="px-6 py-8 text-center">
						<AlertCircle className="mx-auto text-red-400 mb-3" size={32} />
						<p className="text-red-400 font-medium">Failed to load logs</p>
						<p className="text-gray-500 text-sm mt-1">{error.message}</p>
					</div>
				)}

				{loading && (
					<div className="px-6 py-12 text-center text-gray-500">
						Loading logs...
					</div>
				)}

				{!loading && !error && (logs ?? []).length === 0 && (
					<div className="px-6 py-12 text-center">
						<Info className="mx-auto text-gray-600 mb-3" size={32} />
						<p className="text-gray-500">No log entries yet.</p>
						<p className="text-gray-600 text-sm mt-1">
							Logs will appear here when administrative tasks run.
						</p>
					</div>
				)}

				{!loading && !error && (logs ?? []).length > 0 && (
					<div className="divide-y divide-gray-700/30">
						{(logs ?? []).map((log) => (
							<div
								key={log.id}
								className="px-6 py-4 hover:bg-gray-700/20 transition-colors"
							>
								<div className="flex items-start gap-3">
									<div className="flex-1 min-w-0">
										<div className="flex items-center gap-2 mb-1 flex-wrap">
											<LevelBadge level={log.level} />
											<CategoryBadge category={log.category} />
											<span className="text-xs text-gray-500 ml-auto">
												{formatTimestamp(log.created_at)}
											</span>
										</div>
										<p className="text-gray-200 text-sm">{log.message}</p>
										{log.details != null && (
											<details className="mt-2">
												<summary className="text-xs text-gray-500 cursor-pointer hover:text-gray-400">
													Details
												</summary>
												<pre className="mt-1 text-xs text-gray-400 bg-gray-900/50 rounded p-2 overflow-x-auto">
													{JSON.stringify(log.details, null, 2)}
												</pre>
											</details>
										)}
									</div>
								</div>
							</div>
						))}
					</div>
				)}

				{/* Pagination */}
				{totalPages > 1 && (
					<div className="px-6 py-4 border-t border-gray-700/50 flex items-center justify-between">
						<Button
							variant="outline"
							size="sm"
							onClick={() => setPage((p) => Math.max(0, p - 1))}
							disabled={page === 0 || loading}
							className="border-gray-600 text-gray-300 hover:text-white hover:bg-gray-700"
						>
							Previous
						</Button>
						<span className="text-sm text-gray-500">
							{page + 1} / {totalPages}
						</span>
						<Button
							variant="outline"
							size="sm"
							onClick={() => setPage((p) => Math.min(totalPages - 1, p + 1))}
							disabled={page >= totalPages - 1 || loading}
							className="border-gray-600 text-gray-300 hover:text-white hover:bg-gray-700"
						>
							Next
						</Button>
					</div>
				)}
			</div>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Page shell
// ---------------------------------------------------------------------------

export function AdminPage({
	tab,
	navigate,
}: {
	tab: AdminTab;
	navigate: AdminNavigate;
}) {
	const { user } = useAuth();

	return (
		<div className="min-h-screen bg-gradient-to-br from-gray-950 via-gray-900 to-gray-950">
			<div className="max-w-6xl mx-auto px-6 py-12">
				<div className="flex items-center justify-between mb-8">
					<div className="flex items-center gap-3">
						<Shield className="text-cyan-400" size={28} />
						<div>
							<h1 className="text-3xl font-bold text-white">Admin Dashboard</h1>
							<p className="text-gray-400 text-sm mt-1">
								Users, system status, and administrative logs
							</p>
						</div>
					</div>
					<span className="text-sm text-gray-500">
						Logged in as{" "}
						<span className="text-gray-300 font-medium">
							{user?.display_name}
						</span>
					</span>
				</div>

				<TabBar tab={tab} navigate={navigate} />

				{tab === "users" && <UsersTab />}
				{tab === "status" && <StatusTab />}
				{tab === "logs" && <LogsTab />}
			</div>
		</div>
	);
}
