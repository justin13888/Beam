import { gql } from "@apollo/client";
import { useQuery } from "@apollo/client/react";
import { createFileRoute, redirect } from "@tanstack/react-router";
import {
	AlertCircle,
	AlertTriangle,
	Info,
	RefreshCw,
	Shield,
} from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { useAuth } from "../hooks/auth";

const GET_ADMIN_LOGS = gql`
  query GetAdminLogs($limit: Int, $offset: Int) {
    logs(limit: $limit, offset: $offset) {
      id
      level
      category
      message
      details
      createdAt
    }
    logCount
  }
`;

interface AdminLogEntry {
	id: string;
	level: "INFO" | "WARNING" | "ERROR";
	category: string;
	message: string;
	details: string | null;
	createdAt: string;
}

interface AdminQueryResult {
	logs: AdminLogEntry[];
	logCount: number;
}

const PAGE_SIZE = 50;

export const Route = createFileRoute("/admin")({
	beforeLoad: ({ context }) => {
		if (!context.auth.isAuthenticated) {
			throw redirect({ to: "/login" });
		}
		if (!context.auth.user?.is_admin) {
			throw redirect({ to: "/" });
		}
	},
	component: AdminPage,
});

function LevelBadge({ level }: { level: AdminLogEntry["level"] }) {
	if (level === "ERROR") {
		return (
			<span className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium bg-red-900/40 text-red-300 border border-red-700/50">
				<AlertCircle size={12} />
				Error
			</span>
		);
	}
	if (level === "WARNING") {
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

function AdminPage() {
	const { user } = useAuth();
	const [page, setPage] = useState(0);
	const offset = page * PAGE_SIZE;

	const { data, loading, error, refetch } = useQuery<AdminQueryResult>(
		GET_ADMIN_LOGS,
		{
			variables: { limit: PAGE_SIZE, offset },
			fetchPolicy: "network-only",
		},
	);

	const logs = data?.logs ?? [];
	const totalCount = data?.logCount ?? 0;
	const totalPages = Math.max(1, Math.ceil(totalCount / PAGE_SIZE));

	return (
		<div className="min-h-screen bg-gradient-to-br from-gray-950 via-gray-900 to-gray-950">
			<div className="max-w-6xl mx-auto px-6 py-12">
				<div className="flex items-center justify-between mb-8">
					<div className="flex items-center gap-3">
						<Shield className="text-cyan-400" size={28} />
						<div>
							<h1 className="text-3xl font-bold text-white">Admin Dashboard</h1>
							<p className="text-gray-400 text-sm mt-1">
								System logs and administrative events
							</p>
						</div>
					</div>
					<div className="flex items-center gap-3">
						<span className="text-sm text-gray-500">
							Logged in as{" "}
							<span className="text-gray-300 font-medium">
								{user?.username}
							</span>
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

				{/* Stats */}
				<div className="grid grid-cols-1 sm:grid-cols-3 gap-4 mb-8">
					<div className="rounded-xl bg-gray-800/40 border border-gray-700/50 p-5 text-center">
						<div className="text-3xl font-bold text-white">{totalCount}</div>
						<div className="text-sm text-gray-400 mt-1">Total Log Entries</div>
					</div>
					<div className="rounded-xl bg-gray-800/40 border border-gray-700/50 p-5 text-center">
						<div className="text-3xl font-bold text-red-400">
							{logs.filter((l) => l.level === "ERROR").length}
						</div>
						<div className="text-sm text-gray-400 mt-1">Errors (this page)</div>
					</div>
					<div className="rounded-xl bg-gray-800/40 border border-gray-700/50 p-5 text-center">
						<div className="text-3xl font-bold text-yellow-400">
							{logs.filter((l) => l.level === "WARNING").length}
						</div>
						<div className="text-sm text-gray-400 mt-1">
							Warnings (this page)
						</div>
					</div>
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

					{!loading && !error && logs.length === 0 && (
						<div className="px-6 py-12 text-center">
							<Info className="mx-auto text-gray-600 mb-3" size={32} />
							<p className="text-gray-500">No log entries yet.</p>
							<p className="text-gray-600 text-sm mt-1">
								Logs will appear here when administrative tasks run.
							</p>
						</div>
					)}

					{!loading && !error && logs.length > 0 && (
						<div className="divide-y divide-gray-700/30">
							{logs.map((log) => (
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
													{formatTimestamp(log.createdAt)}
												</span>
											</div>
											<p className="text-gray-200 text-sm">{log.message}</p>
											{log.details && (
												<details className="mt-2">
													<summary className="text-xs text-gray-500 cursor-pointer hover:text-gray-400">
														Details
													</summary>
													<pre className="mt-1 text-xs text-gray-400 bg-gray-900/50 rounded p-2 overflow-x-auto">
														{(() => {
															try {
																return JSON.stringify(
																	JSON.parse(log.details),
																	null,
																	2,
																);
															} catch {
																return log.details;
															}
														})()}
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
		</div>
	);
}
